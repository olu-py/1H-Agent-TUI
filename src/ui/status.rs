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
    if !app.current.busy && app.provider_editor.is_none() && !app.has_pending_approval() {
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
    (app.provider_control_rect, app.model_control_rect) = provider_model_rects(area, view);
}

fn provider_model_rects(area: Rect, view: &UiViewModel) -> (Option<Rect>, Option<Rect>) {
    let Some(secondary) = view.footer.secondary.as_ref() else {
        return (None, None);
    };
    let right = clip_segments(&secondary.right, area.width as usize);
    let right_width = segment_width(&right);
    let left_budget = (area.width as usize)
        .saturating_sub(right_width.saturating_add(usize::from(right_width > 0)));
    let left = clip_segments_with_fit(&secondary.left, left_budget);
    if area.height < 2 {
        return (None, None);
    }
    // The painter lays these segments out from the footer's left edge, so
    // walking the same clipped output reproduces that layout exactly instead of
    // re-deriving offsets from the assembled text. Slot 0 is the provider pill
    // and slot 2 the model; a slot only becomes a control when its text was
    // painted whole.
    let mut column = area.x;
    let mut provider = None;
    let mut model = None;
    for (index, (segment, whole)) in left.iter().enumerate() {
        let width = UnicodeWidthStr::width(segment.text.as_str()) as u16;
        if *whole && width > 0 {
            let rect = Rect::new(column, area.y.saturating_add(1), width, 1);
            match index {
                0 => provider = Some(rect),
                2 => model = Some(rect),
                _ => {}
            }
        }
        column = column.saturating_add(width);
    }
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
    // Each row is formatted once (label column plus key state) and the widest
    // row sizes the popup, so painted text and hit-tested text cannot disagree
    // while the frame still grows upward from its own footer control.
    let rows = choices
        .iter()
        .map(|choice| {
            let connected = app
                .provider_settings
                .as_ref()
                .is_some_and(|settings| settings.connected.iter().any(|id| id == &choice.id));
            format!(
                "{:<14} {}",
                choice.label,
                if connected {
                    "已连接"
                } else {
                    "需要 API Key"
                }
            )
        })
        .collect::<Vec<_>>();
    // Each painted row is `"{marker} {label:<14} {state}"` inside a bordered
    // popup: the marker column and both borders are part of the budget, so the
    // widest row plus four is the width that shows a row unabridged.
    let content_width = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.as_str()))
        .max()
        .unwrap_or(18)
        .saturating_add(4) as u16;
    let width = content_width.clamp(20, 40).min(screen.width);
    let control = app
        .provider_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let selected = app
        .provider_menu_selected
        .min(choices.len().saturating_sub(1));
    let picker =
        PickerGeometry::new_with_action(screen, footer, control.x, width, choices.len(), selected);
    app.provider_menu_geometry = Some(picker);
    let items = rows
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(picker.visible)
        .map(|(index, row)| {
            let active = index == selected;
            ListItem::new(Line::from(Span::styled(
                format!("{} {}", if active { "›" } else { " " }, row),
                if active {
                    theme.selected
                } else {
                    theme.style(VisualRole::Primary)
                },
            )))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, picker.area);
    frame.render_widget(
        Block::default()
            .title(" 供应商 ↑↓ 选择 · Enter 应用 ")
            .borders(Borders::ALL)
            .border_style(theme.focus_border),
        picker.area,
    );
    // The list and the pinned action row share the popup's inner area, so the
    // item rows are drawn into exactly `visible` rows and the action row fills
    // the one the list was never given.
    let inner = picker.inner();
    frame.render_widget(
        List::new(items),
        Rect::new(inner.x, inner.y, inner.width, picker.visible as u16),
    );
    if let Some(action) = picker.action {
        frame.render_widget(provider_settings_row(action, theme), action);
    }
}

/// The provider picker's pinned entry into the full settings panel. Painted and
/// hit-tested from the same geometry slot, so the row that advertises `Ctrl+S`
/// is the row a click resolves.
pub(super) fn provider_settings_row(action: Rect, theme: &UiTheme) -> Paragraph<'static> {
    const LABEL: &str = "⚙ 供应商设置…";
    const SHORTCUT: &str = "Ctrl+S";
    let gap = (action.width as usize).saturating_sub(
        UnicodeWidthStr::width(LABEL)
            .saturating_add(UnicodeWidthStr::width(SHORTCUT))
            .saturating_add(2),
    );
    Paragraph::new(Line::from(vec![
        Span::styled(format!(" {LABEL}"), theme.style(VisualRole::Accent)),
        Span::styled(" ".repeat(gap), theme.style(VisualRole::Muted)),
        Span::styled(SHORTCUT, theme.strong(VisualRole::Shortcut)),
        Span::styled(" ", theme.style(VisualRole::Muted)),
    ]))
}

pub(super) fn draw_model_menu(
    frame: &mut Frame<'_>,
    screen: Rect,
    footer: Rect,
    app: &mut App,
    theme: &UiTheme,
) {
    let choices = crate::app::model_choices(app);
    let labels = choices
        .iter()
        .map(|choice| choice.label.as_str())
        .collect::<Vec<_>>();
    let content_width = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(*label))
        .max()
        .unwrap_or(12)
        .saturating_add(3) as u16;
    let width = content_width.clamp(22, 52).min(screen.width);
    let control = app
        .model_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let selected = app.model_menu_selected.min(choices.len().saturating_sub(1));
    let picker = PickerGeometry::new(screen, footer, control.x, width, choices.len(), selected);
    app.model_menu_geometry = Some(picker);
    let items = labels
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(picker.visible)
        .map(|(index, label)| {
            let active = index == selected;
            ListItem::new(Line::from(Span::styled(
                format!("{} {}", if active { "›" } else { " " }, label),
                if active {
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
    frame.render_widget(Clear, picker.area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(format!(
                    " {} 模型 · r 刷新{status} ↑↓ Enter ",
                    app.provider_label()
                ))
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        picker.area,
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
    let rows = thinking_menu_rows(&view.columns);
    let levels = view.columns.first().map_or(&[][..], |column| &column.cells);
    let budgets = view.columns.get(1).map_or(&[][..], |column| &column.cells);
    // A row is the level cell padded to exactly the column width the hit-test
    // splits on, followed by the Qwen3.7 budget cell. Deriving the text once
    // for the width and again for the spans keeps painter and hit-test in
    // lockstep instead of approximating the same layout twice.
    let parts = (0..rows)
        .map(|index| {
            let level = levels.get(index).map_or(String::new(), |cell| {
                format!("{} {}", if cell.active { "●" } else { "○" }, cell.label)
            });
            let budget = budgets.get(index).map_or(String::new(), |cell| {
                format!(" {} {}", if cell.active { "●" } else { "○" }, cell.label)
            });
            (level, budget)
        })
        .collect::<Vec<_>>();
    let content_width = parts
        .iter()
        .map(|(level, budget)| {
            UnicodeWidthStr::width(pad_cells(level, THINKING_LEVEL_COLUMN_WIDTH).as_str())
                + UnicodeWidthStr::width(budget.as_str())
        })
        .max()
        .unwrap_or(14)
        .saturating_add(3) as u16;
    let width = content_width.clamp(16, 30).min(screen.width);
    let control = app
        .thinking_control_rect
        .unwrap_or(Rect::new(footer.x, footer.y, 0, 0));
    let cursor = app
        .thinking_menu_cursor
        .clamped(rows, view.columns.len().max(1));
    let picker = PickerGeometry::new(
        screen,
        footer,
        control.right().saturating_sub(width),
        width,
        rows,
        cursor.row,
    );
    app.thinking_menu_geometry = Some(picker);
    let plain = theme.style(VisualRole::Primary);
    let lines = (picker.scroll..picker.scroll.saturating_add(picker.visible))
        .filter_map(|index| parts.get(index).map(|part| (index, part)))
        .map(|(index, (level, budget))| {
            Line::from(vec![
                Span::styled(
                    pad_cells(level, THINKING_LEVEL_COLUMN_WIDTH),
                    if cursor.column == 0 && cursor.row == index {
                        theme.selected
                    } else {
                        plain
                    },
                ),
                Span::styled(
                    budget.clone(),
                    if cursor.column == 1 && cursor.row == index {
                        theme.selected
                    } else {
                        plain
                    },
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, picker.area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(if budgets.is_empty() {
                    " 思考强度 ↑↓ Enter Esc "
                } else {
                    " 思考强度 ↑↓←→ Enter Esc "
                })
                .border_style(theme.style(VisualRole::Accent)),
        ),
        picker.area,
    );
}

/// Pads or truncates `text` to exactly `width` terminal cells so the column
/// after it starts where the picker hit-test expects.
fn pad_cells(text: &str, width: u16) -> String {
    let budget = usize::from(width);
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used + grapheme_width > budget {
            break;
        }
        output.push_str(grapheme);
        used += grapheme_width;
    }
    output.push_str(&" ".repeat(budget - used));
    output
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
    clip_segments_with_fit(segments, width)
        .into_iter()
        .map(|(segment, _)| segment)
        .collect()
}

/// [`clip_segments`] plus, per painted segment, whether it was painted whole.
///
/// The painter and every hit-test read this one function, so a control's
/// rectangle is always derived from the text the frame actually drew. A segment
/// cut short by the width budget reports `false` and therefore gets no
/// rectangle: a clipped label must never open a picker.
fn clip_segments_with_fit(segments: &[UiSegment], width: usize) -> Vec<(UiSegment, bool)> {
    let mut output = Vec::new();
    let mut remaining = width;
    for segment in segments {
        if remaining == 0 {
            break;
        }
        let segment_width = UnicodeWidthStr::width(segment.text.as_str());
        if segment_width <= remaining {
            output.push((segment.clone(), true));
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
            output.push((
                UiSegment {
                    text,
                    role: segment.role,
                },
                false,
            ));
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
