//! TUI 状态投影：v2 核心不向消费端暴露内部 runtime，TUI 通过协议事件与
//! snapshot/messages 增量维护当前会话的展示状态（entries、thinking、工具卡片、
//! approval、todo、滚动与布局），并向核心上报所有变更操作。

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use protium_core::{
    commands::AgentMode,
    model::{
        AgentPhase, DisplayContent, DisplayEntry, DisplayKind, ModelPhase, ThinkingDisplay,
        ThinkingResult, TodoStatus, TodoTask, ToolDisplay, ToolDisplayStatus,
    },
    protocol::{ContextBudgetDto, Event, MessageDto},
    provider::{ToolCall, Usage},
    secrets,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::output::{CachedMarkdown, EdgeScroll, MessageLayout, OutputSelection};

/// A pending approval surfaced to the TUI. Only the consumer-facing id, the
/// tool call and the reason cross the boundary; the oneshot sender lives in
/// the core's runtime.
#[derive(Clone, Debug)]
pub struct ApprovalDisplay {
    pub approval_id: String,
    pub call: ToolCall,
    pub reason: String,
    pub source_session_id: Option<String>,
    pub source_title: Option<String>,
    pub created_at: Instant,
}

/// Outcome of applying one protocol event to the projection.
#[derive(Debug, Default)]
pub struct ProjectionOutcome {
    /// The session list (or the active session) may have changed; the caller
    /// should refresh the snapshot.
    pub sessions_dirty: bool,
    /// The visible state changed and the frame needs a repaint: an approval
    /// overlay was dismissed, or a streaming delta (reasoning/text) arrived.
    /// The caller coalesces high-frequency deltas before drawing.
    pub force_redraw: bool,
    /// A transcript-changing event arrived; the caller must refetch the
    /// message page to rebuild history from the database.
    pub transcript_dirty: bool,
}

#[derive(Debug, Default)]
pub(crate) struct LiveThinkingLayoutCache {
    pub width: usize,
    pub source_start: usize,
    pub processed_len: usize,
    pub buffer_epoch: u64,
    pub rows: Vec<String>,
    pub current_row: String,
    pub current_width: usize,
    #[cfg(test)]
    pub full_rebuilds: usize,
    #[cfg(test)]
    pub processed_bytes: usize,
}

impl LiveThinkingLayoutCache {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Display state of one session, maintained by the TUI from v2 events and
/// message pages. The projection deliberately mirrors the display-facing
/// surface the renderer used to read off `SessionRuntime`, so `ui.rs` keeps
/// reading compatible fields; core-owned state (conversation, runners,
/// approvals, tool registry) never appears here.
#[derive(Debug)]
pub struct TuiSessionProjection {
    pub session_id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub entries: Vec<DisplayEntry>,
    pub todos: Vec<TodoTask>,
    pub todo_collapsed: bool,
    pub todo_hidden: bool,
    pub busy: bool,
    pub agent_phase: AgentPhase,
    pub model_phase: ModelPhase,
    pub status: String,
    pub mode: AgentMode,
    pub child_role: Option<String>,
    pub thinking_active: bool,
    pub thinking_last_line: String,
    pub thinking_buffer: String,
    pub thinking_buffer_truncated: bool,
    pub thinking_buffer_epoch: u64,
    pub thinking_result: ThinkingResult,
    pub thinking_animation_frame: usize,
    pub thinking_anchor: Option<usize>,
    pub thinking_expanded: bool,
    /// Active "generating tool call" live row: the localized display name and
    /// the cumulative argument bytes received so far. `Some` only while the
    /// model is streaming a tool call's arguments; any next-phase event clears
    /// it. Rendered as a collapsed, non-clickable animated row.
    pub generating_tool: Option<(String, u64)>,
    pub(crate) live_thinking_layout_cache: LiveThinkingLayoutCache,
    pub pending_approval: Option<ApprovalDisplay>,
    pub usage: Usage,
    /// Core-authoritative context usage from the last `ContextUpdated` (or
    /// snapshot). `Usage` events never mutate this.
    pub context_used_tokens: u64,
    /// TUI-only bounded overlay for tokens streamed since the last
    /// `ContextUpdated`; display-only, never used for request trimming.
    pub context_overlay_tokens: u64,
    pub context_limit_tokens: Option<u64>,
    /// The core-computed context budget for this session; the authority for
    /// the safe-input budget the meter shows.
    pub context_budget: Option<ContextBudgetDto>,
    pub expanded_tools: HashSet<String>,
    pub expanded_thinking: HashSet<String>,
    pub message_scroll: usize,
    pub follow_output: bool,
    pub output_scroll_top: Option<usize>,
    pub output_selection: Option<OutputSelection>,
    pub message_layout: Option<MessageLayout>,
    pub markdown_render_cache: HashMap<usize, CachedMarkdown>,
    pub output_layout_dirty: bool,
    /// Whether older messages exist beyond the loaded history head (from the
    /// last message page's `has_more`), and the opaque cursor to fetch them.
    pub history_has_more: bool,
    pub history_next_before: Option<i64>,
    #[cfg(test)]
    pub output_layout_rebuild_count: usize,
    #[cfg(test)]
    pub markdown_parse_count: usize,
    #[cfg(test)]
    pub footer_rebuild_count: usize,
    pub edge_scroll: EdgeScroll,
}

pub(crate) const MAX_THINKING_LINE_BYTES: usize = 1024;
pub(crate) const MAX_THINKING_BUFFER_BYTES: usize = 64 * 1024;

impl TuiSessionProjection {
    /// A fresh, empty projection for a session (used before the first message
    /// page arrives).
    pub fn new(session_id: String, mode: AgentMode, context_limit_tokens: Option<u64>) -> Self {
        Self {
            session_id,
            title: String::new(),
            parent_id: None,
            entries: Vec::new(),
            todos: Vec::new(),
            todo_collapsed: false,
            todo_hidden: false,
            busy: false,
            agent_phase: AgentPhase::Idle,
            model_phase: ModelPhase::Idle,
            status: String::new(),
            mode,
            child_role: None,
            thinking_active: false,
            thinking_last_line: String::new(),
            thinking_buffer: String::new(),
            thinking_buffer_truncated: false,
            thinking_buffer_epoch: 0,
            thinking_result: ThinkingResult::Completed,
            thinking_animation_frame: 0,
            thinking_anchor: None,
            thinking_expanded: false,
            generating_tool: None,
            live_thinking_layout_cache: LiveThinkingLayoutCache::default(),
            pending_approval: None,
            usage: Usage::default(),
            context_used_tokens: 0,
            context_overlay_tokens: 0,
            context_limit_tokens,
            context_budget: None,
            expanded_tools: HashSet::new(),
            expanded_thinking: HashSet::new(),
            message_scroll: 0,
            follow_output: true,
            output_scroll_top: None,
            output_selection: None,
            message_layout: None,
            markdown_render_cache: HashMap::new(),
            output_layout_dirty: true,
            history_has_more: false,
            history_next_before: None,
            #[cfg(test)]
            output_layout_rebuild_count: 0,
            #[cfg(test)]
            markdown_parse_count: 0,
            #[cfg(test)]
            footer_rebuild_count: 0,
            edge_scroll: EdgeScroll::default(),
        }
    }

    pub fn set_todos(&mut self, tasks: Vec<TodoTask>) {
        self.todo_hidden = false;
        self.todo_collapsed =
            !tasks.is_empty() && tasks.iter().all(|task| task.status == TodoStatus::Done);
        self.todos = tasks;
    }

    /// Applies a routed protocol event to the projection. Returns what the
    /// caller (the facade) should do next.
    pub fn push_entry(&mut self, entry: DisplayEntry) {
        self.clear_output_selection();
        self.invalidate_output_layout();
        self.entries.push(entry);
    }

    pub fn clear_output_selection(&mut self) {
        self.output_selection = None;
        self.edge_scroll = EdgeScroll::default();
    }

    /// Marks the trailing live assistant entry as incomplete when a stream was
    /// interrupted before its answer was persisted.
    fn mark_partial_if_streaming(&mut self) {
        if let Some(entry) = self.entries.last_mut()
            && matches!(entry.kind, DisplayKind::Assistant)
            && matches!(&entry.content, DisplayContent::Markdown(t) if !t.trim().is_empty())
        {
            entry.kind = DisplayKind::AssistantPartial;
        }
    }

    pub fn invalidate_output_layout(&mut self) {
        self.message_layout.take();
        self.output_layout_dirty = true;
    }

    pub fn trim_entries(&mut self) {
        const MAX_ENTRIES: usize = 1000;
        const MAX_BYTES: usize = 2 * 1024 * 1024;
        if self.entries.len() <= MAX_ENTRIES && display_entry_bytes(&self.entries) <= MAX_BYTES {
            return;
        }
        self.invalidate_output_layout();
        let removed = trim_entries(&mut self.entries);
        self.markdown_render_cache.clear();
        self.thinking_anchor = self
            .thinking_anchor
            .map(|anchor| anchor.saturating_sub(removed));
        self.clear_output_selection();
    }

    pub fn scroll_messages(&mut self, delta: isize) -> bool {
        let previous = (
            self.message_scroll,
            self.follow_output,
            self.output_scroll_top,
        );
        let Some(layout) = &self.message_layout else {
            if delta > 0 {
                self.message_scroll = self.message_scroll.saturating_add(delta as usize);
                self.follow_output = false;
            } else {
                self.message_scroll = self.message_scroll.saturating_sub(delta.unsigned_abs());
                if self.message_scroll == 0 {
                    self.follow_output = true;
                }
            }
            return previous
                != (
                    self.message_scroll,
                    self.follow_output,
                    self.output_scroll_top,
                );
        };
        let max_scroll = layout.max_scroll();
        let current = self
            .output_scroll_top
            .unwrap_or(layout.scroll)
            .min(max_scroll);
        let next = next_output_scroll_top(current, max_scroll, delta);
        if delta < 0 && next == max_scroll {
            self.output_scroll_top = None;
            self.follow_output = true;
            self.message_scroll = 0;
        } else {
            self.output_scroll_top = Some(next);
            self.follow_output = false;
            self.message_scroll = max_scroll.saturating_sub(next);
        }
        previous
            != (
                self.message_scroll,
                self.follow_output,
                self.output_scroll_top,
            )
    }

    pub fn scroll_to_bottom(&mut self) {
        self.message_scroll = 0;
        self.follow_output = true;
        self.output_scroll_top = None;
    }
}

fn status_from_wire(status: &str) -> ToolDisplayStatus {
    match status {
        "running" => ToolDisplayStatus::Running,
        "failed" => ToolDisplayStatus::Failed,
        "rejected" => ToolDisplayStatus::Rejected,
        _ => ToolDisplayStatus::Completed,
    }
}

/// Human-readable byte count for the "generating tool call" row (e.g. "9.0 KB").
pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Localized display name for a tool, used in status lines.
pub fn tool_display_name(name: &str) -> String {
    let translated = match name {
        "file_list" => Some("文件列表"),
        "file_stat" => Some("文件信息"),
        "file_read" => Some("文件读取"),
        "file_search" => Some("文件搜索"),
        "file_glob" => Some("文件查找"),
        "repo_map" => Some("符号大纲"),
        "file_mkdir" => Some("新建目录"),
        "file_write" => Some("文件修改"),
        "file_edit" => Some("文件编辑"),
        "file_copy" => Some("文件复制"),
        "file_move" => Some("文件移动"),
        "file_delete" => Some("文件删除"),
        "web_search" => Some("网络搜索"),
        "web_fetch" | "webfetch" => Some("网页读取"),
        "market_quote" => Some("实时行情"),
        "terminal_exec" => Some("命令执行"),
        "terminal_shell" => Some("Shell 命令"),
        "agent_spawn" => Some("子 Agent"),
        "git" => Some("Git 操作"),
        "git_diff" => Some("差异查看"),
        "browser_open" => Some("打开网页"),
        "browser_snapshot" => Some("页面快照"),
        "browser_click" => Some("页面点击"),
        "browser_type" => Some("页面输入"),
        "browser_press" => Some("页面按键"),
        _ => None,
    };
    if let Some(translated) = translated {
        return translated.to_owned();
    }
    if let Some(external) = name.strip_prefix("mcp:") {
        let tool = external.rsplit([':', '/']).next().unwrap_or(external);
        return format!("外部工具：{}", tool.replace('_', " "));
    }
    name.replace('_', " ")
}

pub(crate) fn tool_result_status(result: &str) -> ToolDisplayStatus {
    let lower = result.to_ascii_lowercase();
    if lower.starts_with("rejected by user") || lower.starts_with("denied by policy") {
        ToolDisplayStatus::Rejected
    } else if lower.starts_with("tool failed")
        || lower.starts_with("security policy denied")
        || lower.starts_with("process timed out")
        || lower.starts_with("duplicate tool call")
    {
        ToolDisplayStatus::Failed
    } else {
        ToolDisplayStatus::Completed
    }
}

fn utf8_tail(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let minimum = value.len().saturating_sub(max_bytes);
    let start = value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .find(|offset| *offset >= minimum)
        .unwrap_or(value.len());
    &value[start..]
}

fn display_entry_bytes(entries: &[DisplayEntry]) -> usize {
    entries
        .iter()
        .map(|entry| match &entry.content {
            DisplayContent::Markdown(text) | DisplayContent::Diff(text) => text.len() + 32,
            DisplayContent::Tool(tool) => {
                tool.call_id.len() + tool.name.len() + tool.arguments.to_string().len() + 64
            }
            DisplayContent::Thinking(thinking) => thinking.content.len() + 32,
        })
        .sum()
}

pub(crate) fn trim_entries(entries: &mut Vec<DisplayEntry>) -> usize {
    const MAX_ENTRIES: usize = 1000;
    const MAX_BYTES: usize = 2 * 1024 * 1024;
    let mut removed = 0;
    while entries.len() > MAX_ENTRIES || display_entry_bytes(entries) > MAX_BYTES {
        if entries.len() > MAX_ENTRIES {
            let count = entries.len() - MAX_ENTRIES;
            entries.drain(..count);
            removed += count;
        } else {
            entries.remove(0);
            removed += 1;
        }
    }
    removed
}

fn trim_entries_from_end(entries: &mut Vec<DisplayEntry>) -> usize {
    const MAX_ENTRIES: usize = 1000;
    const MAX_BYTES: usize = 2 * 1024 * 1024;
    let mut removed = 0;
    while entries.len() > MAX_ENTRIES || display_entry_bytes(entries) > MAX_BYTES {
        if entries.is_empty() {
            break;
        }
        entries.pop();
        removed += 1;
    }
    removed
}

pub(crate) fn next_output_scroll_top(current: usize, max_scroll: usize, delta: isize) -> usize {
    let current = current.min(max_scroll);
    if delta > 0 {
        current.saturating_sub(delta as usize)
    } else {
        current.saturating_add(delta.unsigned_abs()).min(max_scroll)
    }
}

mod events;
mod history;

#[cfg(test)]
mod tests;
