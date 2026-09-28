use super::*;

use crate::app::{EDITOR_FIELDS, EditorAction, EditorField, EditorPane, ProviderEditor};

pub(super) fn draw_approval(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &UiTheme) {
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

pub(super) fn approval_lines(call: &crate::provider::ToolCall, reason: &str) -> Vec<Line<'static>> {
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

pub(super) fn argument_label(key: &str) -> &'static str {
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

pub(super) fn human_argument(key: &str, value: &Value) -> String {
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

/// Width of the label column in the editor's field pane.
const EDITOR_LABEL_COLUMNS: usize = 12;

/// The widest status the current rows can print. The provider pane reserves
/// exactly this column and gives the label the rest, so a longer badge (for
/// example the `当前 已配置 ●` row, which is one cell wider than
/// `需要 API Key`) costs the label characters instead of moving the divider.
fn provider_status_columns(editor: &ProviderEditor) -> usize {
    editor
        .rows
        .iter()
        .map(|row| {
            let badge = row.badge();
            if badge.is_empty() {
                UnicodeWidthStr::width(row.key_state())
            } else {
                UnicodeWidthStr::width(badge.as_str())
            }
        })
        .max()
        .unwrap_or(0)
}

/// The provider panel: the providers on the left, the draft's fields on the
/// right, and a pinned action row.
///
/// Every clickable row is painted and hit-tested in this one pass: the
/// rectangles recorded here are the rows the frame actually drew, so a scrolled
/// or clipped row can never resolve a click it does not show.
pub(super) fn draw_provider_editor(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    editor: &mut ProviderEditor,
    theme: &UiTheme,
) {
    let popup = centered_rect(92, 26, area);
    let inner = Block::bordered().inner(popup);
    editor.rect = popup;
    editor.row_rects = vec![Rect::default(); editor.rows.len() + 1];
    editor.field_rects = vec![Rect::default(); EDITOR_FIELDS.len()];
    editor.action_rects.clear();
    editor.model_picker_rect = None;

    let footer_rows = 2u16.min(inner.height);
    let body_height = inner.height.saturating_sub(footer_rows) as usize;
    // Two panes need room for a label column *and* a value on each side. Below
    // that the panel stacks them and shows the pane that holds the keyboard,
    // rather than shrinking both into unusable slivers.
    let two_pane = inner.width >= 88;
    let (left_width, right_x, right_width) = if two_pane {
        let left = (inner.width * 2 / 5)
            .clamp(34, 44)
            .min(inner.width.saturating_sub(40));
        (
            left,
            inner.x.saturating_add(left).saturating_add(3),
            inner.width - left - 3,
        )
    } else {
        (inner.width, inner.x, inner.width)
    };
    let show_left = two_pane || editor.pane == EditorPane::Providers;
    let show_right = two_pane || editor.pane == EditorPane::Fields;

    let label_columns = (left_width as usize)
        .saturating_sub(4 + provider_status_columns(editor))
        .max(8);
    let left_lines = provider_row_lines(editor, theme, label_columns);
    let right_lines = editor_field_lines(app, editor, theme, right_width);

    let left_start = window_start(editor.selected_row, left_lines.len(), body_height);
    let right_focus = right_lines
        .iter()
        .position(|(field, _)| *field == Some(editor.field_index))
        .unwrap_or(0);
    let right_start = window_start(right_focus, right_lines.len(), body_height);

    // Each pane is rendered into its own rectangle. A row wider than its pane
    // therefore clips inside that pane instead of shifting the divider between
    // them — composing both panes into one line could only be kept aligned by
    // padding, and a row one cell too wide silently broke it.
    let mut left_visible: Vec<Line<'static>> = Vec::with_capacity(body_height);
    let mut right_visible: Vec<Line<'static>> = Vec::with_capacity(body_height);
    for row in 0..body_height {
        if show_left {
            let index = left_start + row;
            match left_lines.get(index) {
                Some(line) => {
                    editor.row_rects[index] =
                        Rect::new(inner.x, inner.y + row as u16, left_width, 1);
                    left_visible.push(Line::from(line.clone()));
                }
                None => left_visible.push(Line::default()),
            }
        }
        if show_right {
            let index = right_start + row;
            match right_lines.get(index) {
                Some((field, line)) => {
                    if let Some(field) = field {
                        editor.field_rects[*field] =
                            Rect::new(right_x, inner.y + row as u16, right_width, 1);
                    }
                    right_visible.push(Line::from(line.clone()));
                }
                None => right_visible.push(Line::default()),
            }
        }
    }

    let divider_row = inner.y.saturating_add(body_height as u16);
    let mut footer: Vec<Line<'static>> = Vec::new();
    footer.push(Line::from(Span::styled(
        format!("  {}", "─".repeat(inner.width.saturating_sub(2) as usize)),
        theme.style(VisualRole::Muted),
    )));
    if editor.delete_confirm {
        footer.push(Line::from(vec![
            Span::styled("  确认删除？", theme.strong(VisualRole::Warning)),
            Span::styled(
                "该供应商的 API Key 会保留在系统钥匙串中  ",
                theme.style(VisualRole::Muted),
            ),
            Span::styled("y 确认", theme.strong(VisualRole::Warning)),
            Span::styled("    ", theme.style(VisualRole::Muted)),
            Span::styled("n / Esc 取消", theme.strong(VisualRole::Shortcut)),
        ]));
    } else {
        footer.push(Line::from(action_row(
            editor,
            theme,
            divider_row.saturating_add(1),
            inner,
        )));
    }

    // A clear one cell *beyond* the panel, not just inside it: a wide grapheme
    // painted by whatever sits underneath the panel makes ratatui skip the cell
    // that follows it when it diffs the frame, so the panel's border cell beside
    // it would keep the previous screen's half-glyph for as long as that widget
    // keeps redrawing the character. Erasing the neighbour leaves a narrow blank
    // there, which lets the border be diffed again.
    frame.render_widget(Clear, expand_within(popup, 1, area));
    let body_area = Rect::new(inner.x, inner.y, inner.width, body_height as u16);
    if show_left {
        frame.render_widget(
            Paragraph::new(left_visible),
            Rect::new(inner.x, inner.y, left_width, body_height as u16),
        );
    }
    if two_pane {
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(theme.style(VisualRole::Muted)),
            Rect::new(
                inner.x.saturating_add(left_width).saturating_add(1),
                inner.y,
                1,
                body_height as u16,
            ),
        );
    }
    if show_right {
        frame.render_widget(
            Paragraph::new(right_visible),
            Rect::new(right_x, inner.y, right_width, body_height as u16),
        );
    }
    frame.render_widget(
        Paragraph::new(footer),
        Rect::new(inner.x, divider_row, inner.width, footer_rows),
    );
    // The frame is painted last so neither pane's content can ever overwrite a
    // border cell, whatever a row happens to contain.
    frame.render_widget(
        Block::default()
            .title(format!(" 供应商管理 · {} ", editor_title(editor)))
            .borders(Borders::ALL)
            .border_style(theme.focus_border),
        popup,
    );

    // The picker floats over the body only: anchoring it to the panel bottom
    // would paint its frame across the action row the user needs to reach.
    draw_editor_model_picker(frame, body_area, app, editor, theme);
}

/// `rect` grown by `margin` cells on every side, clamped to `bounds`.
fn expand_within(rect: Rect, margin: u16, bounds: Rect) -> Rect {
    let x = rect.x.saturating_sub(margin).max(bounds.x);
    let y = rect.y.saturating_sub(margin).max(bounds.y);
    let right = rect.right().saturating_add(margin).min(bounds.right());
    let bottom = rect.bottom().saturating_add(margin).min(bounds.bottom());
    Rect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

/// The panel's title names the profile the *fields* are editing. It deliberately
/// does not follow the cursor row: moving the selection does not load a draft
/// until `Enter`, so a title bound to the selection would name one provider while
/// the pane beside it still showed another.
fn editor_title(editor: &ProviderEditor) -> String {
    if editor.creating() {
        return "新建自定义供应商".to_owned();
    }
    editor.form.provider.display_label().to_owned()
}

/// The pinned action row plus the key hints that fit beside it. Each action
/// records its own rectangle as it is laid out, so the painted word and the
/// clickable area are the same span.
fn action_row(
    editor: &mut ProviderEditor,
    theme: &UiTheme,
    row: u16,
    inner: Rect,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut column = inner.x.saturating_add(2);
    for (index, action) in [
        EditorAction::Apply,
        EditorAction::Delete,
        EditorAction::Cancel,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled("  ", theme.style(VisualRole::Muted)));
            column = column.saturating_add(2);
        }
        let label = format!(" {} ", action.label());
        let width = UnicodeWidthStr::width(label.as_str()) as u16;
        let enabled = action != EditorAction::Apply || editor.blocked_reason().is_none();
        spans.push(Span::styled(
            label,
            if enabled {
                theme.strong(VisualRole::Success)
            } else {
                theme.style(VisualRole::Muted)
            },
        ));
        editor
            .action_rects
            .push((action, Rect::new(column, row, width, 1)));
        column = column.saturating_add(width);
    }
    let hints = "Tab 切换窗格  ↑/↓ 选择  ←/→ 修改  Enter 打开  Ctrl+S 应用  Ctrl+R 刷新  Ctrl+G 取窗口  Ctrl+D 删除  Esc 返回";
    let free = inner
        .right()
        .saturating_sub(column)
        .saturating_sub(UnicodeWidthStr::width(hints) as u16 + 1) as usize;
    if free > 0 {
        spans.push(Span::styled(
            format!("  {hints}"),
            theme.style(VisualRole::Muted),
        ));
    }
    spans
}

fn provider_row_lines(
    editor: &ProviderEditor,
    theme: &UiTheme,
    label_columns: usize,
) -> Vec<Vec<Span<'static>>> {
    // ` marker label gap status` fills the pane exactly, so the status keeps its
    // own column and the divider column stays put whichever status a row carries.
    let mut lines = Vec::with_capacity(editor.rows.len() + 1);
    for (index, row) in editor.rows.iter().enumerate() {
        let selected = index == editor.selected_row && editor.pane == EditorPane::Providers;
        let label = fit_text(&row.label, label_columns);
        let pad = " ".repeat(label_columns.saturating_sub(UnicodeWidthStr::width(label.as_str())));
        let badge = row.badge();
        let status = if badge.is_empty() {
            row.key_state().to_owned()
        } else {
            badge
        };
        let role = if row.active {
            VisualRole::Accent
        } else if row.connected {
            VisualRole::Primary
        } else {
            VisualRole::Warning
        };
        lines.push(vec![
            Span::styled(
                format!(" {} {} ", if selected { "›" } else { " " }, label),
                if selected {
                    theme.selected
                } else {
                    theme.style(role)
                },
            ),
            Span::styled(pad, theme.style(VisualRole::Muted)),
            Span::styled(
                status,
                if selected {
                    theme.selected
                } else {
                    theme.style(if row.connected {
                        VisualRole::Success
                    } else {
                        VisualRole::Warning
                    })
                },
            ),
        ]);
    }
    // The create command is the last address, one past the last profile.
    let selected = editor.on_add_row() && editor.pane == EditorPane::Providers;
    lines.push(vec![Span::styled(
        format!(" {} ＋ 添加自定义供应商", if selected { "›" } else { " " }),
        if selected {
            theme.selected
        } else {
            theme.strong(VisualRole::Success)
        },
    )]);
    lines
}

/// The field pane, one entry per painted line. `Some(index)` marks the lines a
/// click can focus; the context-window hint and the error line are informational
/// and therefore address no field.
fn editor_field_lines(
    app: &App,
    editor: &ProviderEditor,
    theme: &UiTheme,
    width: u16,
) -> Vec<(Option<usize>, Vec<Span<'static>>)> {
    let mut lines: Vec<(Option<usize>, Vec<Span<'static>>)> = Vec::new();
    for (index, field) in EDITOR_FIELDS.iter().enumerate() {
        let focused = index == editor.field_index && editor.pane == EditorPane::Fields;
        let label = field.label();
        let pad = " ".repeat(EDITOR_LABEL_COLUMNS.saturating_sub(UnicodeWidthStr::width(label)));
        let value_style = if focused {
            theme.selected
        } else {
            theme.style(VisualRole::Secondary)
        };
        let hint = theme.strong(VisualRole::Shortcut);
        let mut spans = vec![Span::styled(
            format!(" {} {label}{pad}  ", if focused { "›" } else { " " }),
            if focused {
                theme.selected
            } else {
                theme.style(VisualRole::Primary)
            },
        )];
        match field {
            EditorField::Name => {
                let name = editor.form.provider.name.trim();
                let (text, role) = if name.is_empty() && editor.creating() {
                    ("必填：留空则无法创建".to_owned(), VisualRole::Warning)
                } else if name.is_empty() {
                    ("（内置/未命名）".to_owned(), VisualRole::Muted)
                } else {
                    (name.to_owned(), VisualRole::Secondary)
                };
                spans.push(Span::styled(
                    text,
                    if focused {
                        theme.selected
                    } else {
                        theme.style(role)
                    },
                ));
            }
            EditorField::Template => {
                spans.push(Span::styled(
                    editor.form.provider.preset.label().to_owned(),
                    value_style,
                ));
                spans.push(Span::styled(
                    "（只读）".to_owned(),
                    theme.style(VisualRole::Muted),
                ));
            }
            EditorField::ContextWindow => {
                spans.push(Span::styled(editor.window_value(app), value_style));
                if focused && editor.window_fetch_available(app) {
                    spans.push(Span::styled("  Ctrl+G 获取".to_owned(), hint));
                }
            }
            EditorField::ApiKey => {
                spans.push(Span::styled(
                    editor.form.value(SettingsField::ApiKey).into_owned(),
                    value_style,
                ));
                let (state, role) = if editor.form.has_existing_key() {
                    ("已配置", VisualRole::Success)
                } else {
                    ("未配置", VisualRole::Warning)
                };
                spans.push(Span::styled(
                    format!("  {state}"),
                    if focused {
                        theme.selected
                    } else {
                        theme.style(role)
                    },
                ));
                spans.push(Span::styled(
                    "（只写）".to_owned(),
                    theme.style(VisualRole::Muted),
                ));
            }
            _ => {
                // The Model row shows the window the provider reported for the
                // value, which the core form's plain field text does not carry.
                let value = if *field == EditorField::Model {
                    editor.model_value(app)
                } else {
                    let core = field.core().expect("core-owned field row");
                    editor.form.value(core).into_owned()
                };
                spans.push(Span::styled(value, value_style));
                if focused {
                    match field {
                        EditorField::Model => spans.push(Span::styled("  Enter".to_owned(), hint)),
                        EditorField::Protocol | EditorField::Thinking => {
                            spans.push(Span::styled("  ←/→".to_owned(), hint));
                        }
                        _ => {}
                    }
                }
            }
        }
        lines.push((Some(index), spans));
        if *field == EditorField::ContextWindow
            && let Some(note) = editor.window_hint(app)
        {
            let role = if editor.window_fetch_pending {
                VisualRole::Warning
            } else {
                VisualRole::Muted
            };
            for chunk in wrap_cells(&note, width.saturating_sub(5) as usize) {
                lines.push((
                    None,
                    vec![Span::styled(format!("   {chunk}"), theme.style(role))],
                ));
            }
        }
    }
    if let Some(error) = &editor.error {
        for chunk in wrap_cells(error, width.saturating_sub(5) as usize) {
            lines.push((
                None,
                vec![Span::styled(
                    format!("   {chunk}"),
                    theme.strong(VisualRole::Warning),
                )],
            ));
        }
    }
    lines
}

/// The in-panel model picker, anchored to the model row's column and floating
/// inside the panel. It is drawn last so it overlays the body it was opened
/// from, and it records the geometry the next hit-test reads.
fn draw_editor_model_picker(
    frame: &mut Frame<'_>,
    body: Rect,
    app: &App,
    editor: &mut ProviderEditor,
    theme: &UiTheme,
) {
    if !editor.model_picker.open {
        editor.model_picker.geometry = None;
        return;
    }
    let model_field = EDITOR_FIELDS
        .iter()
        .position(|field| *field == EditorField::Model)
        .unwrap_or(0);
    let anchor = editor.field_rects[model_field];
    let choices = editor.model_choices(app);
    // Items plus the trailing "type it myself" row.
    let items = choices.len() + 1;
    let content = choices
        .iter()
        .map(|choice| {
            UnicodeWidthStr::width(choice.label.as_str())
                + choice
                    .max_output
                    .map(|out| grouped_tokens(out).len() + 7)
                    .unwrap_or(0)
        })
        .max()
        .unwrap_or(18)
        .max(UnicodeWidthStr::width("（自定义模型名…）"))
        .saturating_add(6) as u16;
    let width = content.min(body.width).max(28);
    let anchor_x = if anchor.height > 0 { anchor.x } else { body.x };
    let geometry =
        PickerGeometry::floating(body, anchor_x, width, items, editor.model_picker.selected);
    editor.model_picker.geometry = Some(geometry);
    editor.model_picker_rect = Some(geometry.area);

    let rows = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let active = index == editor.model_picker.selected;
            let mut text = format!("{} {}", if active { "›" } else { " " }, choice.label);
            if active && let Some(out) = choice.max_output {
                text.push_str(&format!("   {} out", grouped_tokens(out)));
            }
            (index, text, active)
        })
        .chain(std::iter::once((
            choices.len(),
            format!(
                "{} （自定义模型名…）",
                if editor.model_picker.selected == choices.len() {
                    "›"
                } else {
                    " "
                }
            ),
            editor.model_picker.selected == choices.len(),
        )))
        .collect::<Vec<_>>();
    let items = rows
        .into_iter()
        .enumerate()
        .filter(|(index, _)| {
            *index >= geometry.scroll && *index < geometry.scroll + geometry.visible
        })
        .map(|(_, (_, text, active))| {
            ListItem::new(Line::from(Span::styled(
                text,
                if active {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                },
            )))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, geometry.area);
    frame.render_widget(
        Block::default()
            .title(" 选择模型 ↑↓ Enter Esc · Ctrl+R ")
            .borders(Borders::ALL)
            .border_style(theme.focus_border),
        geometry.area,
    );
    let inner = geometry.inner();
    frame.render_widget(
        List::new(items),
        Rect::new(inner.x, inner.y, inner.width, geometry.visible as u16),
    );
}

/// First painted row of a window that keeps `selected` visible.
fn window_start(selected: usize, len: usize, height: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    selected
        .saturating_sub(height.saturating_sub(1))
        .min(len - height)
}

/// Breaks `text` into chunks of at most `width` cells on grapheme boundaries.
/// Panel body rows are composed from both panes, so a wrapped hint has to become
/// separate rows rather than let the paragraph reflow the alignment.
fn wrap_cells(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let cell = UnicodeWidthStr::width(grapheme);
        if used + cell > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(grapheme);
        used += cell;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Thousands separator, so a token budget reads as `8,192` rather than `8192`.
fn grouped_tokens(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(character);
    }
    out
}

pub(super) fn draw_palette(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &CommandPaletteState,
    theme: &UiTheme,
) {
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

pub(super) fn palette_item_text(
    item: &commands::PaletteItem,
    selected: bool,
    label_columns: usize,
) -> String {
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

pub(super) fn draw_file_suggestions(frame: &mut Frame<'_>, input_area: Rect, app: &App) {
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

pub(super) fn fit_text(value: &str, width: usize) -> String {
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

pub(super) fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
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
