use super::*;

const SESSIONS_TITLE_OFFSET: u16 = 1;
const SESSIONS_HEADER_ROWS: u16 = 4;
const SESSIONS_TRAILING_ROWS: u16 = 1;

/// Number of session rows visible below the panel title and the four-row
/// header. The `List` block has a title, so its inner area starts one row lower
/// than the panel rect.
fn session_visible_slots(area_height: u16) -> usize {
    area_height
        .saturating_sub(SESSIONS_TITLE_OFFSET + SESSIONS_HEADER_ROWS + SESSIONS_TRAILING_ROWS)
        as usize
}

/// A flattened, depth-annotated session for tree rendering and hit-testing.
pub(crate) struct SessionRow {
    pub id: String,
    pub title: String,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// Flattens the session list into depth-first tree order, dropping the
/// children of collapsed parents. Roots (`parent_id == None`) come first, each
/// followed by its visible descendants. Parents are collapsed by default and
/// only expand when the user clicks them; the active session does not force an
/// expansion.
pub(crate) fn flatten_session_tree(
    sessions: &[SessionSummary],
    expanded: &HashSet<String>,
) -> Vec<SessionRow> {
    let mut rows = Vec::new();
    for root in sessions
        .iter()
        .filter(|session| session.parent_id.is_none())
    {
        push_session_tree(root, 0, sessions, expanded, &mut rows);
    }
    rows
}

fn push_session_tree(
    session: &SessionSummary,
    depth: usize,
    all: &[SessionSummary],
    expanded: &HashSet<String>,
    rows: &mut Vec<SessionRow>,
) {
    let has_children = all
        .iter()
        .any(|child| child.parent_id.as_deref() == Some(session.id.as_str()));
    let is_expanded = has_children && expanded.contains(&session.id);
    rows.push(SessionRow {
        id: session.id.clone(),
        title: session.title.clone(),
        depth,
        has_children,
        expanded: is_expanded,
    });
    if is_expanded {
        for child in all
            .iter()
            .filter(|child| child.parent_id.as_deref() == Some(session.id.as_str()))
        {
            push_session_tree(child, depth + 1, all, expanded, rows);
        }
    }
}

/// Window start such that the current session stays visible, pinned to the
/// bottom slot whenever the list has scrolled. Shared by rendering and hit
/// testing so a click always maps to the same session that was drawn.
pub(super) fn session_window_start(total: usize, current: usize, visible_slots: usize) -> usize {
    current
        .saturating_add(1)
        .saturating_sub(visible_slots)
        .min(total.saturating_sub(visible_slots))
}

/// Maps a mouse position inside the sessions panel to the session list index,
/// or `None` when the position is outside the panel, in the header, or in the
/// trailing blank rows.
pub(crate) fn session_index_at(
    area: Rect,
    column: u16,
    row: u16,
    total: usize,
    current: usize,
) -> Option<usize> {
    if area.width < 2 {
        return None;
    }
    let list_top = area.y.saturating_add(SESSIONS_TITLE_OFFSET);
    let list_right = area.right().saturating_sub(1);
    if column < area.x || column >= list_right || row < list_top || row >= area.bottom() {
        return None;
    }
    let header_end = list_top.saturating_add(SESSIONS_HEADER_ROWS);
    if row < header_end {
        return None;
    }
    let visible_slots = session_visible_slots(area.height);
    if visible_slots == 0 {
        return None;
    }
    let offset = row.saturating_sub(header_end) as usize;
    if offset >= visible_slots {
        return None;
    }
    let start = session_window_start(total, current, visible_slots);
    let index = start.saturating_add(offset);
    (index < total).then_some(index)
}

pub(super) fn draw_sessions(frame: &mut Frame<'_>, area: Rect, app: &mut App, theme: &UiTheme) {
    app.session_panel_rect = Some(area);
    let workspace = app
        .workspace
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| app.workspace.to_string_lossy());
    let content_width = area.width.saturating_sub(3) as usize;
    let mut items = vec![
        ListItem::new(Line::from(Span::styled(
            "Alt+Up/Down  切换会话",
            theme.style(VisualRole::Muted),
        ))),
        ListItem::new(Line::from(Span::styled(
            "Ctrl+N       新建会话",
            theme.style(VisualRole::Muted),
        ))),
        ListItem::new(Line::default()),
        ListItem::new(Line::from(Span::styled(
            fit_text(&workspace, content_width),
            theme.strong(VisualRole::Accent),
        ))),
    ];
    let visible_sessions = session_visible_slots(area.height);
    let rows = flatten_session_tree(&app.sessions, &app.expanded_sessions);
    let current = rows
        .iter()
        .position(|row| row.id == app.current.session_id)
        .unwrap_or(0);
    let start = session_window_start(rows.len(), current, visible_sessions);
    for row in rows.iter().skip(start).take(visible_sessions) {
        let active = row.id == app.current.session_id;
        let arrow = if row.has_children {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let indent = "  ".repeat(row.depth);
        let marker = if active { ">" } else { " " };
        let child_progress = app.child_status.get(&row.id);
        let waiting_approval = app.session_waiting_approval(&row.id);
        let status_text = match (child_progress, waiting_approval) {
            (_, true) => " ⏳审批".to_owned(),
            (Some(progress), false) if progress.status == ChildSessionStatus::Completed => {
                String::new()
            }
            (Some(progress), false) => format!(" ·{}", progress.label()),
            (None, false) => String::new(),
        };
        let style = if active {
            theme.selected
        } else if row.has_children {
            theme.strong(VisualRole::Accent)
        } else {
            theme.style(VisualRole::Primary)
        };
        let title = fit_text(
            &row.title,
            content_width.saturating_sub(
                row.depth
                    .saturating_mul(2)
                    .saturating_add(4)
                    .saturating_add(
                        status_text
                            .chars()
                            .map(|character| UnicodeWidthChar::width(character).unwrap_or(0))
                            .sum::<usize>(),
                    ),
            ),
        );
        items.push(ListItem::new(Line::from(Span::styled(
            format!("{indent}{arrow}{marker} {title}{status_text}"),
            style,
        ))));
    }
    frame.render_widget(
        List::new(items).block(Block::default().title(" 会话 ").borders(Borders::RIGHT)),
        area,
    );
}

pub(crate) const TODO_WINDOW_MAX_WIDTH: u16 = 44;

pub(crate) fn todo_window_rect(viewport: Rect, task_count: usize, collapsed: bool) -> Option<Rect> {
    let minimum_height = if collapsed || task_count == 1 { 3 } else { 4 };
    if task_count == 0 || viewport.width < 4 || viewport.height < minimum_height {
        return None;
    }
    let width = TODO_WINDOW_MAX_WIDTH.min(viewport.width);
    let desired_height = if collapsed {
        3
    } else {
        task_count.saturating_add(2).min(usize::from(u16::MAX))
    };
    let height = desired_height.min(usize::from(viewport.height)) as u16;
    if height < minimum_height {
        return None;
    }
    Some(Rect {
        x: viewport.right().saturating_sub(width),
        y: viewport.bottom().saturating_sub(height),
        width,
        height,
    })
}

pub(super) fn draw_todo_window(
    frame: &mut Frame<'_>,
    viewport: Rect,
    app: &mut App,
    theme: &UiTheme,
) {
    if app.current.todos.is_empty() || app.current.todo_hidden {
        app.todo_window_rect = None;
        return;
    }
    let Some(rect) = todo_window_rect(
        viewport,
        app.current.todos.len(),
        app.current.todo_collapsed,
    ) else {
        app.todo_window_rect = None;
        return;
    };
    app.todo_window_rect = Some(rect);
    let todo = TodoDisplay {
        tasks: app.current.todos.clone(),
    };
    let lines = todo_window_lines(
        &todo,
        usize::from(rect.width.saturating_sub(2)),
        usize::from(rect.height.saturating_sub(2)),
        app.current.todo_collapsed,
    );
    let (done, total) = todo.progress();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(format!(" 任务清单 {done}/{total} "))
                .borders(Borders::ALL)
                .border_style(theme.focus_border),
        ),
        rect,
    );
    if let Some((toggle_column, _)) = todo_control_columns(rect) {
        let controls = if app.current.todo_collapsed {
            "▾  ×"
        } else {
            "▴  ×"
        };
        frame.render_widget(
            Paragraph::new(controls).style(theme.focus_border),
            Rect::new(toggle_column, rect.y, 4, 1),
        );
    }
}

pub(crate) fn todo_control_columns(rect: Rect) -> Option<(u16, u16)> {
    if rect.width < 7 {
        return None;
    }
    let toggle = rect.right().saturating_sub(6);
    Some((toggle, toggle + 3))
}

pub(crate) fn todo_visible_task_count(task_count: usize, visible_rows: usize) -> usize {
    if task_count > visible_rows {
        visible_rows.saturating_sub(1)
    } else {
        task_count
    }
}

pub(crate) fn todo_task_index_at_row(
    tasks: &[TodoTask],
    visible_rows: usize,
    content_row: usize,
    collapsed: bool,
) -> Option<usize> {
    if collapsed {
        return (content_row == 0)
            .then(|| todo_preview_task_index(tasks))
            .flatten();
    }
    let task_row = if tasks.len() > visible_rows {
        content_row.checked_sub(1)?
    } else {
        content_row
    };
    (task_row < todo_visible_task_count(tasks.len(), visible_rows)).then_some(task_row)
}

fn todo_preview_task_index(tasks: &[TodoTask]) -> Option<usize> {
    tasks
        .iter()
        .position(|task| task.status == TodoStatus::InProgress)
        .or_else(|| {
            tasks
                .iter()
                .position(|task| task.status == TodoStatus::Pending)
        })
        .or_else(|| tasks.len().checked_sub(1))
}

fn todo_window_lines(
    todo: &TodoDisplay,
    width: usize,
    visible_rows: usize,
    collapsed: bool,
) -> Vec<Line<'static>> {
    if collapsed {
        let Some(index) = todo_preview_task_index(&todo.tasks) else {
            return Vec::new();
        };
        let task = &todo.tasks[index];
        let (marker, color) = todo_status_marker(task.status);
        return vec![Line::from(vec![
            Span::styled(marker.to_owned(), Style::default().fg(color)),
            Span::raw(" "),
            Span::raw(fit_text(&task.title, width.saturating_sub(2))),
        ])];
    }
    let visible_tasks = todo_visible_task_count(todo.tasks.len(), visible_rows);
    let tasks = &todo.tasks[..visible_tasks];
    let mut lines = Vec::new();
    if todo.tasks.len() > visible_rows {
        let hidden = todo.tasks.len().saturating_sub(visible_tasks);
        if hidden > 0 {
            lines.push(Line::from(Span::styled(
                format!("… 还有 {hidden} 项"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    for (index, task) in tasks.iter().enumerate() {
        let number = (index + 1).to_string();
        let prefix_width = number.width() + 4;
        let title = fit_text(&task.title, width.saturating_sub(prefix_width));
        let (marker, color) = todo_status_marker(task.status);
        lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), Style::default().fg(color)),
            Span::raw(format!(" {number}. ")),
            Span::raw(title),
        ]));
    }
    lines
}

fn todo_status_marker(status: TodoStatus) -> (&'static str, Color) {
    match status {
        TodoStatus::Pending => ("○", Color::DarkGray),
        TodoStatus::InProgress => ("◐", Color::Yellow),
        TodoStatus::Done => ("●", Color::Green),
    }
}
