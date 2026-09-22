use std::{
    collections::{HashMap, HashSet},
    future::pending,
    io,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::Print,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ignore::WalkBuilder;
use protium_core::{
    agent::ChildSessionProgress,
    commands::{self, AgentMode, Command, TodoCommand},
    config::{
        Config, ProviderKind, ProviderPreset, ThinkingLevel, ThinkingProfile, ThinkingProfileKind,
        thinking_profile,
    },
    protocol::{
        AppSnapshotV2, ApprovalDto, Envelope, Event as ProtocolEvent, ProviderModelDto,
        ProviderModelsDto, ProviderProfileDto, ProviderSettingsDto,
    },
    secrets,
    security::Workspace,
    service::{AppHandle, AppService, CoreConfig},
    settings::{FIELDS, SettingsField, SettingsForm, SettingsState},
    storage::SessionSummary,
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use crate::{
    home::{self, HomeAction, HomeSelection, HomeState, RECENT_SESSION_LIMIT},
    input::InputBuffer,
    output::{EdgeScroll, InteractionTarget, OutputSelection},
    projection::{ApprovalDisplay, TuiSessionProjection},
    ui,
};

#[path = "app/commands.rs"]
mod command_ops;
use command_ops::{
    apply_file_completion, handle_palette_key, next_mode, open_palette, session_switch_direction,
    todo_status_from_wire, update_file_suggestions,
};
#[cfg(test)]
use command_ops::{command_to_text, todo_to_text};

mod event_loop;
mod input;
mod provider;
pub use event_loop::run;
#[cfg(test)]
use event_loop::{build_app, should_coalesce_stream_redraw};
use event_loop::{parse_phase, session_summary};
use input::handle_terminal_event;
use provider::{
    apply_model_refresh_result, apply_thinking_selection, handle_model_menu_key,
    handle_model_mouse, handle_provider_menu_key, handle_provider_mouse, load_provider_models,
    model_menu_key_handled, point_in_rect, provider_menu_key_handled, refresh_provider_settings,
    thinking_menu_selection,
};
pub(crate) use provider::{model_choices, provider_choices};

const MOUSE_WHEEL_SCROLL_LINES: isize = 1;
const DEFERRED_REDRAW_INTERVAL: Duration = Duration::from_millis(16);

pub use protium_core::model::{
    AgentPhase, DisplayContent, DisplayEntry, DisplayKind, ModelPhase, PendingApproval,
    ThinkingDisplay, ThinkingResult, TodoDisplay, TodoStatus, TodoTask, ToolDisplay,
    ToolDisplayStatus,
};

/// How the user answered a pending approval prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalChoice {
    Approve,
    Reject,
    AlwaysSession,
}

#[derive(Clone, Debug)]
pub struct CommandPaletteState {
    pub query: String,
    pub selected: usize,
}

/// Outcome of handling a terminal event: whether to redraw and whether an
/// OSC 52 clipboard sequence should be emitted.
struct EventOutcome {
    redraw: bool,
    osc52: Option<String>,
}

impl EventOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            osc52: None,
        }
    }

    fn default() -> Self {
        Self {
            redraw: false,
            osc52: None,
        }
    }
}

/// TUI cache of the core's dynamic provider model list. The core remains the
/// source of truth; this only mirrors the last `provider_models` answer for the
/// active preset so menus can render without another round trip.
#[derive(Clone, Debug, Default)]
pub struct ProviderModelsState {
    pub preset: Option<ProviderPreset>,
    pub models: Vec<ProviderModelDto>,
    pub fetched_at: Option<i64>,
    pub loading: bool,
    pub last_error: Option<String>,
}

/// Result of a background `provider_models(true)` refresh.
#[derive(Debug)]
pub(crate) struct ModelRefreshResult {
    pub generation: u64,
    pub preset: ProviderPreset,
    pub result: std::result::Result<ProviderModelsDto, String>,
}

/// One model picker entry: the stable id plus its display label. Deduping and
/// selection always key off `id`, never the decorated label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    pub id: String,
    pub label: String,
}

/// TUI facade. Owns the terminal shell, the display projection of the active
/// session, and every TUI-only interaction state (menus, scroll, layout,
/// mouse). All mutation is delegated to [`AppHandle`]; this struct never
/// touches the core's internal runtime, storage, registry or approvals.
pub struct App {
    pub handle: AppHandle,
    pub workspace: PathBuf,
    pub workspace_security: Workspace,
    /// Shared configuration for display and the settings page. The core owns
    /// the authoritative copy; every mutation goes through the handle.
    pub config: Config,
    /// Core-authoritative provider settings view: active profile, saved
    /// profiles and the presets with a currently resolvable API key.
    pub provider_settings: Option<ProviderSettingsDto>,
    /// Last `provider_models(false)` answer for the active preset.
    pub provider_models: ProviderModelsState,
    /// Background model-refresh result channel (sender side).
    pub(crate) model_refresh_tx: tokio::sync::mpsc::Sender<ModelRefreshResult>,
    /// Background model-refresh result channel (receiver side; taken by the
    /// event loop so its `select!` branch never borrows the whole facade).
    pub(crate) model_refresh_rx: Option<tokio::sync::mpsc::Receiver<ModelRefreshResult>>,
    /// At most one provider model refresh may be in flight. The handle also
    /// gives provider changes and shutdown an explicit cancellation path.
    model_refresh_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonic identity for refresh requests; results from older provider
    /// state are ignored even if cancellation races with completion.
    model_refresh_generation: u64,
    /// TUI-only settings selection: the core `FIELDS` rows followed by the
    /// synthetic context-window override row.
    pub settings_field_index: usize,
    /// Write-only context-window override buffer. Empty means "inherit the
    /// merged profile value"; digits are passed to the core, which clamps.
    pub context_window_input: String,
    pub input: InputBuffer,
    pub context_meter_enabled: bool,
    pub settings: Option<SettingsState>,
    pub settings_rect: Option<Rect>,
    pub palette: Option<CommandPaletteState>,
    pub thinking_menu_open: bool,
    pub thinking_control_rect: Option<Rect>,
    pub thinking_menu_rect: Option<Rect>,
    pub session_panel_rect: Option<Rect>,
    pub input_mode_rect: Option<Rect>,
    pub provider_control_rect: Option<Rect>,
    pub model_control_rect: Option<Rect>,
    pub provider_menu_open: bool,
    pub provider_menu_rect: Option<Rect>,
    pub provider_menu_selected: usize,
    pub model_menu_open: bool,
    pub model_menu_rect: Option<Rect>,
    pub model_menu_selected: usize,
    pub todo_window_rect: Option<Rect>,
    pub force_full_redraw: bool,
    pub mouse_press_target: Option<InteractionTarget>,
    pub mouse_press_position: Option<(u16, u16)>,
    pub mouse_dragged: bool,
    pub layout_restore_anchor: Option<(InteractionTarget, usize)>,
    pub file_suggestions: Vec<String>,
    pub file_selected: usize,
    pub sessions: Vec<SessionSummary>,
    pub expanded_sessions: HashSet<String>,
    pub child_status: HashMap<String, ChildSessionProgress>,
    pub child_batches: HashMap<String, HashSet<String>>,
    pub active_session: String,
    /// Display projection of the active session.
    pub current: TuiSessionProjection,
    /// The globally oldest pending approval surfaced by the snapshot.
    pub approval: Option<ApprovalDisplay>,
    /// Live event stream cursor; events after it are replayed then live.
    pub event_cursor: u64,
    pub should_quit: bool,
    /// True while a snapshot/message refetch is pending; suppresses extra
    /// refetches from bursty events.
    pub sync_pending: bool,
    /// The request sequence of the last submit the facade started, per session.
    /// Passed to `cancel` so a stale cancel never aborts a newer request.
    pub request_ids: HashMap<String, u64>,
    /// Set when the user scrolled to the top of the loaded history and older
    /// messages exist; the event loop drains it into `load_older_history`.
    pub history_load_pending: bool,
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(task) = self.model_refresh_task.take() {
            task.abort();
        }
    }
}

async fn handle_navigation_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || app.settings.is_some()
        || app.palette.is_some()
        || app.has_pending_approval()
    {
        return Ok(None);
    }
    if let Some(area) = app.session_panel_rect {
        let rows = ui::flatten_session_tree(&app.sessions, &app.expanded_sessions);
        let current = rows
            .iter()
            .position(|row| row.id == app.current.session_id)
            .unwrap_or(0);
        if let Some(index) =
            ui::session_index_at(area, mouse.column, mouse.row, rows.len(), current)
        {
            let row = &rows[index];
            if row.has_children {
                if !app.expanded_sessions.insert(row.id.clone()) {
                    app.expanded_sessions.remove(&row.id);
                }
                return Ok(Some(EventOutcome::redraw()));
            }
            if row.id == app.current.session_id {
                return Ok(Some(EventOutcome::redraw()));
            }
            app.activate_session(&row.id).await?;
            return Ok(Some(EventOutcome::redraw()));
        }
    }
    if let Some(rect) = app.input_mode_rect
        && point_in_rect(mouse.column, mouse.row, rect)
    {
        if app.current.busy {
            app.current.status = "请求运行中，无法切换模式".into();
            return Ok(Some(EventOutcome::redraw()));
        }
        app.switch_mode(next_mode(app.current.mode)).await?;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

fn todo_interaction_at(app: &App, column: u16, row: u16) -> Option<InteractionTarget> {
    let rect = app.todo_window_rect?;
    if row == rect.y
        && let Some((toggle_column, close_column)) = ui::todo_control_columns(rect)
    {
        if column == toggle_column {
            return Some(InteractionTarget::TodoToggle);
        }
        if column == close_column {
            return Some(InteractionTarget::TodoClose);
        }
    }
    if row <= rect.y || row >= rect.bottom() || column != rect.x + 1 {
        return None;
    }
    let content_row = usize::from(row - rect.y - 1);
    let visible_rows = usize::from(rect.height.saturating_sub(2));
    let index = ui::todo_task_index_at_row(
        &app.current.todos,
        visible_rows,
        content_row,
        app.current.todo_collapsed,
    )?;
    app.current
        .todos
        .get(index)
        .map(|task| InteractionTarget::Todo(task.id.clone()))
}

fn handle_output_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> EventOutcome {
    match mouse.kind {
        MouseEventKind::ScrollUp => EventOutcome {
            redraw: app.current.scroll_messages(MOUSE_WHEEL_SCROLL_LINES),
            osc52: None,
        },
        MouseEventKind::ScrollDown => EventOutcome {
            redraw: app.current.scroll_messages(-MOUSE_WHEEL_SCROLL_LINES),
            osc52: None,
        },
        MouseEventKind::Down(MouseButton::Left) => {
            app.mouse_dragged = false;
            app.mouse_press_position = Some((mouse.column, mouse.row));
            app.mouse_press_target =
                todo_interaction_at(app, mouse.column, mouse.row).or_else(|| {
                    app.current
                        .message_layout
                        .as_ref()
                        .and_then(|layout| layout.interaction_at(mouse.column, mouse.row))
                });
            if app.mouse_press_target.is_some() {
                app.current.clear_output_selection();
                app.current.edge_scroll = EdgeScroll::default();
                return EventOutcome::redraw();
            }
            let Some(offset) = app
                .current
                .message_layout
                .as_ref()
                .and_then(|layout| layout.hit_test(mouse.column, mouse.row))
            else {
                app.current.clear_output_selection();
                return EventOutcome::redraw();
            };
            if app.current.follow_output {
                app.current.output_scroll_top = app
                    .current
                    .message_layout
                    .as_ref()
                    .map(|layout| layout.scroll);
                if let Some(layout) = &app.current.message_layout {
                    app.current.message_scroll = layout.max_scroll().saturating_sub(layout.scroll);
                }
            }
            app.current.follow_output = false;
            app.current.output_selection = Some(OutputSelection::new(offset));
            update_edge_scroll(app, mouse.column, mouse.row);
            EventOutcome::redraw()
        }
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
            if app.mouse_press_target.is_some() {
                let moved = matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left))
                    || app.mouse_press_position.is_some_and(|(column, row)| {
                        column.abs_diff(mouse.column) > 1 || row.abs_diff(mouse.row) > 1
                    });
                if moved {
                    app.mouse_dragged = true;
                    app.mouse_press_target = None;
                    if let Some((column, row)) = app.mouse_press_position
                        && let Some(offset) = app
                            .current
                            .message_layout
                            .as_ref()
                            .and_then(|layout| layout.hit_test(column, row))
                    {
                        app.current.output_selection = Some(OutputSelection::new(offset));
                    }
                    update_drag_position(app, mouse.column, mouse.row);
                    return EventOutcome::redraw();
                }
                return EventOutcome::default();
            }
            if app
                .current
                .output_selection
                .is_some_and(|selection| selection.dragging)
            {
                update_drag_position(app, mouse.column, mouse.row);
                EventOutcome::redraw()
            } else {
                EventOutcome::default()
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.current.edge_scroll = EdgeScroll::default();
            let pressed_target = app.mouse_press_target.take();
            app.mouse_press_position = None;
            if let Some(target) = pressed_target {
                let released_target =
                    todo_interaction_at(app, mouse.column, mouse.row).or_else(|| {
                        app.current
                            .message_layout
                            .as_ref()
                            .and_then(|layout| layout.interaction_at(mouse.column, mouse.row))
                    });
                if !app.mouse_dragged && released_target.as_ref() == Some(&target) {
                    let todo_target = matches!(
                        &target,
                        InteractionTarget::Todo(_)
                            | InteractionTarget::TodoToggle
                            | InteractionTarget::TodoClose
                    );
                    if !app.current.follow_output && !todo_target {
                        app.layout_restore_anchor =
                            app.current.message_layout.as_ref().and_then(|layout| {
                                layout
                                    .visual_lines
                                    .iter()
                                    .position(|line| line.interaction.as_ref() == Some(&target))
                                    .map(|visual_row| {
                                        (target.clone(), visual_row.saturating_sub(layout.scroll))
                                    })
                            });
                    }
                    let live_thinking_target = matches!(&target, InteractionTarget::Thinking);
                    match target {
                        InteractionTarget::Tool(call_id) => {
                            if !app.current.expanded_tools.insert(call_id.clone()) {
                                app.current.expanded_tools.remove(&call_id);
                            }
                        }
                        InteractionTarget::Thinking => {
                            app.current.thinking_expanded = !app.current.thinking_expanded;
                        }
                        InteractionTarget::ThinkingSummary(id) => {
                            if !app.current.expanded_thinking.insert(id.clone()) {
                                app.current.expanded_thinking.remove(&id);
                            }
                        }
                        InteractionTarget::Todo(task_id) => {
                            let tasks = app.current.todos.clone();
                            if let Some(index) = tasks.iter().position(|task| task.id == task_id)
                                && let Some(task) = tasks.get(index)
                            {
                                let command = match task.status.next() {
                                    TodoStatus::Pending => "/todo undo",
                                    TodoStatus::InProgress => "/todo doing",
                                    TodoStatus::Done => "/todo done",
                                };
                                let text = format!("{command} {}", index + 1);
                                let session = app.current.session_id.clone();
                                let handle = app.handle.clone();
                                tokio::spawn(async move {
                                    let _ = handle.execute_command(Some(session), &text).await;
                                });
                            }
                        }
                        InteractionTarget::TodoToggle => {
                            app.current.todo_collapsed = !app.current.todo_collapsed;
                        }
                        InteractionTarget::TodoClose => {
                            app.current.todo_hidden = true;
                            app.todo_window_rect = None;
                        }
                    }
                    if !live_thinking_target && !todo_target {
                        app.current.invalidate_output_layout();
                    }
                }
                app.mouse_dragged = false;
                return EventOutcome::redraw();
            }
            app.mouse_dragged = false;
            let Some(mut selection) = app.current.output_selection else {
                return EventOutcome::default();
            };
            selection.dragging = false;
            let Some((start, end)) = selection.range() else {
                app.current.output_selection = None;
                return EventOutcome::redraw();
            };
            app.current.output_selection = Some(selection);
            let Some(text) = app
                .current
                .message_layout
                .as_ref()
                .and_then(|layout| layout.text.get(start..end))
                .map(str::to_owned)
            else {
                app.current.status = "复制失败：选区位置已失效".into();
                return EventOutcome::redraw();
            };
            match crate::clipboard::copy_text(&text) {
                crate::clipboard::CopyResult::Native => {
                    app.current.status = "系统剪贴板已复制".into();
                    EventOutcome::redraw()
                }
                crate::clipboard::CopyResult::Osc52Requested(sequence) => {
                    app.current.status = "已向终端发送复制请求".into();
                    EventOutcome {
                        redraw: true,
                        osc52: Some(sequence),
                    }
                }
                crate::clipboard::CopyResult::Error(error) => {
                    app.current.status = format!("复制失败：{error}");
                    EventOutcome::redraw()
                }
            }
        }
        _ => EventOutcome::default(),
    }
}

fn update_drag_position(app: &mut App, column: u16, row: u16) {
    update_edge_scroll(app, column, row);
    let Some(offset) = app.current.message_layout.as_ref().and_then(|layout| {
        let clamped_row = row
            .max(layout.viewport.y)
            .min(layout.viewport.bottom().saturating_sub(1));
        layout.hit_test(column, clamped_row)
    }) else {
        return;
    };
    if let Some(selection) = &mut app.current.output_selection {
        selection.active = offset;
    }
}

fn update_edge_scroll(app: &mut App, column: u16, row: u16) {
    let Some(layout) = &app.current.message_layout else {
        return;
    };
    let direction = edge_scroll_direction(row, layout.viewport);
    app.current.edge_scroll = EdgeScroll { direction, column };
}

fn auto_scroll_selection(app: &mut App) {
    let direction = app.current.edge_scroll.direction;
    if direction == 0
        || !app
            .current
            .output_selection
            .is_some_and(|selection| selection.dragging)
    {
        return;
    }
    let _ = app
        .current
        .scroll_messages(if direction < 0 { 1 } else { -1 });
    let Some(layout) = &app.current.message_layout else {
        return;
    };
    let scroll = app.current.output_scroll_top.unwrap_or(layout.scroll);
    let row = if direction < 0 {
        scroll
    } else {
        scroll.saturating_add(layout.viewport.height.saturating_sub(1) as usize)
    };
    let column = relative_output_column(app.current.edge_scroll.column, layout.viewport);
    if let Some(offset) = layout.position_at_visual_row(row, column)
        && let Some(selection) = &mut app.current.output_selection
    {
        selection.active = offset;
    }
}

fn output_mouse_event_allowed(
    kind: MouseEventKind,
    settings_open: bool,
    palette_open: bool,
    approval_open: bool,
) -> bool {
    !settings_open
        && !palette_open
        && !approval_open
        && matches!(
            kind,
            MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
                | MouseEventKind::Moved
        )
}

fn relative_output_column(column: u16, viewport: ratatui::layout::Rect) -> usize {
    column.saturating_sub(viewport.x) as usize
}

const EDGE_SCROLL_ROWS: u16 = 1;

fn edge_scroll_direction(row: u16, viewport: ratatui::layout::Rect) -> i8 {
    if viewport.height == 0 {
        return 0;
    }
    let top_edge = viewport
        .y
        .saturating_add(EDGE_SCROLL_ROWS.saturating_sub(1));
    let bottom_edge = viewport.bottom().saturating_sub(EDGE_SCROLL_ROWS);
    if row <= top_edge {
        -1
    } else if row >= bottom_edge {
        1
    } else {
        0
    }
}

fn accept_envelope(app: &mut App, envelope: &Envelope) -> Option<bool> {
    if envelope.cursor <= app.event_cursor {
        return None;
    }
    let redraw = handle_envelope(app, envelope);
    app.event_cursor = envelope.cursor;
    Some(redraw)
}

fn handle_envelope(app: &mut App, envelope: &Envelope) -> bool {
    let approval_redraw = match &envelope.event {
        ProtocolEvent::Approval {
            approval_id,
            call,
            reason,
            source_title,
            ..
        } => {
            // The modal reads the facade's global oldest approval, while the
            // projection only owns active-session display state. Populate the
            // global slot directly from the live event so an agent cannot wait
            // invisibly until the server-side approval timeout. A newer event
            // must not replace an older approval already being shown.
            if app.approval.is_none() {
                app.approval = Some(ApprovalDisplay {
                    approval_id: approval_id.clone(),
                    call: call.clone(),
                    reason: reason.clone(),
                    source_session_id: Some(envelope.session_id.clone()),
                    source_title: source_title.clone(),
                    created_at: std::time::Instant::now(),
                });
            }
            true
        }
        ProtocolEvent::ApprovalResolved { approval_id, .. } => {
            if app
                .approval
                .as_ref()
                .is_some_and(|approval| approval.approval_id == *approval_id)
            {
                app.approval = None;
            }
            // The resolved approval may expose another pending approval behind
            // it. Refresh the authoritative global-oldest slot without moving
            // the live event cursor (sync_snapshot preserves it).
            app.sync_pending = true;
            true
        }
        _ => false,
    };
    match &envelope.event {
        ProtocolEvent::SessionsChanged | ProtocolEvent::ResyncRequired => {
            app.sync_pending = true;
            true
        }
        ProtocolEvent::ChildSessionProgress {
            child_session_id,
            status,
            turn,
            max_turns,
            tool,
        } => {
            app.child_status.insert(
                child_session_id.clone(),
                ChildSessionProgress {
                    status: child_status_from_wire(status),
                    turn: *turn,
                    max_turns: *max_turns,
                    tool: tool.clone(),
                    updated_at: std::time::Instant::now(),
                },
            );
            true
        }
        _ => {
            if envelope.session_id == app.current.session_id && !app.current.session_id.is_empty() {
                let outcome = app.current.handle_event(&envelope.event);
                if outcome.sessions_dirty || outcome.transcript_dirty {
                    app.sync_pending = true;
                }
                approval_redraw
                    || outcome.force_redraw
                    || outcome.transcript_dirty
                    || outcome.sessions_dirty
            } else {
                // Background session events only affect the session list.
                app.sync_pending = true;
                true
            }
        }
    }
}

impl App {
    /// Merges snapshot state into the facade. `advance_cursor` is only true
    /// when the caller is establishing a new event-stream baseline; a normal
    /// refresh while the live stream is active must not skip envelopes that
    /// have reached the bridge but have not yet been rendered.
    fn merge_snapshot(&mut self, snapshot: &AppSnapshotV2, advance_cursor: bool) {
        if advance_cursor {
            self.event_cursor = snapshot.event_cursor;
        }
        if let Some(session_id) = &snapshot.active_session {
            if !self.current.session_id.is_empty() && self.current.session_id != *session_id {
                // The active session changed: rebuild the projection shell.
                self.current = TuiSessionProjection::new(
                    session_id.clone(),
                    AgentMode::parse(&snapshot.mode).unwrap_or_default(),
                    snapshot
                        .context
                        .as_ref()
                        .and_then(|context| context.context_window_tokens),
                );
            }
            self.active_session = session_id.clone();
            if self.current.session_id.is_empty() {
                self.current.session_id = session_id.clone();
            }
        }
        if let Some(session) = snapshot
            .sessions
            .iter()
            .find(|session| session.id == self.current.session_id)
        {
            self.current.title = session.title.clone();
            self.current.parent_id = session.parent_id.clone();
            self.current.status = session.status.clone();
            self.current.busy = session.busy;
            if let Ok(phase) = parse_phase(&session.phase) {
                self.current.agent_phase = phase;
            }
        }
        self.current.mode = AgentMode::parse(&snapshot.mode).unwrap_or(self.current.mode);
        // The snapshot budget is authoritative; clear stale local context
        // state when the core reports no budget for this session.
        self.current.context_budget = snapshot.context.clone();
        self.current.context_limit_tokens = snapshot
            .context
            .as_ref()
            .and_then(|context| context.context_window_tokens);
        self.current.context_used_tokens = snapshot
            .context
            .as_ref()
            .map_or(0, |context| context.used_tokens);
        self.current.context_overlay_tokens = 0;
        // Render a persisted incomplete answer (surviving a restart) once, and
        // drop it once the core clears the partial (normal completion).
        self.current
            .sync_partial(snapshot.assistant_partial.as_ref());
        self.sessions = snapshot.sessions.iter().map(session_summary).collect();
        self.approval = snapshot.approval.as_ref().map(approval_display);
        self.current.todos = snapshot
            .todos
            .iter()
            .map(|todo| TodoTask {
                id: todo.id.clone(),
                title: todo.title.clone(),
                status: todo_status_from_wire(&todo.status),
                created_at: todo.created_at.clone(),
                updated_at: todo.updated_at.clone(),
            })
            .collect();
        self.current.todo_collapsed = !self.current.todos.is_empty()
            && self
                .current
                .todos
                .iter()
                .all(|task| task.status == TodoStatus::Done);
        self.config.provider.model = snapshot.model.clone();
        if let Some(preset) = ProviderPreset::ALL
            .iter()
            .copied()
            .find(|preset| preset.label() == snapshot.provider)
        {
            self.config.provider.preset = preset;
        }
        if let Some(provider) = self
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.preset.label() == snapshot.provider)
        {
            provider.model = snapshot.model.clone();
            self.config.provider = provider.clone();
        }
        if let Some(settings) = &mut self.provider_settings {
            settings.active.model = snapshot.model.clone();
            if let Some(preset) = ProviderPreset::ALL
                .iter()
                .copied()
                .find(|preset| preset.label() == snapshot.provider)
            {
                settings.active.preset = preset.key_id().to_owned();
            }
        }
    }

    pub(crate) fn sync_from_snapshot(&mut self, snapshot: &AppSnapshotV2) {
        self.merge_snapshot(snapshot, true);
    }

    /// Refetches the snapshot and merges it into the facade. Never touches
    /// the live history; call [`App::load_history`] separately when the
    /// transcript changed.
    pub(crate) async fn sync_snapshot(&mut self) -> Result<()> {
        let snapshot = self.handle.snapshot().await?;
        self.merge_snapshot(&snapshot, false);
        Ok(())
    }

    /// Refetches state after the event cursor has been evicted or a broadcast
    /// receiver has lagged. The snapshot becomes the new replay baseline.
    pub(crate) async fn resync_all(&mut self) -> Result<()> {
        let snapshot = self.handle.snapshot().await?;
        self.merge_snapshot(&snapshot, true);
        self.load_history().await
    }

    /// Refetches the newest message page for the active session and replaces
    /// the projection history from the database. The page's `has_more` /
    /// `next_before` cursors drive subsequent [`Self::load_older_history`]
    /// calls, so history is no longer capped at a fixed "last 100" snapshot.
    pub(crate) async fn load_history(&mut self) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if session_id.is_empty() {
            return Ok(());
        }
        let page = self.handle.messages(&session_id, None, None).await?;
        self.current.apply_message_page(&page, false);
        Ok(())
    }

    /// Loads the next older message page and prepends it, anchoring the scroll
    /// so the visible content stays put. No-op when the head is fully loaded.
    pub(crate) async fn load_older_history(&mut self) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if session_id.is_empty() || !self.current.history_has_more {
            return Ok(());
        }
        let Some(before) = self.current.history_next_before else {
            return Ok(());
        };
        let page = self
            .handle
            .messages(&session_id, Some(before), None)
            .await?;
        self.current.apply_message_page(&page, true);
        Ok(())
    }

    /// Snapshot merge plus history refetch. Used after any mutation that may
    /// have changed the active session's transcript.
    pub(crate) async fn sync_all(&mut self) -> Result<()> {
        self.sync_snapshot().await?;
        self.load_history().await?;
        Ok(())
    }
}

fn child_status_from_wire(status: &str) -> protium_core::agent::ChildSessionStatus {
    match status {
        "completed" => protium_core::agent::ChildSessionStatus::Completed,
        "failed" => protium_core::agent::ChildSessionStatus::Failed,
        "turn_limit" => protium_core::agent::ChildSessionStatus::TurnLimit,
        "timed_out" => protium_core::agent::ChildSessionStatus::TimedOut,
        "cancelled" => protium_core::agent::ChildSessionStatus::Cancelled,
        _ => protium_core::agent::ChildSessionStatus::Queued,
    }
}

fn approval_display(approval: &ApprovalDto) -> ApprovalDisplay {
    ApprovalDisplay {
        approval_id: approval.approval_id.clone(),
        call: approval.call.clone(),
        reason: approval.reason.clone(),
        source_session_id: Some(approval.session_id.clone()),
        source_title: approval.source_title.clone(),
        created_at: std::time::Instant::now(),
    }
}

async fn open_settings(app: &mut App) {
    // Provider settings are core-authoritative; the local config is only a
    // fallback until the first read lands.
    let _ = refresh_provider_settings(app).await;
    app.settings = Some(provider_list_state(app));
    app.settings_field_index = 0;
    app.context_window_input.clear();
    let _ = load_provider_models(app, false).await;
    app.current.status = "已连接的供应商".into();
}

/// Converts the core settings DTO back into the display/edit shape the
/// existing settings UI consumes. Secrets never cross this boundary.
fn provider_configs_from_settings(app: &App) -> Vec<crate::config::ProviderConfig> {
    let Some(settings) = &app.provider_settings else {
        return app.config.providers.clone();
    };
    let mut providers = settings
        .saved
        .iter()
        .map(|profile| provider_config_from_profile(app, profile))
        .collect::<Vec<_>>();
    // The active profile is always editable even when it has never been
    // explicitly saved (for example the built-in default provider).
    if !providers
        .iter()
        .any(|provider| provider.preset.key_id() == settings.active.preset)
        && let Some(active) = provider_config_from_active(app, settings)
    {
        providers.insert(0, active);
    }
    providers
}

fn provider_config_from_active(
    app: &App,
    settings: &ProviderSettingsDto,
) -> Option<crate::config::ProviderConfig> {
    let preset = ProviderPreset::parse(&settings.active.preset)?;
    let mut config = if preset == app.config.provider.preset {
        app.config.provider.clone()
    } else {
        preset.defaults()
    };
    config.preset = preset;
    config.model = settings.active.model.clone();
    if !settings.active.base_url.trim().is_empty() {
        config.base_url = settings.active.base_url.clone();
    }
    if let Some(kind) = ProviderKind::parse_wire_tag(&settings.active.kind) {
        config.kind = kind;
    }
    Some(config)
}

fn provider_config_from_profile(
    app: &App,
    profile: &ProviderProfileDto,
) -> crate::config::ProviderConfig {
    let preset = ProviderPreset::parse(&profile.preset).unwrap_or_default();
    // The DTO intentionally omits thinking/retry fields. For the preset the
    // local config still tracks, start from that richer copy so editing the
    // active provider does not silently reset those customizations; the core
    // merge keeps them on apply anyway.
    let mut config = if preset == app.config.provider.preset {
        app.config.provider.clone()
    } else {
        preset.defaults()
    };
    config.preset = preset;
    config.model = profile.model.clone();
    if !profile.base_url.trim().is_empty() {
        config.base_url = profile.base_url.clone();
    }
    if let Some(kind) = ProviderKind::parse_wire_tag(&profile.kind) {
        config.kind = kind;
    }
    config
}

/// Connected presets from the core settings view. Falls back to the local
/// secret cache only before the first core read (e.g. during tests).
fn available_key_presets(app: &App) -> HashSet<ProviderPreset> {
    if let Some(settings) = &app.provider_settings {
        return settings
            .connected
            .iter()
            .filter_map(|preset| ProviderPreset::parse(preset))
            .collect();
    }
    ProviderPreset::ALL
        .iter()
        .filter_map(|preset| secrets::api_key_cached_only(*preset).ok().map(|_| *preset))
        .collect()
}

fn provider_form(app: &App, provider: crate::config::ProviderConfig) -> SettingsForm {
    let preset = provider.preset;
    let available = available_key_presets(app);
    let existing_key_preset = available.contains(&preset).then_some(preset);
    let mut form = SettingsForm::new(provider, existing_key_preset);
    form.set_available_key_presets(available);
    form
}

/// Builds the settings list with `connected` sourced from the core DTO (the
/// local `SettingsState::list` constructor would re-derive it from the cache).
fn provider_list_state(app: &App) -> SettingsState {
    let providers = provider_configs_from_settings(app);
    let mut state = SettingsState::list(providers, app.active_preset());
    if let SettingsState::List(list) = &mut state
        && let Some(settings) = &app.provider_settings
    {
        list.connected = settings
            .connected
            .iter()
            .filter_map(|preset| ProviderPreset::parse(preset))
            .collect();
    }
    state
}

fn reopen_provider_list(app: &mut App) {
    app.settings = Some(provider_list_state(app));
    app.settings_field_index = 0;
    app.context_window_input.clear();
}

fn open_provider_form(app: &mut App, provider: crate::config::ProviderConfig) {
    // The core profile DTO intentionally omits the explicit context window, so
    // the safe default is "inherit the merged profile value". Typing a number
    // sends an override the core clamps; leaving it empty keeps whatever the
    // core already has (including edits made by WebUI).
    app.context_window_input.clear();
    app.settings_field_index = 0;
    app.settings = Some(SettingsState::Form(provider_form(app, provider)));
}

fn open_template_picker(app: &mut App) {
    if let Some(settings) = &mut app.settings {
        settings.open_templates();
        app.current.status = "选择供应商模板".into();
    }
}

fn open_selected_profile(app: &mut App) {
    if let Some(provider) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_profile)
    {
        open_provider_form(app, provider);
        app.current.status = "编辑供应商".into();
    }
}

fn open_selected_template(app: &mut App) {
    if let Some(preset) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_template)
    {
        open_provider_form(app, preset.defaults());
        app.current.status = format!("添加 {}", preset.label());
    }
}

/// Total selectable rows in the settings form: the core `FIELDS` registry plus
/// one TUI-only synthetic context-window override row.
fn settings_row_count() -> usize {
    FIELDS.len() + 1
}

/// TUI row index of the synthetic context-window override. It is rendered
/// immediately after Thinking and before the write-only API key, so it sits at
/// the last core field index rather than at the end of the row list.
fn context_window_row() -> usize {
    FIELDS.len().saturating_sub(1)
}

fn settings_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    let paste_shortcut = code == KeyCode::Char('v')
        && modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::META);
    paste_shortcut
        || matches!(
            code,
            KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Enter
        )
        || (code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL))
        || matches!(code, KeyCode::Char(_) if !modifiers.contains(KeyModifiers::CONTROL))
}

fn palette_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Up | KeyCode::Down | KeyCode::Backspace
    ) || matches!(code, KeyCode::Char(_) if !modifiers.contains(KeyModifiers::CONTROL))
}

fn paste_text_into_settings(app: &mut App, text: &str) -> bool {
    if app.settings_field_index == context_window_row() {
        let sanitized = text.replace(['\r', '\n'], "");
        if sanitized
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            app.context_window_input = sanitized;
            return true;
        }
        return false;
    }
    let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) else {
        return false;
    };
    let field = form.field();
    if !matches!(
        field,
        SettingsField::Model | SettingsField::BaseUrl | SettingsField::ApiKey
    ) {
        return false;
    }
    let sanitized = text.replace(['\r', '\n'], "");
    let mut sanitized = sanitized.as_str();
    if sanitized.len() > crate::clipboard::MAX_CLIPBOARD_BYTES {
        let mut end = crate::clipboard::MAX_CLIPBOARD_BYTES;
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized = &sanitized[..end];
    }
    form.paste(field, sanitized);
    true
}

async fn handle_settings_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            if matches!(app.settings, Some(SettingsState::List(_))) {
                app.settings = None;
                app.current.status = "设置已取消".into();
            } else {
                reopen_provider_list(app);
                app.current.status = "已返回供应商列表".into();
            }
        }
        KeyCode::Tab | KeyCode::Down => {
            if app
                .settings
                .as_ref()
                .is_some_and(|settings| settings.form().is_some())
            {
                app.settings_field_index = (app.settings_field_index + 1) % settings_row_count();
                sync_form_selection(app);
            } else if let Some(settings) = &mut app.settings {
                settings.move_selection(1);
            }
        }
        KeyCode::BackTab | KeyCode::Up => {
            if app
                .settings
                .as_ref()
                .is_some_and(|settings| settings.form().is_some())
            {
                app.settings_field_index =
                    (app.settings_field_index + settings_row_count() - 1) % settings_row_count();
                sync_form_selection(app);
            } else if let Some(settings) = &mut app.settings {
                settings.move_selection(-1);
            }
        }
        KeyCode::Left | KeyCode::Right => {
            let direction = if code == KeyCode::Right { 1 } else { -1 };
            if app.settings_field_index == context_window_row() {
                return;
            }
            if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.cycle(field, direction);
            }
        }
        KeyCode::Backspace => {
            if app.settings_field_index == context_window_row() {
                app.context_window_input.pop();
            } else if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.edit(field, None);
            }
        }
        KeyCode::Char('v')
            if modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::META) =>
        {
            match crate::clipboard::read_text() {
                Ok(text) => {
                    if paste_text_into_settings(app, &text) {
                        app.current.status = "已粘贴剪贴板内容".into();
                    } else {
                        app.current.status = "当前字段不支持粘贴".into();
                    }
                }
                Err(error) => {
                    app.current.status = format!("无法读取系统剪贴板：{}", secrets::redact(&error));
                }
            }
        }
        KeyCode::Delete | KeyCode::Char('d')
            if matches!(app.settings, Some(SettingsState::Form(_)))
                && (code == KeyCode::Delete || modifiers.contains(KeyModifiers::CONTROL)) =>
        {
            if app.settings_field_index == context_window_row() {
                if code == KeyCode::Delete {
                    app.context_window_input.clear();
                }
            } else if let Err(error) = remove_settings_provider(app).await {
                app.current.status = format!("移除失败：{}", secrets::redact(&error.to_string()));
            }
        }
        KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if app.settings_field_index == context_window_row() {
                if character.is_ascii_digit() {
                    app.context_window_input.push(character);
                }
            } else if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.edit(field, Some(character));
            }
        }
        KeyCode::Enter => match app.settings.as_ref() {
            Some(settings) if settings.on_add_row() => open_template_picker(app),
            Some(SettingsState::List(_)) => open_selected_profile(app),
            Some(SettingsState::Templates(_)) => open_selected_template(app),
            Some(SettingsState::Form(_)) => {
                if let Err(error) = apply_settings(app).await {
                    app.current.status =
                        format!("设置错误：{}", secrets::redact(&error.to_string()));
                }
            }
            None => {}
        },
        _ => {}
    }
}

/// Mirrors the TUI row index onto the core form's `FIELDS` selection. The
/// synthetic context-window row has no core `SettingsField`, so the core
/// selection clamps to the last real field while it is highlighted.
fn sync_form_selection(app: &mut App) {
    // TUI rows: [Preset, Protocol, Model, BaseUrl, Thinking, ContextWindow,
    // ApiKey]. The synthetic row maps onto the last core field so the form's
    // own `field()` never indexes out of bounds; edits for it are intercepted
    // before this mapping is consulted.
    let index = app.settings_field_index.min(FIELDS.len().saturating_sub(1));
    if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
        form.selected = index;
    }
}

async fn apply_settings(app: &mut App) -> Result<()> {
    let (preset, model, base_url, kind, entered_key, context_window, form_provider) = {
        let form = app
            .settings
            .as_ref()
            .and_then(SettingsState::form)
            .context("provider editor is not open")?;
        let provider = form.prepare()?;
        let context_window = parse_context_window_override(&app.context_window_input)?;
        (
            provider.preset,
            provider.model.clone(),
            provider.base_url.clone(),
            provider.kind,
            form.api_key.trim().to_owned(),
            context_window,
            provider,
        )
    };

    // Ordering matters: `store_api_key_cached` seeds the in-process cache even
    // when the OS keyring write fails, so the core's rebuilt runner can pick
    // the new key up immediately. The warning below marks the degraded case.
    let key_warning = if entered_key.is_empty() {
        None
    } else {
        secrets::store_api_key_cached(preset, &entered_key)
            .err()
            .map(|error| {
                format!(
                    "API Key 仅本次运行有效：{}",
                    secrets::redact(&error.to_string())
                )
            })
    };

    // Thinking is an advanced full-profile field the profile endpoint does not
    // carry. When the edited provider is active and the user changed it in the
    // form, commit the merged full profile once instead of saving twice; for a
    // non-active provider we keep the merge endpoint and warn that thinking is
    // edited via the top-level menu once the provider is active.
    let active = app.config.provider.preset == preset;
    let thinking_changed = active
        && (form_provider.thinking != app.config.provider.thinking
            || form_provider.thinking_level != app.config.provider.thinking_level
            || form_provider.thinking_budget_tokens != app.config.provider.thinking_budget_tokens);
    let thinking_warning = (!active
        && (form_provider.thinking != preset.defaults().thinking
            || form_provider.thinking_level != preset.defaults().thinking_level
            || form_provider.thinking_budget_tokens != preset.defaults().thinking_budget_tokens))
        .then(|| "思考设置请在切换为当前供应商后通过顶部菜单修改".to_owned());

    if active {
        app.cancel_model_refresh();
    }
    if thinking_changed {
        let mut provider = app.config.provider.clone();
        provider.preset = preset;
        provider.model = model.clone();
        if !base_url.trim().is_empty() {
            provider.base_url = base_url.clone();
        }
        provider.kind = kind;
        if let Some(window) = context_window {
            provider.context_window_tokens = Some(window);
        }
        provider.thinking = form_provider.thinking;
        provider.thinking_level = form_provider.thinking_level;
        provider.thinking_budget_tokens = form_provider.thinking_budget_tokens;
        provider.normalize_thinking();
        if let Err(error) = app.handle.set_provider_config(provider).await {
            return Err(anyhow::anyhow!(secrets::redact(&error.message)));
        }
    } else if let Err(error) = app
        .handle
        .set_provider_profile(
            preset,
            &model,
            (!base_url.trim().is_empty()).then_some(base_url.as_str()),
            Some(kind),
            context_window,
        )
        .await
    {
        return Err(anyhow::anyhow!(secrets::redact(&error.message)));
    }

    // The core persisted the profile once; refresh every derived surface from
    // the core instead of writing the local config a second time.
    let _ = refresh_provider_settings(app).await;
    let _ = load_provider_models(app, false).await;
    app.current.context_limit_tokens = None;
    app.sync_all().await?;
    reopen_provider_list(app);
    let warnings = [key_warning, thinking_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    app.current.status = format!(
        "就绪 | {} | {}{}",
        preset.label(),
        model,
        if warnings.is_empty() {
            String::new()
        } else {
            format!(" | {warnings}")
        },
    );
    Ok(())
}

/// Parses the optional context-window override. Empty inherits the merged
/// profile value; a number is passed to the core, which clamps it.
fn parse_context_window_override(input: &str) -> Result<Option<u64>> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let value = input
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("上下文窗口必须是正整数"))?;
    if value == 0 {
        return Err(anyhow::anyhow!("上下文窗口必须是正整数"));
    }
    Ok(Some(value))
}

pub(crate) fn braille_spinner_supported() -> bool {
    const BRAILLE_BLANK: char = '⠀';
    const BRAILLE_SET: [char; 6] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴'];
    let mut width = 0;
    for character in [BRAILLE_BLANK].into_iter().chain(BRAILLE_SET) {
        if let Some(value) = unicode_width::UnicodeWidthChar::width(character) {
            width += value;
        }
    }
    width == BRAILLE_SET.len() + 1
}

pub(crate) fn thinking_animation_glyph(frame: usize, braille: bool) -> &'static str {
    if braille {
        const BRAILLE: [&str; 6] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];
        BRAILLE[frame % BRAILLE.len()]
    } else {
        const ASCII: [&str; 4] = ["-", "\\", "|", "/"];
        ASCII[frame % ASCII.len()]
    }
}

async fn remove_settings_provider(app: &mut App) -> Result<()> {
    let preset = app
        .settings
        .as_ref()
        .and_then(SettingsState::form)
        .map(|form| form.provider.preset)
        .context("provider editor is not open")?;
    app.cancel_model_refresh();
    app.handle.remove_provider(preset).await?;
    // Converge from the core view; never reload config as the authority.
    let _ = refresh_provider_settings(app).await;
    app.provider_models = ProviderModelsState::default();
    let _ = load_provider_models(app, false).await;
    app.sync_all().await?;
    reopen_provider_list(app);
    app.current.status = "供应商已移除；API Key 已保留在系统钥匙串".into();
    Ok(())
}

#[cfg(test)]
mod tests;
