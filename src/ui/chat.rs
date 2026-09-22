use super::*;

pub(super) fn draw_messages(
    frame: &mut Frame<'_>,
    area: Rect,
    viewport: Rect,
    app: &mut App,
    theme: &UiTheme,
) {
    let block = message_block();
    update_message_layout(app, viewport);
    let Some(layout) = &app.current.message_layout else {
        frame.render_widget(
            Paragraph::new(Vec::<Line<'static>>::new()).block(block),
            area,
        );
        return;
    };
    let selection = app.current.output_selection;
    let visible_lines = layout
        .visual_lines
        .iter()
        .skip(layout.scroll)
        .take(layout.viewport.height as usize)
        .map(|line| render_visual_line(layout, line, selection, theme))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(visible_lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
    draw_todo_window(frame, viewport, app, theme);
}

pub(crate) fn update_message_layout(app: &mut App, viewport: Rect) {
    let cached_width = app
        .current
        .message_layout
        .as_ref()
        .map(|layout| layout.width);
    let viewport_width = viewport.width.max(1) as usize;
    if app.current.output_layout_dirty || app.current.message_layout.is_none() {
        drop(app.current.message_layout.take());
        let rendered = render_message_lines(app, viewport_width);
        app.current.message_layout = Some(MessageLayout::new_with_interactions(
            rendered.lines,
            rendered.interactions,
            viewport,
            0,
            rendered.thinking_before,
        ));
        #[cfg(test)]
        {
            app.current.output_layout_rebuild_count += 1;
        }
    } else if cached_width != Some(viewport_width) {
        let layout = app
            .current
            .message_layout
            .take()
            .expect("layout existence was checked above");
        app.current.message_layout = Some(layout.reflow(viewport));
        #[cfg(test)]
        {
            app.current.output_layout_rebuild_count += 1;
        }
    } else if let Some(layout) = &mut app.current.message_layout {
        layout.update_viewport(viewport);
    }

    let existing_live_rows = app
        .current
        .message_layout
        .as_ref()
        .map_or(0, |layout| layout.live_thinking_lines.len());
    let thinking_update =
        live_thinking_lines(app, viewport.width.max(1) as usize, existing_live_rows);
    if let Some(layout) = &mut app.current.message_layout {
        // The "generating tool call" row is display-only: it must not be
        // clickable (expanding would be meaningless while arguments stream).
        layout.set_live_thinking_clickable(app.current.generating_tool.is_none());
        if let Some(lines) = thinking_update.lines {
            layout.set_live_thinking_lines(lines);
        } else {
            layout.set_live_thinking_title(thinking_update.title);
        }
        let max_scroll = layout.max_scroll();
        let anchored_scroll =
            app.layout_restore_anchor
                .take()
                .and_then(|(target, relative_row)| {
                    layout
                        .visual_lines
                        .iter()
                        .position(|line| line.interaction.as_ref() == Some(&target))
                        .map(|visual_row| visual_row.saturating_sub(relative_row))
                });
        let scroll = anchored_scroll
            .or(app.current.output_scroll_top)
            .unwrap_or_else(|| {
                if app.current.follow_output {
                    max_scroll
                } else {
                    max_scroll.saturating_sub(app.current.message_scroll)
                }
            })
            .min(max_scroll);
        if anchored_scroll.is_some() {
            app.current.output_scroll_top = Some(scroll);
            app.current.message_scroll = max_scroll.saturating_sub(scroll);
        }
        layout.set_scroll(scroll);
    }
    app.current.output_layout_dirty = false;
}

fn render_message_lines(app: &mut App, width: usize) -> RenderedMessageLines {
    let theme = UiTheme::default();
    let mut lines = Vec::new();
    let mut interactions = Vec::new();
    let mut thinking_before = None;
    let mut in_tool_group = false;
    let mut parsed_markdown = 0usize;
    let current = &mut app.current;
    let entries = &current.entries;
    let expanded_tools = &current.expanded_tools;
    let expanded_thinking = &current.expanded_thinking;
    let thinking_anchor = current.thinking_anchor;
    let render_cache = &mut current.markdown_render_cache;
    for (entry_index, entry) in entries.iter().enumerate() {
        if thinking_anchor == Some(entry_index) {
            thinking_before = Some(lines.len());
            in_tool_group = false;
        }
        if let DisplayContent::Tool(tool) = &entry.content {
            if !in_tool_group {
                push_rendered_line(
                    &mut lines,
                    &mut interactions,
                    Line::from(Span::styled("工具", theme.strong(VisualRole::Tool))),
                    None,
                );
                in_tool_group = true;
            }
            render_tool(
                tool,
                expanded_tools.contains(&tool.call_id),
                width,
                &mut lines,
                &mut interactions,
            );
            let group_ends = entries.get(entry_index + 1).is_none_or(|next| {
                !matches!(next.content, DisplayContent::Tool(_))
                    || thinking_anchor == Some(entry_index + 1)
            });
            if group_ends {
                push_rendered_line(&mut lines, &mut interactions, Line::default(), None);
                in_tool_group = false;
            }
            continue;
        }
        in_tool_group = false;
        if let DisplayContent::Thinking(thinking) = &entry.content {
            let expanded = expanded_thinking.contains(&thinking.id);
            let expanded_body = expanded.then(|| {
                let (rendered, parsed) = cached_markdown(
                    render_cache,
                    entry_index,
                    &thinking.content,
                    theme.style(VisualRole::Primary),
                    1,
                );
                parsed_markdown += usize::from(parsed);
                rendered
            });
            render_thinking_summary(
                thinking,
                expanded,
                expanded_body.as_deref(),
                width,
                &mut lines,
                &mut interactions,
            );
            push_rendered_line(&mut lines, &mut interactions, Line::default(), None);
            continue;
        }
        if let DisplayContent::Markdown(text) = &entry.content
            && matches!(entry.kind, DisplayKind::System | DisplayKind::Error)
        {
            let (prefix, role) = if matches!(entry.kind, DisplayKind::Error) {
                ("× ", VisualRole::Danger)
            } else {
                ("", VisualRole::Muted)
            };
            push_rendered_line(
                &mut lines,
                &mut interactions,
                Line::from(Span::styled(
                    format!("{prefix}{}", text.replace('\n', " ")),
                    theme.style(role),
                )),
                None,
            );
            push_rendered_line(&mut lines, &mut interactions, Line::default(), None);
            continue;
        }
        let (label, role) = match &entry.kind {
            DisplayKind::User => ("用户", VisualRole::User),
            DisplayKind::Assistant => ("Agent", VisualRole::Accent),
            DisplayKind::AssistantPartial => ("Agent（未完成）", VisualRole::Warning),
            DisplayKind::Thinking => ("思考摘要", VisualRole::Thinking),
            DisplayKind::Tool => ("工具", VisualRole::Tool),
            DisplayKind::Error => ("错误", VisualRole::Danger),
            DisplayKind::System => ("系统", VisualRole::Muted),
        };
        push_rendered_line(
            &mut lines,
            &mut interactions,
            Line::from(Span::styled(label, theme.strong(role))),
            None,
        );
        let content_style = theme.style(VisualRole::Primary);
        match &entry.content {
            DisplayContent::Markdown(text) => {
                if text.is_empty() {
                    push_rendered_line(
                        &mut lines,
                        &mut interactions,
                        Line::from(Span::styled("...", theme.style(VisualRole::Muted))),
                        None,
                    );
                } else {
                    let (rendered, parsed) =
                        cached_markdown(render_cache, entry_index, text, content_style, 0);
                    parsed_markdown += usize::from(parsed);
                    for line in rendered {
                        push_rendered_line(&mut lines, &mut interactions, line, None);
                    }
                }
            }
            DisplayContent::Diff(diff) => {
                for line in render_diff(diff) {
                    push_rendered_line(&mut lines, &mut interactions, line, None);
                }
            }
            DisplayContent::Tool(_) => unreachable!("tool entries are rendered as a group"),
            DisplayContent::Thinking(_) => unreachable!("thinking entries are rendered inline"),
        }
        push_rendered_line(&mut lines, &mut interactions, Line::default(), None);
    }
    if thinking_anchor.is_some() && thinking_before.is_none() {
        thinking_before = Some(lines.len());
    }
    #[cfg(test)]
    {
        current.markdown_parse_count += parsed_markdown;
    }
    RenderedMessageLines {
        lines,
        interactions,
        thinking_before,
    }
}

fn cached_markdown(
    cache: &mut std::collections::HashMap<usize, crate::output::CachedMarkdown>,
    entry_index: usize,
    text: &str,
    base: Style,
    variant: u8,
) -> (Vec<Line<'static>>, bool) {
    let mut hasher = DefaultHasher::new();
    variant.hash(&mut hasher);
    text.hash(&mut hasher);
    let fingerprint = hasher.finish();
    if let Some(cached) = cache.get(&entry_index)
        && cached.fingerprint == fingerprint
    {
        return (cached.lines.clone(), false);
    }
    let lines = render_markdown(text, base);
    cache.insert(
        entry_index,
        crate::output::CachedMarkdown {
            fingerprint,
            lines: lines.clone(),
        },
    );
    (lines, true)
}

fn push_rendered_line(
    lines: &mut Vec<Line<'static>>,
    interactions: &mut Vec<Option<InteractionTarget>>,
    line: Line<'static>,
    interaction: Option<InteractionTarget>,
) {
    lines.push(line);
    interactions.push(interaction);
}

pub(super) fn render_visual_line(
    layout: &MessageLayout,
    visual: &VisualLine,
    selection: Option<OutputSelection>,
    theme: &UiTheme,
) -> Line<'static> {
    if visual.synthetic {
        return Line::from(Span::styled(
            layout
                .live_thinking_lines
                .get(visual.start)
                .cloned()
                .unwrap_or_default(),
            theme.style(VisualRole::Thinking),
        ));
    }
    let Some(line) = layout.lines.get(visual.logical_line) else {
        return Line::default();
    };
    let local_start = visual.start.saturating_sub(line.start);
    let local_end = visual.end.saturating_sub(line.start);
    let selected_range = selection.and_then(OutputSelection::range);
    let first_run = line
        .style_runs
        .partition_point(|run| run.end <= local_start);
    let mut parts = Vec::<(String, Style)>::new();
    for run in line.style_runs.iter().skip(first_run) {
        if run.start >= local_end {
            break;
        }
        let start = run.start.max(local_start);
        let end = run.end.min(local_end);
        if start >= end {
            continue;
        }
        let global_start = line.start + start;
        let global_end = line.start + end;
        if let Some((selected_start, selected_end)) = selected_range
            && selected_start < global_end
            && selected_end > global_start
        {
            let before_end = selected_start.clamp(global_start, global_end);
            push_styled_slice(
                &mut parts,
                &line.text[start..before_end - line.start],
                run.style,
            );
            let highlighted_start = selected_start.max(global_start);
            let highlighted_end = selected_end.min(global_end);
            push_styled_slice(
                &mut parts,
                &line.text[highlighted_start - line.start..highlighted_end - line.start],
                run.style.fg(Color::Black).bg(Color::Cyan),
            );
            push_styled_slice(
                &mut parts,
                &line.text[highlighted_end - line.start..end],
                run.style,
            );
        } else {
            push_styled_slice(&mut parts, &line.text[start..end], run.style);
        }
    }
    Line::from(
        parts
            .into_iter()
            .map(|(text, style)| Span::styled(text, style))
            .collect::<Vec<_>>(),
    )
}

fn push_styled_slice(parts: &mut Vec<(String, Style)>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some((previous, previous_style)) = parts.last_mut()
        && *previous_style == style
    {
        previous.push_str(text);
    } else {
        parts.push((text.to_owned(), style));
    }
}

pub(super) fn render_markdown(text: &str, base: Style) -> Vec<Line<'static>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM;
    MarkdownRenderer::new(base).render(Parser::new_ext(text, options).into_offset_iter(), text)
}

struct MarkdownRenderer {
    base: Style,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    quote_depth: usize,
    lists: Vec<ListState>,
    assets: Vec<InlineAsset>,
    table: Option<TableState>,
    table_row: Option<TableRow>,
    table_cell: Option<Vec<Span<'static>>>,
    table_head: bool,
    code_block: Option<CodeBlockState>,
}

#[derive(Clone, Debug)]
struct CodeBlockState {
    _language: Option<String>,
}

#[derive(Clone, Copy)]
enum ListState {
    Unordered,
    Ordered(u64),
}

enum InlineAsset {
    Link(String),
    Image(String),
}

struct TableState {
    alignments: Vec<MarkdownAlignment>,
    rows: Vec<TableRow>,
}

struct TableRow {
    header: bool,
    cells: Vec<Vec<Span<'static>>>,
}

impl MarkdownRenderer {
    fn new(base: Style) -> Self {
        Self {
            base,
            lines: Vec::new(),
            current: Vec::new(),
            styles: vec![base],
            quote_depth: 0,
            lists: Vec::new(),
            assets: Vec::new(),
            table: None,
            table_row: None,
            table_cell: None,
            table_head: false,
            code_block: None,
        }
    }

    fn render<'a, I>(mut self, events: I, source: &str) -> Vec<Line<'static>>
    where
        I: IntoIterator<Item = (Event<'a>, Range<usize>)>,
    {
        for (event, range) in events {
            self.event_spacing(source, range.start, &event);
            self.event(event);
        }
        self.flush_line(false);
        self.lines
    }

    fn event_spacing(&mut self, source: &str, offset: usize, event: &Event<'_>) {
        let is_block_start = matches!(
            event,
            Event::Start(
                Tag::Paragraph
                    | Tag::Heading { .. }
                    | Tag::BlockQuote(_)
                    | Tag::CodeBlock(_)
                    | Tag::List(_)
                    | Tag::FootnoteDefinition(_)
                    | Tag::Table(_)
                    | Tag::HtmlBlock
            )
        );
        if !is_block_start || self.lines.is_empty() || !self.current.is_empty() {
            return;
        }
        let trailing_newlines = source[..offset.min(source.len())]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\n')
            .count();
        for _ in 1..trailing_newlines {
            self.lines.push(Line::default());
        }
    }

    fn event<'a>(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                let style = self.current_style();
                self.append_text(text.as_ref(), style);
            }
            Event::Code(text) => self.append_text(text.as_ref(), code_style()),
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.append_text(text.as_ref(), code_style());
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.append_text(text.as_ref(), raw_html_style());
            }
            Event::FootnoteReference(label) => {
                let style = self.current_style();
                self.append_text(&format!("[^{label}]"), style);
            }
            Event::SoftBreak | Event::HardBreak => self.flush_line(true),
            Event::Rule => {
                self.flush_line(false);
                self.lines.push(Line::from(Span::styled(
                    "────────",
                    self.base.fg(Color::DarkGray),
                )));
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "[x] " } else { "[ ] " };
                self.append_text(marker, self.current_style());
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.table_cell.is_none() {
                    self.ensure_prefix();
                }
            }
            Tag::Heading { level, .. } => {
                self.flush_line(false);
                let style = heading_style(self.base, level);
                self.push_style(style);
                self.ensure_prefix();
            }
            Tag::BlockQuote(kind) => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.quote_depth = self.quote_depth.saturating_add(1);
                    let quote_style = self
                        .current_style()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC);
                    self.push_style(quote_style);
                    if let Some(kind) = kind {
                        self.ensure_quote_prefix();
                        self.append_span(Span::styled(
                            alert_label(kind),
                            Style::default()
                                .fg(alert_color(kind))
                                .add_modifier(Modifier::BOLD),
                        ));
                        self.flush_line(false);
                    }
                }
            }
            Tag::CodeBlock(kind) => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.push_style(code_style());
                    self.code_block = Some(CodeBlockState {
                        _language: match kind {
                            CodeBlockKind::Fenced(language) if !language.is_empty() => {
                                Some(language.to_string())
                            }
                            _ => None,
                        },
                    });
                }
            }
            Tag::List(start) => {
                if self.table_cell.is_none() {
                    self.lists.push(match start {
                        Some(number) => ListState::Ordered(number),
                        None => ListState::Unordered,
                    });
                }
            }
            Tag::Item => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.ensure_quote_prefix();
                    let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                    let marker = match self.lists.last_mut() {
                        Some(ListState::Unordered) => "• ".to_owned(),
                        Some(ListState::Ordered(number)) => {
                            let marker = format!("{number}. ");
                            *number = number.saturating_add(1);
                            marker
                        }
                        None => String::new(),
                    };
                    self.current.push(Span::styled(
                        format!("{indent}{marker}"),
                        self.base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_line(false);
                self.append_span(Span::styled(
                    format!("[^{label}]: "),
                    self.base.fg(Color::DarkGray),
                ));
            }
            Tag::Table(alignments) => {
                self.flush_line(false);
                self.table = Some(TableState {
                    alignments,
                    rows: Vec::new(),
                });
            }
            Tag::TableHead => {
                self.table_head = true;
                self.table_row = Some(TableRow {
                    header: true,
                    cells: Vec::new(),
                });
                self.push_style(self.current_style().add_modifier(Modifier::BOLD));
            }
            Tag::TableRow => {
                self.table_row = Some(TableRow {
                    header: self.table_head,
                    cells: Vec::new(),
                });
            }
            Tag::TableCell => {
                self.table_cell = Some(Vec::new());
            }
            Tag::Emphasis => self.push_style(self.current_style().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(self.current_style().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(self.current_style().add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Superscript | Tag::Subscript => {
                self.push_style(self.current_style());
            }
            Tag::Link { dest_url, .. } => {
                self.assets.push(InlineAsset::Link(dest_url.to_string()));
                self.push_style(
                    self.current_style()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.assets.push(InlineAsset::Image(dest_url.to_string()));
                self.push_style(self.current_style().fg(Color::Cyan));
            }
            Tag::HtmlBlock => {
                self.flush_line(false);
                self.push_style(raw_html_style());
            }
            Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if self.table_cell.is_none() {
                    self.flush_line(true);
                }
            }
            TagEnd::Heading(_) => {
                if self.table_cell.is_none() {
                    self.flush_line(true);
                }
                self.pop_style();
            }
            TagEnd::BlockQuote(_) => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.quote_depth = self.quote_depth.saturating_sub(1);
                    self.pop_style();
                }
            }
            TagEnd::CodeBlock => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.code_block = None;
                    self.pop_style();
                }
            }
            TagEnd::List(_) => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                    self.lists.pop();
                }
            }
            TagEnd::Item => {
                if self.table_cell.is_none() {
                    self.flush_line(false);
                }
            }
            TagEnd::FootnoteDefinition => self.flush_line(false),
            TagEnd::TableHead => {
                self.table_head = false;
                if let Some(row) = self.table_row.take()
                    && let Some(table) = &mut self.table
                {
                    table.rows.push(row);
                }
                self.pop_style();
            }
            TagEnd::TableRow => {
                if let Some(row) = self.table_row.take()
                    && let Some(table) = &mut self.table
                {
                    table.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                if let Some(cell) = self.table_cell.take()
                    && let Some(row) = &mut self.table_row
                {
                    row.cells.push(cell);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.lines.extend(render_table(table));
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link | TagEnd::Image => {
                self.pop_style();
                if let Some(asset) = self.assets.pop() {
                    let destination = match asset {
                        InlineAsset::Link(destination) | InlineAsset::Image(destination) => {
                            destination
                        }
                    };
                    self.append_span(Span::styled(
                        format!(" ({destination})"),
                        self.base.fg(Color::DarkGray),
                    ));
                }
            }
            TagEnd::HtmlBlock => {
                self.flush_line(false);
                self.pop_style();
            }
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn current_style(&self) -> Style {
        self.styles.last().copied().unwrap_or(self.base)
    }

    fn push_style(&mut self, style: Style) {
        self.styles.push(style);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn append_text(&mut self, text: &str, style: Style) {
        if self.table_cell.is_some() {
            let text = text.replace(['\r', '\n'], " ");
            if !text.is_empty() {
                self.append_span(Span::styled(text, style));
            }
            return;
        }
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush_line(true);
            }
            if !part.is_empty() {
                self.ensure_prefix();
                self.append_span(Span::styled(part.to_owned(), style));
            }
        }
    }

    fn append_span(&mut self, span: Span<'static>) {
        if let Some(cell) = &mut self.table_cell {
            cell.push(span);
        } else {
            self.current.push(span);
        }
    }

    fn ensure_quote_prefix(&mut self) {
        if self.current.is_empty() && self.quote_depth > 0 {
            self.current.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                self.base.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ));
        }
    }

    fn ensure_prefix(&mut self) {
        if self.table_cell.is_some() || !self.current.is_empty() {
            return;
        }
        self.ensure_quote_prefix();
        if self.current.is_empty() && !self.lists.is_empty() {
            self.current.push(Span::raw("  ".repeat(self.lists.len())));
        }
    }

    fn flush_line(&mut self, force: bool) {
        if !self.current.is_empty() || force {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
    }
}

fn render_table(table: TableState) -> Vec<Line<'static>> {
    const MAX_TABLE_CELL_WIDTH: usize = 24;
    let alignments = table.alignments;
    let column_count = table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }
    let mut widths = vec![3; column_count];
    for row in &table.rows {
        for (column, cell) in row.cells.iter().enumerate() {
            let width = cell
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>();
            widths[column] = widths[column].max(width.min(MAX_TABLE_CELL_WIDTH));
        }
    }

    let mut lines = Vec::new();
    let mut rendered_header = false;
    for row in table.rows {
        let mut spans = vec![Span::styled("| ", table_border_style())];
        for (column, width) in widths.iter().enumerate() {
            let cell = row.cells.get(column).cloned().unwrap_or_default();
            let alignment = alignments
                .get(column)
                .copied()
                .unwrap_or(MarkdownAlignment::None);
            spans.extend(render_table_cell(cell, *width, alignment));
            spans.push(Span::styled(" | ", table_border_style()));
        }
        lines.push(Line::from(spans));
        if row.header && !rendered_header {
            rendered_header = true;
            let mut separator = vec![Span::styled("| ", table_border_style())];
            for (column, width) in widths.iter().enumerate() {
                let alignment = alignments
                    .get(column)
                    .copied()
                    .unwrap_or(MarkdownAlignment::None);
                separator.push(Span::styled(
                    table_separator(*width, alignment),
                    table_border_style(),
                ));
                separator.push(Span::styled(" | ", table_border_style()));
            }
            lines.push(Line::from(separator));
        }
    }
    lines
}

fn render_table_cell(
    cell: Vec<Span<'static>>,
    target_width: usize,
    alignment: MarkdownAlignment,
) -> Vec<Span<'static>> {
    let cell_width = cell
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    let extra = target_width.saturating_sub(cell_width);
    let (left, right) = match alignment {
        MarkdownAlignment::Right => (extra, 0),
        MarkdownAlignment::Center => {
            let left = extra / 2;
            (left, extra.saturating_sub(left))
        }
        MarkdownAlignment::Left | MarkdownAlignment::None => (0, extra),
    };
    let mut spans = Vec::new();
    if left > 0 {
        spans.push(Span::styled(" ".repeat(left), table_border_style()));
    }
    spans.extend(cell);
    if right > 0 {
        spans.push(Span::styled(" ".repeat(right), table_border_style()));
    }
    spans
}

fn table_separator(width: usize, alignment: MarkdownAlignment) -> String {
    let dashes = "-".repeat(width);
    match alignment {
        MarkdownAlignment::Left => format!(":{dashes}"),
        MarkdownAlignment::Center => format!(":{dashes}:"),
        MarkdownAlignment::Right => format!("{dashes}:"),
        MarkdownAlignment::None => dashes,
    }
}

fn heading_style(base: Style, level: HeadingLevel) -> Style {
    base.fg(if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
        Color::Cyan
    } else {
        Color::Blue
    })
    .add_modifier(Modifier::BOLD)
}

fn alert_label(kind: BlockQuoteKind) -> &'static str {
    match kind {
        BlockQuoteKind::Note => "NOTE",
        BlockQuoteKind::Tip => "TIP",
        BlockQuoteKind::Important => "IMPORTANT",
        BlockQuoteKind::Warning => "WARNING",
        BlockQuoteKind::Caution => "CAUTION",
    }
}

fn alert_color(kind: BlockQuoteKind) -> Color {
    match kind {
        BlockQuoteKind::Note => Color::Cyan,
        BlockQuoteKind::Tip => Color::Green,
        BlockQuoteKind::Important => Color::Magenta,
        BlockQuoteKind::Warning => Color::Yellow,
        BlockQuoteKind::Caution => Color::Red,
    }
}

fn raw_html_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn table_border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub(super) fn render_tool(
    tool: &ToolDisplay,
    expanded: bool,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    interactions: &mut Vec<Option<InteractionTarget>>,
) {
    let marker = match (expanded, &tool.status) {
        (true, _) => "▾",
        (false, ToolDisplayStatus::Running) => "◌",
        (false, _) => "▸",
    };
    let status = match tool.status {
        ToolDisplayStatus::Running => "",
        ToolDisplayStatus::Completed => "  ✓",
        ToolDisplayStatus::Failed | ToolDisplayStatus::Rejected => "  ✗",
    };
    let name = tool_display_name(&tool.name);
    let fixed_width = UnicodeWidthStr::width(marker)
        .saturating_add(1)
        .saturating_add(UnicodeWidthStr::width(name.as_str()))
        .saturating_add(UnicodeWidthStr::width(status));
    let summary = tool_compact_summary(
        &tool.name,
        &tool.arguments,
        width.saturating_sub(fixed_width.saturating_add(2)),
    );
    let summary = if summary.is_empty() {
        String::new()
    } else {
        format!("  {summary}")
    };
    push_rendered_line(
        lines,
        interactions,
        Line::from(vec![
            Span::styled(format!("{marker} "), Style::default().fg(Color::DarkGray)),
            Span::styled(
                name,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(summary),
            Span::styled(
                status,
                Style::default().fg(match tool.status {
                    ToolDisplayStatus::Completed => Color::Green,
                    ToolDisplayStatus::Failed | ToolDisplayStatus::Rejected => Color::Red,
                    ToolDisplayStatus::Running => Color::Yellow,
                }),
            ),
        ]),
        Some(InteractionTarget::Tool(tool.call_id.clone())),
    );
    if !expanded {
        return;
    }
    push_rendered_line(lines, interactions, Line::from("  参数"), None);
    if let Some(arguments) = tool.arguments.as_object() {
        for (key, value) in arguments {
            push_rendered_line(
                lines,
                interactions,
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        format!("{}：", argument_label(key)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(human_argument(key, value)),
                ]),
                None,
            );
        }
    } else {
        push_rendered_line(lines, interactions, Line::from("    （无）"), None);
    }
    push_rendered_line(lines, interactions, Line::from("  结果"), None);
    match tool.result.as_deref() {
        Some(result) if !result.is_empty() => {
            for line in result.lines() {
                push_rendered_line(
                    lines,
                    interactions,
                    Line::from(vec![
                        Span::raw("    "),
                        Span::styled(secrets::redact(line), code_style()),
                    ]),
                    None,
                );
            }
        }
        Some(_) => push_rendered_line(lines, interactions, Line::from("    （空）"), None),
        None => push_rendered_line(lines, interactions, Line::from("    执行中…"), None),
    }
}

pub(super) fn render_thinking_summary(
    thinking: &ThinkingDisplay,
    expanded: bool,
    expanded_body: Option<&[Line<'static>]>,
    width: usize,
    lines: &mut Vec<Line<'static>>,
    interactions: &mut Vec<Option<InteractionTarget>>,
) {
    let theme = UiTheme::default();
    let marker = if expanded { "▾" } else { "▸" };
    let label = "思考摘要";
    let interaction = Some(InteractionTarget::ThinkingSummary(thinking.id.clone()));
    if !expanded {
        let last_line = thinking
            .content
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();
        let fixed_width = UnicodeWidthStr::width(marker)
            .saturating_add(1)
            .saturating_add(UnicodeWidthStr::width(label));
        let summary = fit_text_tail(
            last_line,
            width.saturating_sub(fixed_width.saturating_add(2)),
        );
        push_rendered_line(
            lines,
            interactions,
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(Color::DarkGray)),
                Span::styled(label, theme.strong(VisualRole::Thinking)),
                Span::raw(if summary.is_empty() {
                    String::new()
                } else {
                    format!("  {summary}")
                }),
            ]),
            interaction,
        );
        return;
    }
    push_rendered_line(
        lines,
        interactions,
        Line::from(vec![
            Span::styled(format!("{marker} "), Style::default().fg(Color::DarkGray)),
            Span::styled(label, theme.strong(VisualRole::Thinking)),
        ]),
        interaction,
    );
    if thinking.content.trim().is_empty() {
        push_rendered_line(
            lines,
            interactions,
            Line::from(Span::styled("  （空）", theme.style(VisualRole::Muted))),
            None,
        );
        return;
    }
    if let Some(body) = expanded_body {
        for line in body {
            push_rendered_line(lines, interactions, line.clone(), None);
        }
    } else {
        for line in render_markdown(&thinking.content, theme.style(VisualRole::Primary)) {
            push_rendered_line(lines, interactions, line, None);
        }
    }
}

pub(crate) fn tool_display_name(name: &str) -> String {
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

fn tool_compact_summary(name: &str, arguments: &Value, width: usize) -> String {
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| arguments.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
    };
    let raw = match name {
        "file_read" | "file_write" | "file_edit" | "file_stat" | "file_list" | "file_mkdir"
        | "file_delete" | "repo_map" => get(&["path"]).to_owned(),
        "file_search" => [get(&["path"]), get(&["query", "pattern"])]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("  "),
        "file_glob" => [get(&["path"]), get(&["pattern"])]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("  "),
        "web_search" => get(&["query"]).to_owned(),
        "market_quote" => [get(&["symbol"]), get(&["query"])]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("  "),
        "web_fetch" | "webfetch" | "browser_open" => get(&["url"]).to_owned(),
        "terminal_exec" => {
            let mut parts = Vec::new();
            let program = get(&["program", "command"]);
            if !program.is_empty() {
                parts.push(program.to_owned());
            }
            if let Some(args) = arguments.get("args").and_then(Value::as_array) {
                parts.extend(
                    args.iter()
                        .filter_map(Value::as_str)
                        .take(4)
                        .map(str::to_owned),
                );
            }
            parts.join(" ")
        }
        "terminal_shell" => get(&["command"]).to_owned(),
        "git" | "git_diff" => arguments
            .get("args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(Value::as_str)
                    .take(6)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        "file_move" | "file_copy" => format!(
            "{} -> {}",
            get(&["source", "from"]),
            get(&["destination", "to"])
        ),
        "agent_spawn" => get(&["prompt", "task"]).to_owned(),
        _ => get(&["path", "query", "url"]).to_owned(),
    };
    fit_text_tail(&secrets::redact(raw.trim()), width)
}

fn fit_text_tail(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    let ellipsis = if width > 1 { "…" } else { "" };
    let target = width.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut kept = Vec::new();
    let mut used = 0usize;
    for grapheme in value.graphemes(true).rev() {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used.saturating_add(grapheme_width) > target {
            break;
        }
        kept.push(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    kept.reverse();
    format!("{ellipsis}{}", kept.concat())
}

pub(super) fn render_diff(diff: &str) -> Vec<Line<'static>> {
    if diff.trim().is_empty() {
        return vec![Line::from(Span::styled(
            "（工作区干净）",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    diff.lines()
        .map(|line| {
            let color = if line.starts_with("+++") || line.starts_with("---") {
                Color::Cyan
            } else if line.starts_with('+') {
                Color::Green
            } else if line.starts_with('-') {
                Color::Red
            } else if line.starts_with("@@") {
                Color::Yellow
            } else if line.starts_with("diff ") {
                Color::Cyan
            } else {
                Color::White
            };
            Line::from(Span::styled(line.to_owned(), Style::default().fg(color)))
        })
        .collect()
}

fn code_style() -> Style {
    UiTheme::default().style(VisualRole::Code)
}

/// Clickable rectangle for the mode portion of the input title, which is
/// rendered left-aligned on the top border as `" 输入 · {模式} "`.
struct LiveThinkingUpdate {
    title: String,
    lines: Option<Vec<String>>,
}

fn live_thinking_lines(app: &mut App, width: usize, existing_rows: usize) -> LiveThinkingUpdate {
    live_thinking_lines_cached(
        app,
        width,
        crate::app::braille_spinner_supported(),
        existing_rows,
    )
}

fn live_thinking_lines_cached(
    app: &mut App,
    width: usize,
    braille: bool,
    existing_rows: usize,
) -> LiveThinkingUpdate {
    if app.current.thinking_anchor.is_none() {
        return LiveThinkingUpdate {
            title: String::new(),
            lines: (existing_rows != 0).then(Vec::new),
        };
    }
    if !app.current.thinking_expanded {
        let lines = live_thinking_lines_with_braille(app, width, braille);
        let title = lines.into_iter().next().unwrap_or_default();
        return LiveThinkingUpdate {
            title: title.clone(),
            lines: (existing_rows != 1).then(|| vec![title]),
        };
    }

    let width = width.max(1);
    let status = if app.current.thinking_active {
        "思考中"
    } else {
        match app.current.thinking_result {
            crate::app::ThinkingResult::Completed => "思考完成",
            crate::app::ThinkingResult::Failed => "思考失败",
            crate::app::ThinkingResult::Cancelled => "思考已取消",
        }
    };
    let title = if app.current.thinking_active {
        format!(
            "▾ {} {status}",
            crate::app::thinking_animation_glyph(app.current.thinking_animation_frame, braille)
        )
    } else {
        format!("▾ {status}")
    };
    let title = fit_text_tail(&title, width);

    let buffer = &app.current.thinking_buffer;
    let reasoning = buffer.trim();
    if reasoning.is_empty() {
        let mut lines = vec![title.clone()];
        if app.current.thinking_buffer_truncated {
            lines.extend(wrap_grapheme_lines("  [较早思考内容已截断]", width));
        }
        lines.extend(wrap_grapheme_lines(
            &format!("  {}", app.current.thinking_last_line),
            width,
        ));
        return LiveThinkingUpdate {
            title,
            lines: Some(lines),
        };
    }
    let source_start = reasoning.as_ptr() as usize - buffer.as_ptr() as usize;
    let epoch = app.current.thinking_buffer_epoch;
    let cache = &mut app.current.live_thinking_layout_cache;
    let prefix_unchanged = cache.width == width
        && cache.buffer_epoch == epoch
        && cache.source_start == source_start
        && cache.processed_len <= reasoning.len()
        && reasoning.is_char_boundary(cache.processed_len);
    let mut body_changed = !prefix_unchanged;
    if body_changed {
        #[cfg(test)]
        let rebuilds = cache.full_rebuilds + 1;
        cache.clear();
        cache.width = width;
        cache.buffer_epoch = epoch;
        cache.source_start = source_start;
        cache.current_row = "  ".into();
        cache.current_width = 2;
        #[cfg(test)]
        {
            cache.full_rebuilds = rebuilds;
        }
    }
    let tail = &reasoning[cache.processed_len..];
    body_changed |= !tail.is_empty();
    #[cfg(test)]
    {
        cache.processed_bytes += tail.len();
    }
    append_wrapped_thinking(cache, tail, width);
    cache.processed_len = reasoning.len();
    let lines = (body_changed || existing_rows <= 1).then(|| {
        let mut lines = vec![title.clone()];
        if app.current.thinking_buffer_truncated {
            lines.extend(wrap_grapheme_lines("  [较早思考内容已截断]", width));
        }
        lines.extend(cache.rows.iter().cloned());
        lines.push(cache.current_row.clone());
        lines
    });
    LiveThinkingUpdate { title, lines }
}

fn append_wrapped_thinking(
    cache: &mut crate::projection::LiveThinkingLayoutCache,
    value: &str,
    width: usize,
) {
    for grapheme in value.graphemes(true) {
        if grapheme == "\n" || grapheme == "\r\n" {
            cache.rows.push(std::mem::take(&mut cache.current_row));
            cache.current_row = "  ".into();
            cache.current_width = 2;
            continue;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if cache.current_width > 0 && cache.current_width.saturating_add(grapheme_width) > width {
            cache.rows.push(std::mem::take(&mut cache.current_row));
            cache.current_width = 0;
        }
        cache.current_row.push_str(grapheme);
        cache.current_width = cache.current_width.saturating_add(grapheme_width);
    }
}

fn live_thinking_lines_with_braille(app: &App, width: usize, braille: bool) -> Vec<String> {
    if app.current.thinking_anchor.is_none() {
        return Vec::new();
    }
    // The "generating tool call" row: an animated, collapsed, non-clickable line
    // that keeps the screen live while a large argument payload (e.g. a 9 KB
    // file_write) streams from the model.
    if let Some((name, bytes)) = &app.current.generating_tool {
        let spinner =
            crate::app::thinking_animation_glyph(app.current.thinking_animation_frame, braille);
        let line = format!(
            "⚙ {spinner} 生成工具调用 {name} · {}",
            crate::projection::format_bytes(*bytes)
        );
        return vec![fit_text_tail(&line, width)];
    }
    let status = if app.current.thinking_active {
        "思考中"
    } else {
        match app.current.thinking_result {
            crate::app::ThinkingResult::Completed => "思考完成",
            crate::app::ThinkingResult::Failed => "思考失败",
            crate::app::ThinkingResult::Cancelled => "思考已取消",
        }
    };
    if !app.current.thinking_expanded {
        let prefix = if app.current.thinking_active {
            crate::app::thinking_animation_glyph(app.current.thinking_animation_frame, braille)
                .to_string()
        } else {
            match app.current.thinking_result {
                crate::app::ThinkingResult::Completed => "✓",
                crate::app::ThinkingResult::Failed => "✗",
                crate::app::ThinkingResult::Cancelled => "■",
            }
            .to_owned()
        };
        let suffix = app.current.thinking_last_line.trim();
        let fixed = format!("{prefix} {status}");
        let available = width
            .saturating_sub(UnicodeWidthStr::width(fixed.as_str()))
            .saturating_sub(2);
        let suffix = fit_text_tail(suffix, available);
        return vec![if suffix.is_empty()
            || (app.current.thinking_active && suffix == "模型正在思考")
        {
            fixed
        } else {
            format!("{fixed}  {suffix}")
        }];
    }
    let expanded_title = if app.current.thinking_active {
        format!(
            "▾ {} {status}",
            crate::app::thinking_animation_glyph(app.current.thinking_animation_frame, braille)
        )
    } else {
        format!("▾ {status}")
    };
    let mut lines = vec![fit_text_tail(&expanded_title, width)];
    if app.current.thinking_buffer_truncated {
        lines.extend(wrap_grapheme_lines("  [较早思考内容已截断]", width));
    }
    let reasoning = app.current.thinking_buffer.trim();
    if reasoning.is_empty() {
        lines.extend(wrap_grapheme_lines(
            &format!("  {}", app.current.thinking_last_line),
            width,
        ));
    } else {
        for line in reasoning.lines() {
            lines.extend(wrap_grapheme_lines(&format!("  {line}"), width));
        }
    }
    lines
}

pub(super) fn wrap_grapheme_lines(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut output = Vec::new();
    for logical_line in value.split('\n') {
        if logical_line.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut row = String::new();
        let mut used = 0usize;
        for grapheme in logical_line.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if !row.is_empty() && used.saturating_add(grapheme_width) > width {
                output.push(row);
                row = String::new();
                used = 0;
            }
            row.push_str(grapheme);
            used = used.saturating_add(grapheme_width);
        }
        output.push(row);
    }
    output
}
