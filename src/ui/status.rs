use super::*;

pub(super) fn input_mode_rect(area: Rect, mode: AgentMode) -> Option<Rect> {
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

pub(super) fn draw_input(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    view: &InputView,
    theme: &UiTheme,
) {
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

pub(super) fn draw_footer(
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

pub(super) fn draw_provider_menu(
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

pub(super) fn draw_model_menu(
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

pub(super) fn draw_thinking_menu(
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
