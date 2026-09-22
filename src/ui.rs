use pulldown_cmark::{
    Alignment as MarkdownAlignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options,
    Parser, Tag, TagEnd,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use serde_json::Value;
use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    ops::Range,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    agent::ChildSessionStatus,
    app::{
        App, CommandPaletteState, DisplayContent, DisplayKind, ThinkingDisplay, TodoDisplay,
        TodoStatus, TodoTask, ToolDisplay, ToolDisplayStatus,
    },
    commands::{self, AgentMode},
    input::input_cursor_viewport,
    output::{InteractionTarget, MessageLayout, OutputSelection, VisualLine},
    secrets,
    settings::{FIELDS, SettingsField, SettingsForm, SettingsState},
    storage::SessionSummary,
    ui_layout::{Density, HeightClass, compute_layout, message_block},
    ui_theme::{UiTheme, VisualRole},
    ui_view_model::{
        FooterLine, InputView, ThinkingControlView, UiSegment, UiViewModel, mode_label,
    },
};

#[cfg(test)]
use crate::input::input_viewport;

struct RenderedMessageLines {
    lines: Vec<Line<'static>>,
    interactions: Vec<Option<InteractionTarget>>,
    thinking_before: Option<usize>,
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let density = Density::from_width(area.width);
    let height = HeightClass::from_height(area.height);
    let layout = compute_layout(area, density, height);
    let view = UiViewModel::from_app(app, density, height, layout.footer.width as usize);
    #[cfg(test)]
    {
        app.current.footer_rebuild_count += 1;
    }
    let theme = UiTheme::default();

    if let Some(sessions) = layout.sessions {
        draw_sessions(frame, sessions, app, &theme);
    } else {
        app.session_panel_rect = None;
    }
    draw_messages(
        frame,
        layout.messages_outer,
        layout.messages_inner,
        app,
        &theme,
    );
    draw_input(frame, layout.input, app, &view.input, &theme);
    if !app.file_suggestions.is_empty() && app.palette.is_none() && app.settings.is_none() {
        draw_file_suggestions(frame, layout.input, app);
    }
    draw_footer(frame, layout.footer, &view, app, &theme);
    if app.thinking_menu_open {
        draw_thinking_menu(frame, area, layout.footer, app, &view.thinking, &theme);
    } else {
        app.thinking_menu_rect = None;
    }
    if app.provider_menu_open {
        draw_provider_menu(frame, area, layout.footer, app, &theme);
    } else {
        app.provider_menu_rect = None;
    }
    if app.model_menu_open {
        draw_model_menu(frame, area, layout.footer, app, &theme);
    } else {
        app.model_menu_rect = None;
    }
    app.settings_rect = app.settings.as_ref().map(|settings| match settings {
        SettingsState::List(_) => centered_rect(78, 20, area),
        SettingsState::Templates(_) => centered_rect(68, 18, area),
        SettingsState::Form(_) => centered_rect(88, 24, area),
    });
    if let Some(settings) = &app.settings {
        draw_settings(frame, area, settings, app, &theme);
    }
    if let Some(palette) = &app.palette {
        draw_palette(frame, area, palette, &theme);
    }
    if app.has_pending_approval() {
        draw_approval(frame, area, app, &theme);
    }
}

fn input_mode_rect(area: Rect, mode: AgentMode) -> Option<Rect> {
    const PREFIX: &str = " 输入 · ";
    let label = mode_label(mode);
    let prefix_width = UnicodeWidthStr::width(PREFIX) as u16;
    let label_width = UnicodeWidthStr::width(label) as u16;
    if label_width == 0 {
        return None;
    }
    let x = area.x.saturating_add(1).saturating_add(prefix_width);
    if x.saturating_add(label_width) > area.right().saturating_sub(1) {
        return None;
    }
    Some(Rect::new(x, area.y, label_width, 1))
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, app: &mut App, view: &InputView, theme: &UiTheme) {
    app.input_mode_rect = input_mode_rect(area, app.current.mode);
    let style = if view.enabled {
        theme.style(VisualRole::Primary)
    } else {
        theme.style(VisualRole::Muted)
    };
    let border_style = if view.warning {
        theme.style(VisualRole::Warning)
    } else if view.enabled {
        theme.focus_border
    } else {
        theme.inactive_border
    };
    let inner_width = area.width.saturating_sub(2) as usize;
    let viewport = input_cursor_viewport(app.input.as_str(), app.input.cursor(), inner_width);
    frame.render_widget(
        Paragraph::new(viewport.text).style(style).block(
            Block::default()
                .title(view.title.as_str())
                .borders(Borders::ALL)
                .border_style(border_style),
        ),
        area,
    );
    if !app.current.busy && app.settings.is_none() && !app.has_pending_approval() {
        let cursor_x = area.x + 1 + viewport.cursor_column as u16;
        let cursor_y = area.y + 1 + viewport.cursor_row.min(area.height.saturating_sub(3));
        frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), cursor_y));
    }
}

fn draw_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &UiViewModel,
    app: &mut App,
    theme: &UiTheme,
) {
    let mut lines = vec![footer_line(
        &view.footer.primary,
        area.width as usize,
        theme,
    )];
    if area.height > 1
        && let Some(secondary) = &view.footer.secondary
    {
        lines.push(footer_line(secondary, area.width as usize, theme));
    }
    frame.render_widget(Paragraph::new(lines), area);
    app.thinking_control_rect = thinking_control_rect(area, view);
    (app.provider_control_rect, app.model_control_rect) = provider_model_rects(area, view, app);
}

fn provider_model_rects(area: Rect, view: &UiViewModel, app: &App) -> (Option<Rect>, Option<Rect>) {
    let Some(secondary) = view.footer.secondary.as_ref() else {
        return (None, None);
    };
    let right = clip_segments(&secondary.right, area.width as usize);
    let right_width = segment_width(&right);
    let left_budget = (area.width as usize)
        .saturating_sub(right_width.saturating_add(usize::from(right_width > 0)));
    let left = clip_segments(&secondary.left, left_budget);
    let Some(text) = left.first().map(|segment| segment.text.as_str()) else {
        return (None, None);
    };
    let prefix = format!("{} · ", mode_label(app.current.mode));
    if !text.starts_with(&prefix) || area.height < 2 {
        return (None, None);
    }
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let visible_width = UnicodeWidthStr::width(text) as u16;
    let provider_width = UnicodeWidthStr::width(app.provider_label()) as u16;
    let separator_width = UnicodeWidthStr::width(" · ") as u16;
    let model_width = UnicodeWidthStr::width(app.model_name()) as u16;
    let provider_x = area.x.saturating_add(prefix_width);
    let model_x = provider_x
        .saturating_add(provider_width)
        .saturating_add(separator_width);
    let provider = (provider_width > 0
        && visible_width >= prefix_width.saturating_add(provider_width))
    .then(|| Rect::new(provider_x, area.y.saturating_add(1), provider_width, 1));
    let model = (model_width > 0
        && visible_width
            >= prefix_width
                .saturating_add(provider_width)
                .saturating_add(separator_width)
                .saturating_add(model_width))
    .then(|| Rect::new(model_x, area.y.saturating_add(1), model_width, 1));
    (provider, model)
}

fn draw_provider_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer: Rect,
    app: &mut App,
    theme: &UiTheme,
) {
    let choices = crate::app::provider_choices(app);
    let content_width = choices
        .iter()
        .map(|preset| UnicodeWidthStr::width(preset.label()))
        .max()
        .unwrap_or(12)
        .saturating_add(4) as u16;
    let width = content_width.clamp(20, 36).min(screen.width);
    let height = (choices.len() as u16).saturating_add(2).min(screen.height);
    let control = app
        .provider_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let x = control.x.min(screen.right().saturating_sub(width));
    let y = footer.y.saturating_sub(height);
    let area = Rect::new(x, y, width, height);
    app.provider_menu_rect = Some(area);

    let items = choices
        .iter()
        .enumerate()
        .map(|(index, preset)| {
            let selected = index == app.provider_menu_selected;
            let connected = app
                .provider_settings
                .as_ref()
                .is_some_and(|settings| settings.connected.iter().any(|id| id == preset.key_id()));
            let status = if connected {
                "已连接"
            } else {
                "需要 API Key"
            };
            ListItem::new(Line::from(Span::styled(
                format!(
                    "{} {:<18} {status}",
                    if selected { "›" } else { " " },
                    preset.label()
                ),
                if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                },
            )))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" 选择供应商 ")
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        area,
    );
}

fn draw_model_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer: Rect,
    app: &mut App,
    theme: &UiTheme,
) {
    let choices = crate::app::model_choices(app);
    let content_width = choices
        .iter()
        .map(|choice| UnicodeWidthStr::width(choice.label.as_str()))
        .max()
        .unwrap_or(12)
        .saturating_add(4) as u16;
    let width = content_width.clamp(24, 52).min(screen.width);
    let height = (choices.len() as u16)
        .saturating_add(2)
        .min(14)
        .min(screen.height);
    let control = app
        .model_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let x = control.x.min(screen.right().saturating_sub(width));
    let y = footer.y.saturating_sub(height);
    let area = Rect::new(x, y, width, height);
    app.model_menu_rect = Some(area);

    let visible = area.height.saturating_sub(2) as usize;
    let scroll = app
        .model_menu_selected
        .saturating_sub(visible.saturating_sub(1));
    let items = choices
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(index, choice)| {
            let selected = index == app.model_menu_selected;
            ListItem::new(Line::from(Span::styled(
                format!("{} {}", if selected { "›" } else { " " }, choice.label),
                if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                },
            )))
        })
        .collect::<Vec<_>>();
    let status = if app.provider_models.loading {
        " · 刷新中…"
    } else if app.provider_models.last_error.is_some() {
        " · 刷新失败"
    } else {
        ""
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(format!(" {} 模型 · r 刷新{status} ", app.provider_label()))
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        area,
    );
}

fn thinking_control_rect(area: Rect, view: &UiViewModel) -> Option<Rect> {
    if area.height < 2 || view.footer.secondary.is_none() {
        return None;
    }
    let width = UnicodeWidthStr::width(view.thinking.label.as_str()) as u16;
    (width > 0 && width <= area.width).then(|| {
        Rect::new(
            area.right().saturating_sub(width),
            area.y.saturating_add(1),
            width,
            1,
        )
    })
}

fn draw_thinking_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer: Rect,
    app: &mut App,
    view: &ThinkingControlView,
    theme: &UiTheme,
) {
    let rows = view
        .options
        .len()
        .max(if view.qwen37_budgets { 6 } else { 0 });
    let width = if view.qwen37_budgets { 28 } else { 18 }.min(screen.width);
    let height = (rows as u16).saturating_add(2).min(screen.height);
    let control = app
        .thinking_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let x = control
        .right()
        .saturating_sub(width)
        .min(screen.right().saturating_sub(width));
    let y = footer.y.saturating_sub(height);
    let area = Rect::new(x, y, width, height);
    app.thinking_menu_rect = Some(area);

    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let left = view.options.get(index).map_or_else(String::new, |item| {
            format!("{} {}", if item.selected { "●" } else { "○" }, item.label)
        });
        let text = if view.qwen37_budgets {
            const BUDGETS: [(Option<u32>, &str); 6] = [
                (None, "默认"),
                (Some(1024), "1k"),
                (Some(4096), "4k"),
                (Some(8192), "8k"),
                (Some(16384), "16k"),
                (Some(32768), "32k"),
            ];
            let (budget, label) = BUDGETS.get(index).copied().unwrap_or((None, ""));
            format!(
                "{left:<8} {} {label}",
                if view.budget_tokens == budget
                    && app.thinking_level() == crate::config::ThinkingLevel::Enabled
                {
                    "●"
                } else {
                    "○"
                }
            )
        } else {
            left
        };
        lines.push(Line::from(Span::styled(
            text,
            theme.style(VisualRole::Primary),
        )));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" 思考强度 ")
                .border_style(theme.style(VisualRole::Accent)),
        ),
        area,
    );
}

fn footer_line(view: &FooterLine, width: usize, theme: &UiTheme) -> Line<'static> {
    let right = clip_segments(&view.right, width);
    let right_width = segment_width(&right);
    let left_budget =
        width.saturating_sub(right_width.saturating_add(usize::from(right_width > 0)));
    let left = clip_segments(&view.left, left_budget);
    let left_width = segment_width(&left);
    let gap = width.saturating_sub(left_width.saturating_add(right_width));
    let mut spans = render_segments(&left, theme);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(render_segments(&right, theme));
    Line::from(spans)
}

fn clip_segments(segments: &[UiSegment], width: usize) -> Vec<UiSegment> {
    let mut output = Vec::new();
    let mut remaining = width;
    for segment in segments {
        if remaining == 0 {
            break;
        }
        let segment_width = UnicodeWidthStr::width(segment.text.as_str());
        if segment_width <= remaining {
            output.push(segment.clone());
            remaining -= segment_width;
            continue;
        }
        let mut text = String::new();
        let mut used = 0usize;
        for grapheme in segment.text.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if used.saturating_add(grapheme_width) > remaining {
                break;
            }
            text.push_str(grapheme);
            used = used.saturating_add(grapheme_width);
        }
        if !text.is_empty() {
            output.push(UiSegment {
                text,
                role: segment.role,
            });
        }
        break;
    }
    output
}

fn segment_width(segments: &[UiSegment]) -> usize {
    segments
        .iter()
        .map(|segment| UnicodeWidthStr::width(segment.text.as_str()))
        .sum()
}

fn render_segments(segments: &[UiSegment], theme: &UiTheme) -> Vec<Span<'static>> {
    segments
        .iter()
        .map(|segment| {
            let style = if matches!(segment.role, VisualRole::Primary | VisualRole::Shortcut) {
                theme.strong(segment.role)
            } else {
                theme.style(segment.role)
            };
            Span::styled(segment.text.clone(), style)
        })
        .collect()
}

fn draw_approval(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &UiTheme) {
    let popup = centered_rect(76, 18, area);
    let approval = app.pending_approval().expect("approval exists");
    let mut lines = approval_lines(&approval.call, &approval.reason);
    if let Some(prefix) = session_allow_preview(&approval.call) {
        lines.push(Line::from(Span::styled(
            format!("放行  本会话将自动允许：{prefix}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(title) = approval.source_title.as_deref() {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("来源  子 Agent · {title}"),
                Style::default().fg(Color::Cyan),
            )),
        );
    }
    let waited = approval.created_at.elapsed().as_secs();
    lines.push(Line::from(Span::styled(
        format!("已等待 {:02}:{:02}", waited / 60, waited % 60),
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Y 批准    N 拒绝    A 本会话总是允许    Esc 拒绝",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" 需要确认 ")
                    .borders(Borders::ALL)
                    .border_style(theme.style(VisualRole::Warning)),
            ),
        popup,
    );
}

fn approval_lines(call: &crate::provider::ToolCall, reason: &str) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("工具  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                tool_display_name(&call.name),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("风险  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                tool_risk(&call.name),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::default(),
    ];
    if let Some(arguments) = call.arguments.as_object() {
        for (key, value) in arguments.iter().take(7) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<10}", argument_label(key)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(human_argument(key, value)),
            ]));
        }
    } else {
        lines.push(Line::from("没有结构化参数"));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("原因  ", Style::default().fg(Color::DarkGray)),
        Span::raw(fit_text(reason, 58)),
    ]));
    lines
}

fn tool_risk(name: &str) -> &'static str {
    match name {
        // Read-only network lookup; no workspace or external side effect.
        "market_quote" | "web_search" | "web_fetch" | "webfetch" | "git_diff" => {
            "LOW - read-only lookup"
        }
        "file_delete" => "HIGH - removes workspace data",
        "terminal_shell" | "terminal_exec" | "git" => "HIGH - can change workspace state",
        "file_write" | "file_edit" | "file_move" | "file_copy" | "file_mkdir" => {
            "MEDIUM - changes workspace files"
        }
        value if value.starts_with("browser_") || value.starts_with("mcp:") => {
            "MEDIUM - external side effect"
        }
        _ => "LOW - review parameters",
    }
}

/// Human-readable "will always allow" preview shown on approval cards. Only
/// terminal_exec/git/terminal_shell show a command prefix; other tools allow
/// by exact name.
fn session_allow_preview(call: &crate::provider::ToolCall) -> Option<String> {
    match call.name.as_str() {
        "terminal_shell" => call
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("terminal_shell {command}")),
        "terminal_exec" => {
            let program = call
                .arguments
                .get("program")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if program.is_empty() {
                return None;
            }
            let mut parts = vec![program.to_owned()];
            if let Some(args) = call.arguments.get("args").and_then(Value::as_array) {
                parts.extend(
                    args.iter()
                        .filter_map(Value::as_str)
                        .take(8)
                        .map(str::to_owned),
                );
            }
            Some(format!("terminal_exec {}", parts.join(" ")))
        }
        "git" => {
            let subcommand = call
                .arguments
                .get("args")
                .and_then(Value::as_array)
                .and_then(|args| args.first())
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(format!("git {subcommand}"))
        }
        _ => None,
    }
}

fn argument_label(key: &str) -> &'static str {
    match key {
        "path" => "路径",
        "symbol" => "代码",
        "query" => "查询",
        "old_string" => "原文本",
        "new_string" => "新文本",
        "source" | "from" => "来源",
        "destination" | "to" => "目标",
        "command" => "命令",
        "args" => "参数",
        "cwd" => "目录",
        "url" => "URL",
        "content" => "内容",
        "max_bytes" => "最大大小",
        "timeout_seconds" => "超时",
        "prompt" => "任务",
        _ => "参数",
    }
}

fn human_argument(key: &str, value: &Value) -> String {
    let lower = key.to_ascii_lowercase();
    if lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("authorization")
    {
        return "[redacted]".into();
    }
    if key == "content" {
        return value
            .as_str()
            .map(|text| format!("{} bytes of text", text.len()))
            .unwrap_or_else(|| "structured content".into());
    }
    if key == "max_bytes" {
        return value
            .as_u64()
            .map(format_bytes)
            .unwrap_or_else(|| "default limit".into());
    }
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(|item| item.as_str().unwrap_or("[value]"))
            .collect::<Vec<_>>()
            .join(" "),
        Value::Bool(value) => {
            if *value {
                "yes".into()
            } else {
                "no".into()
            }
        }
        Value::Null => "未设置".into(),
        other => other.to_string(),
    };
    fit_text(&text, 58)
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// One row in the settings list: a section header, an editable field, or a
/// breathing-space spacer. Derived from the `FIELDS` registry.
enum SettingsRow {
    Section(&'static str),
    Field(SettingsField),
    /// TUI-only context-window override; the core DTO does not carry it, so it
    /// is edited through the same form and passed to `set_provider_profile`.
    ContextWindow,
    Spacer,
}

/// TUI row index of the synthetic context-window override. Rendered right
/// after Thinking, so it takes Thinking's following slot and ApiKey shifts by
/// one.
fn context_window_row() -> usize {
    FIELDS.len().saturating_sub(1)
}

/// TUI row index for a core field. ApiKey follows the synthetic row, so it is
/// shifted by one.
fn tui_field_row(field: SettingsField) -> usize {
    let index = FIELDS
        .iter()
        .position(|spec| spec.field == field)
        .unwrap_or(0);
    if index >= FIELDS.len().saturating_sub(1) {
        index + 1
    } else {
        index
    }
}

fn settings_rows() -> Vec<SettingsRow> {
    let mut rows = Vec::with_capacity(FIELDS.len() * 2 + 4);
    let mut last_section = None;
    for spec in FIELDS {
        if last_section != Some(spec.section) {
            last_section = Some(spec.section);
            rows.push(SettingsRow::Section(spec.section));
            rows.push(SettingsRow::Spacer);
        }
        rows.push(SettingsRow::Field(spec.field));
        if spec.field == SettingsField::Thinking {
            // The override sits with the other advanced knobs, immediately
            // after Thinking and before the write-only API key.
            rows.push(SettingsRow::ContextWindow);
        }
        rows.push(SettingsRow::Spacer);
    }
    rows
}

const SETTINGS_LABEL_COLUMNS: usize = 12;

fn draw_settings(
    frame: &mut Frame<'_>,
    area: Rect,
    settings: &SettingsState,
    app: &App,
    theme: &UiTheme,
) {
    match settings {
        SettingsState::List(list) => draw_provider_list(frame, area, list, theme),
        SettingsState::Templates(templates) => {
            draw_provider_templates(frame, area, templates, theme)
        }
        SettingsState::Form(form) => draw_provider_form(frame, area, form, app, theme),
    }
}

fn draw_provider_list(
    frame: &mut Frame<'_>,
    area: Rect,
    list: &crate::settings::ProviderList,
    theme: &UiTheme,
) {
    let popup = centered_rect(78, 20, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "  已连接的供应商",
            theme.strong(VisualRole::Accent),
        )),
        Line::default(),
    ];
    for (index, provider) in list.providers.iter().enumerate() {
        let selected = index == list.selected;
        let current = if provider.preset == list.active {
            " 当前"
        } else {
            ""
        };
        let status = if list.connected.contains(&provider.preset) {
            "已连接"
        } else {
            "需要 API Key"
        };
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {:<20} {:<26} {status}{current}",
                if selected { "›" } else { " " },
                provider.preset.label(),
                provider.model
            ),
            if selected {
                theme.selected
            } else {
                theme.style(VisualRole::Primary)
            },
        )));
    }
    let selected = list.selected == list.providers.len();
    lines.extend([
        Line::default(),
        Line::from(Span::styled(
            format!("  {} 添加供应商", if selected { "›" } else { " " }),
            if selected {
                theme.selected
            } else {
                theme.strong(VisualRole::Success)
            },
        )),
        Line::default(),
        Line::from(Span::styled(
            "  ↑/↓ 选择  Enter 编辑或添加  Esc 关闭",
            theme.style(VisualRole::Muted),
        )),
    ]);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 供应商连接 ")
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        popup,
    );
}

fn draw_provider_templates(
    frame: &mut Frame<'_>,
    area: Rect,
    templates: &crate::settings::TemplateList,
    theme: &UiTheme,
) {
    let popup = centered_rect(68, 18, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "  选择供应商模板",
            theme.strong(VisualRole::Accent),
        )),
        Line::default(),
    ];
    if templates.presets.is_empty() {
        lines.push(Line::from(Span::styled(
            "  所有供应商模板均已添加",
            theme.style(VisualRole::Muted),
        )));
    }
    for (index, preset) in templates.presets.iter().enumerate() {
        let selected = index == templates.selected;
        lines.push(Line::from(Span::styled(
            format!("  {} {}", if selected { "›" } else { " " }, preset.label()),
            if selected {
                theme.selected
            } else {
                theme.style(VisualRole::Primary)
            },
        )));
    }
    lines.extend([
        Line::default(),
        Line::from(Span::styled(
            "  ↑/↓ 选择  Enter 继续  Esc 返回",
            theme.style(VisualRole::Muted),
        )),
    ]);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" 添加供应商 ")
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        popup,
    );
}

fn draw_provider_form(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &SettingsForm,
    app: &App,
    theme: &UiTheme,
) {
    let popup = centered_rect(88, 24, area);
    let inner = Block::bordered().inner(popup);
    let footer_rows = 2usize;
    let visible = (inner.height as usize).saturating_sub(footer_rows);

    let rows = settings_rows();
    let selected_row = if app.settings_field_index == context_window_row() {
        rows.iter()
            .position(|row| matches!(row, SettingsRow::ContextWindow))
            .unwrap_or(0)
    } else {
        rows.iter()
            .position(|row| {
                matches!(
                    row,
                    SettingsRow::Field(field)
                        if tui_field_row(*field) == app.settings_field_index
                )
            })
            .unwrap_or(0)
    };
    let scroll = selected_row
        .saturating_sub(visible.saturating_sub(1))
        .min(rows.len().saturating_sub(visible));

    let value_width = inner
        .width
        .saturating_sub(SETTINGS_LABEL_COLUMNS as u16 + 7) as usize;
    let mut lines = Vec::with_capacity(visible + footer_rows);
    for row in rows.iter().skip(scroll).take(visible) {
        match row {
            SettingsRow::Section(section) => {
                let fill = inner
                    .width
                    .saturating_sub(UnicodeWidthStr::width(*section) as u16 + 6)
                    .min(24) as usize;
                lines.push(Line::from(Span::styled(
                    format!("  ━━ {section} {}", "━".repeat(fill)),
                    theme.strong(VisualRole::Accent),
                )));
            }
            SettingsRow::ContextWindow => {
                let selected = app.settings_field_index == context_window_row();
                let marker = if selected { "›" } else { " " };
                let label = "上下文窗口";
                let label_pad = " "
                    .repeat(SETTINGS_LABEL_COLUMNS.saturating_sub(UnicodeWidthStr::width(label)));
                let value = if app.context_window_input.trim().is_empty() {
                    "（继承）".to_owned()
                } else {
                    app.context_window_input.clone()
                };
                let label_style = if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                };
                let value_style = if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Secondary)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {marker} {label}{label_pad}"), label_style),
                    Span::styled(
                        format!("  {}", fit_text(value.as_str(), value_width)),
                        value_style,
                    ),
                ]));
            }
            SettingsRow::Field(field) => {
                let spec = FIELDS.iter().find(|spec| spec.field == *field).unwrap();
                let selected = app.settings_field_index == tui_field_row(*field);
                let marker = if selected { "›" } else { " " };
                let label_pad = " ".repeat(
                    SETTINGS_LABEL_COLUMNS.saturating_sub(UnicodeWidthStr::width(spec.label)),
                );
                let value = form.value(*field);
                let label_style = if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                };
                let value_style = if selected {
                    theme.selected
                } else {
                    theme.style(VisualRole::Secondary)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {marker} {}{label_pad}", spec.label), label_style),
                    Span::styled(
                        format!("  {}", fit_text(value.as_ref(), value_width)),
                        value_style,
                    ),
                ]));
            }
            SettingsRow::Spacer => lines.push(Line::default()),
        }
    }
    let divider = "─".repeat(inner.width.saturating_sub(2) as usize);
    lines.push(Line::from(Span::styled(
        format!("  {divider}"),
        theme.style(VisualRole::Muted),
    )));
    lines.push(Line::from(vec![
        Span::styled("  ↑/↓ 选择  ", theme.strong(VisualRole::Success)),
        Span::styled("←/→ 修改  ", theme.strong(VisualRole::Success)),
        Span::styled("Enter 保存  ", theme.strong(VisualRole::Success)),
        Span::styled("Ctrl+D 移除  ", theme.strong(VisualRole::Success)),
        Span::styled("Esc 返回", theme.strong(VisualRole::Success)),
    ]));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(" 编辑 {} ", form.provider.preset.label()))
                    .borders(Borders::ALL)
                    .border_style(theme.focus_border),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn draw_palette(frame: &mut Frame<'_>, area: Rect, palette: &CommandPaletteState, theme: &UiTheme) {
    let popup = centered_rect(84, 16, area);
    let matches = commands::matches(&palette.query, 10);
    let outer = Block::default()
        .title(" 命令面板 · Ctrl+P / Ctrl+X ")
        .borders(Borders::ALL)
        .border_style(theme.focus_border);
    let inner = outer.inner(popup);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let columns =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(rows[1]);
    let label_columns = commands::PALETTE_ITEMS
        .iter()
        .map(|item| UnicodeWidthStr::width(item.label))
        .max()
        .unwrap_or_default();
    let query = Line::from(vec![
        Span::styled("/", Style::default().fg(Color::Cyan)),
        Span::raw(palette.query.clone()),
    ]);
    let mut lines = Vec::new();
    for (index, item) in matches.iter().enumerate() {
        let selected = index == palette.selected;
        let style = if selected {
            theme.selected
        } else {
            theme.style(VisualRole::Primary)
        };
        let item = &commands::PALETTE_ITEMS[item.index];
        lines.push(Line::from(Span::styled(
            palette_item_text(item, selected, label_columns),
            style,
        )));
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "没有匹配的命令",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(outer, popup);
    frame.render_widget(Paragraph::new(query), rows[0]);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), columns[0]);

    let detail = matches
        .get(palette.selected)
        .map(|item| &commands::PALETTE_ITEMS[item.index])
        .map(|item| {
            let command = item.command.unwrap_or("直接动作");
            vec![
                Line::from(Span::styled(item.label, theme.strong(VisualRole::Success))),
                Line::default(),
                Line::from(item.description),
                Line::default(),
                Line::from(Span::styled(command, Style::default().fg(Color::Cyan))),
                Line::default(),
                Line::from(Span::styled(
                    "↑/↓ 选择 · Enter 执行 · Esc 关闭",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        })
        .unwrap_or_else(|| {
            vec![Line::from(Span::styled(
                "输入命令名或功能名称进行筛选",
                Style::default().fg(Color::DarkGray),
            ))]
        });
    frame.render_widget(
        Paragraph::new(detail)
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(theme.focus_border),
            )
            .wrap(Wrap { trim: true }),
        columns[1],
    );
}

fn palette_item_text(item: &commands::PaletteItem, selected: bool, label_columns: usize) -> String {
    let label_padding =
        " ".repeat(label_columns.saturating_sub(UnicodeWidthStr::width(item.label)));
    let command = item.command.unwrap_or("直接动作");
    let shortcut = item
        .shortcut
        .map(|shortcut| format!(" · {shortcut}"))
        .unwrap_or_default();
    format!(
        "{} {}{label_padding}  {command}{shortcut}",
        if selected { ">" } else { " " },
        item.label,
    )
}

fn draw_file_suggestions(frame: &mut Frame<'_>, input_area: Rect, app: &App) {
    let height = (app.file_suggestions.len() as u16 + 2).min(12);
    let popup = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width.min(64),
        height,
    };
    let items = app
        .file_suggestions
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let style = if index == app.file_selected {
                UiTheme::default().selected
            } else {
                UiTheme::default().style(VisualRole::Primary)
            };
            ListItem::new(Line::from(Span::styled(
                format!(
                    "{} @{value}",
                    if index == app.file_selected { ">" } else { " " }
                ),
                style,
            )))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, popup);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" 文件引用 ")
                .borders(Borders::ALL)
                .border_style(UiTheme::default().focus_border),
        ),
        popup,
    );
}

fn fit_text(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let target = width - 3;
    let mut used = 0;
    let mut output = String::new();
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > target {
            break;
        }
        output.push(character);
        used += character_width;
    }
    output + "..."
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let width = area
        .width
        .saturating_mul(width_percent)
        .saturating_div(100)
        .max(20)
        .min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

mod chat;
use chat::draw_messages;
pub(crate) use chat::tool_display_name;
#[cfg(test)]
use chat::{
    render_diff, render_markdown, render_thinking_summary, render_tool, render_visual_line,
    wrap_grapheme_lines,
};

mod sidebar;
#[cfg(test)]
use sidebar::session_window_start;
use sidebar::{draw_sessions, draw_todo_window};
pub(crate) use sidebar::{
    flatten_session_tree, session_index_at, todo_control_columns, todo_task_index_at_row,
};

#[cfg(test)]
mod tests;
