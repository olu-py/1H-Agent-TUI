use ratatui::{layout::Rect, style::Style, text::Line};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub struct LayoutLine {
    pub text: String,
    pub style_runs: Vec<StyleRun>,
    pub start: usize,
    pub interaction: Option<InteractionTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleRun {
    /// Byte offsets local to `LayoutLine::text`.
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

#[derive(Clone, Debug)]
pub struct CachedMarkdown {
    pub revision: u64,
    pub variant: u8,
    pub lines: Vec<Line<'static>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InteractionTarget {
    Tool(String),
    Thinking,
    ThinkingSummary(String),
    Todo(String),
    TodoToggle,
    TodoClose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualLine {
    pub logical_line: usize,
    pub start: usize,
    pub end: usize,
    pub interaction: Option<InteractionTarget>,
    pub synthetic: bool,
    pub clickable_width: usize,
}

/// A cached, independently wrapped transcript segment. Stable segments can be
/// reused across tail revisions without grapheme-wrapping the whole transcript.
#[derive(Clone, Debug)]
pub struct CachedLayoutSegment {
    pub revision: u64,
    pub width: usize,
    pub variant: u64,
    data: Arc<SegmentData>,
}

#[derive(Debug)]
struct SegmentData {
    text: String,
    lines: Vec<LayoutLine>,
    visual_lines: Vec<VisualLine>,
}

impl CachedLayoutSegment {
    pub fn new(
        revision: u64,
        width: usize,
        variant: u64,
        lines: Vec<Line<'static>>,
        interactions: Vec<Option<InteractionTarget>>,
    ) -> Self {
        let width = width.max(1);
        let viewport = Rect::new(0, 0, width.min(u16::MAX as usize) as u16, 1);
        let layout = MessageLayout::new_with_interactions(lines, interactions, viewport, 0, None);
        Self {
            revision,
            width,
            variant,
            data: Arc::new(SegmentData {
                text: layout.text,
                lines: layout.lines,
                visual_lines: layout.visual_lines,
            }),
        }
    }

    pub fn logical_line_count(&self) -> usize {
        self.data.lines.len()
    }
}

#[derive(Clone, Debug)]
struct SegmentEnd {
    data: Arc<SegmentData>,
    bytes: usize,
    lines: usize,
    rows: usize,
}

#[derive(Clone, Debug)]
pub struct MessageLayout {
    pub viewport: Rect,
    pub width: usize,
    pub scroll: usize,
    pub text: String,
    pub lines: Vec<LayoutLine>,
    pub visual_lines: Vec<VisualLine>,
    segment_ends: Vec<SegmentEnd>,
    pub live_thinking_before: Option<usize>,
    pub live_thinking_rows: usize,
    /// First synthetic visual row, cached so per-frame title updates do not
    /// scan the full transcript layout.
    pub live_thinking_visual_start: Option<usize>,
    pub live_thinking_lines: Vec<String>,
    /// Whether the first live-thinking row is clickable (toggles expansion).
    /// The "generating tool call" row is display-only and clears this flag.
    pub live_thinking_clickable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSelection {
    pub anchor: usize,
    pub active: usize,
    pub dragging: bool,
}

impl OutputSelection {
    pub fn new(offset: usize) -> Self {
        Self {
            anchor: offset,
            active: offset,
            dragging: true,
        }
    }

    pub fn range(self) -> Option<(usize, usize)> {
        let start = self.anchor.min(self.active);
        let end = self.anchor.max(self.active);
        (start < end).then_some((start, end))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeScroll {
    pub direction: i8,
    pub column: u16,
}

impl MessageLayout {
    /// Compose cached segments. Subsequent updates retain the unchanged prefix.
    pub fn from_segments(
        segments: &[CachedLayoutSegment],
        viewport: Rect,
        scroll: usize,
        live_thinking_before: Option<usize>,
    ) -> Self {
        let width = viewport.width.max(1) as usize;
        let mut layout = Self {
            viewport,
            width,
            scroll: 0,
            text: String::new(),
            lines: Vec::new(),
            visual_lines: Vec::new(),
            segment_ends: Vec::new(),
            live_thinking_before,
            live_thinking_rows: 0,
            live_thinking_visual_start: None,
            live_thinking_lines: Vec::new(),
            live_thinking_clickable: true,
        };
        layout.update_segments(segments, viewport, live_thinking_before);
        layout.scroll = scroll.min(layout.max_scroll());
        layout
    }

    /// Recompose only the suffix beginning at the first changed segment.
    /// The index scan is O(entries), while a tail revision copies no stable
    /// prefix text, style runs, or visual rows. Prepend/resize still rebuild.
    pub fn update_segments(
        &mut self,
        segments: &[CachedLayoutSegment],
        viewport: Rect,
        live_thinking_before: Option<usize>,
    ) {
        let prefix = if self.width == viewport.width.max(1) as usize {
            self.segment_ends
                .iter()
                .zip(segments)
                .take_while(|(old, new)| Arc::ptr_eq(&old.data, &new.data))
                .count()
        } else {
            0
        };
        self.update_segment_suffix(prefix, &segments[prefix..], viewport, live_thinking_before);
    }

    pub(crate) fn segment_count(&self) -> usize {
        self.segment_ends.len()
    }

    pub(crate) fn prefix_logical_lines(&self, count: usize) -> usize {
        count
            .checked_sub(1)
            .map_or(0, |index| self.segment_ends[index].lines)
    }

    /// The caller holds an unchanged prefix by entry identity and generation.
    /// Unlike update_segments, this path receives only the changed suffix.
    pub(crate) fn update_segment_suffix(
        &mut self,
        prefix: usize,
        suffix: &[CachedLayoutSegment],
        viewport: Rect,
        live_thinking_before: Option<usize>,
    ) {
        assert!(prefix <= self.segment_ends.len());
        assert!(prefix == 0 || self.width == viewport.width.max(1) as usize);
        if let Some(start) = self.live_thinking_visual_start.take() {
            self.visual_lines
                .drain(start..start + self.live_thinking_rows);
        }
        self.live_thinking_rows = 0;
        self.live_thinking_lines.clear();
        self.live_thinking_before = live_thinking_before;
        let (bytes, lines, rows) = prefix
            .checked_sub(1)
            .map(|index| &self.segment_ends[index])
            .map_or((0, 0, 0), |end| (end.bytes, end.lines, end.rows));
        self.text.truncate(bytes);
        self.lines.truncate(lines);
        self.visual_lines.truncate(rows);
        self.segment_ends.truncate(prefix);
        for segment in suffix {
            if crate::layout_work::cancelled() {
                break;
            }
            let data = &segment.data;
            if !self.lines.is_empty() && !data.lines.is_empty() {
                self.text.push('\n');
            }
            let byte_base = self.text.len();
            let logical_base = self.lines.len();
            self.text.push_str(&data.text);
            self.lines
                .extend(data.lines.iter().cloned().map(|mut line| {
                    line.start += byte_base;
                    line
                }));
            self.visual_lines
                .extend(data.visual_lines.iter().cloned().map(|mut line| {
                    line.logical_line += logical_base;
                    line.start += byte_base;
                    line.end += byte_base;
                    line
                }));
            self.segment_ends.push(SegmentEnd {
                data: Arc::clone(data),
                bytes: self.text.len(),
                lines: self.lines.len(),
                rows: self.visual_lines.len(),
            });
        }
        self.width = viewport.width.max(1) as usize;
        self.update_viewport(viewport);
    }

    pub fn new(lines: Vec<Line<'static>>, viewport: Rect, scroll: usize) -> Self {
        Self::new_with_live_thinking(lines, viewport, scroll, None)
    }

    pub fn new_with_live_thinking(
        lines: Vec<Line<'static>>,
        viewport: Rect,
        scroll: usize,
        live_thinking_before: Option<usize>,
    ) -> Self {
        Self::new_with_interactions(lines, vec![], viewport, scroll, live_thinking_before)
    }

    pub fn new_with_interactions(
        lines: Vec<Line<'static>>,
        interactions: Vec<Option<InteractionTarget>>,
        viewport: Rect,
        scroll: usize,
        live_thinking_before: Option<usize>,
    ) -> Self {
        let width = viewport.width.max(1) as usize;
        let mut text = String::new();
        let mut layout_lines = Vec::with_capacity(lines.len());
        for (index, styled) in lines.into_iter().enumerate() {
            if crate::layout_work::cancelled() {
                break;
            }
            if index > 0 {
                text.push('\n');
            }
            let start = text.len();
            let mut line_text = String::new();
            let mut style_runs = Vec::<StyleRun>::new();
            for span in styled.spans {
                if crate::layout_work::cancelled() {
                    break;
                }
                let span_text = span.content.as_ref();
                if span_text.is_empty() {
                    continue;
                }
                let run_start = line_text.len();
                line_text.push_str(span_text);
                let run_end = line_text.len();
                if let Some(previous) = style_runs.last_mut()
                    && previous.end == run_start
                    && previous.style == span.style
                {
                    previous.end = run_end;
                } else {
                    style_runs.push(StyleRun {
                        start: run_start,
                        end: run_end,
                        style: span.style,
                    });
                }
            }
            text.push_str(&line_text);
            layout_lines.push(LayoutLine {
                text: line_text,
                style_runs,
                start,
                interaction: interactions.get(index).cloned().flatten(),
            });
        }

        let live_thinking_rows = usize::from(live_thinking_before.is_some());
        let visual_lines = build_visual_lines(
            &layout_lines,
            width,
            live_thinking_before,
            live_thinking_rows,
        );
        let live_thinking_visual_start = visual_lines.iter().position(|line| line.synthetic);
        let max_scroll = visual_lines.len().saturating_sub(viewport.height as usize);
        Self {
            viewport,
            width,
            scroll: scroll.min(max_scroll),
            text,
            lines: layout_lines,
            visual_lines,
            segment_ends: Vec::new(),
            live_thinking_before,
            live_thinking_rows,
            live_thinking_visual_start,
            live_thinking_lines: Vec::new(),
            live_thinking_clickable: true,
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.visual_lines
            .len()
            .saturating_sub(self.viewport.height as usize)
    }

    pub fn update_viewport(&mut self, viewport: Rect) {
        debug_assert_eq!(self.width, viewport.width.max(1) as usize);
        self.viewport = viewport;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll.min(self.max_scroll());
    }

    pub fn reflow(mut self, viewport: Rect) -> Self {
        self.segment_ends.clear();
        let width = viewport.width.max(1) as usize;
        self.visual_lines = build_visual_lines(
            &self.lines,
            width,
            self.live_thinking_before,
            self.live_thinking_rows,
        );
        self.live_thinking_visual_start = self.visual_lines.iter().position(|line| line.synthetic);
        if !self.live_thinking_clickable
            && let Some(visual) = self
                .live_thinking_visual_start
                .and_then(|index| self.visual_lines.get_mut(index))
        {
            visual.interaction = None;
        }
        self.viewport = viewport;
        self.width = width;
        self.scroll = self.scroll.min(self.max_scroll());
        self
    }

    pub fn set_live_thinking_clickable(&mut self, clickable: bool) {
        if self.live_thinking_clickable == clickable {
            return;
        }
        self.live_thinking_clickable = clickable;
        // Update the interaction on the existing synthetic first row so a
        // transition to/from the generating-tool row also toggles clickability
        // on the fast path (when the row count is unchanged).
        if let Some(visual) = self
            .live_thinking_visual_start
            .and_then(|index| self.visual_lines.get_mut(index))
        {
            visual.interaction = if clickable {
                Some(InteractionTarget::Thinking)
            } else {
                None
            };
        }
    }

    pub fn set_live_thinking_lines(&mut self, lines: Vec<String>) {
        if self.live_thinking_rows == lines.len() {
            if let Some(start) = self.live_thinking_visual_start {
                for (index, visual) in self.visual_lines[start..start + lines.len()]
                    .iter_mut()
                    .enumerate()
                {
                    visual.clickable_width = if index == 0 {
                        lines
                            .first()
                            .map(|line| UnicodeWidthStr::width(line.as_str()).min(self.width))
                            .unwrap_or(0)
                    } else {
                        0
                    };
                }
            }
            self.live_thinking_lines = lines;
            return;
        }
        let previous_start = self.live_thinking_visual_start.take();
        if let Some(start) = previous_start {
            self.visual_lines
                .drain(start..start + self.live_thinking_rows);
        }
        if let Some(logical_line) = self.live_thinking_before {
            let insertion = previous_start.unwrap_or_else(|| {
                self.visual_lines
                    .iter()
                    .position(|line| line.logical_line >= logical_line)
                    .unwrap_or(self.visual_lines.len())
            });
            self.visual_lines.splice(
                insertion..insertion,
                lines.iter().enumerate().map(|(live_row, line)| VisualLine {
                    logical_line: logical_line.min(self.lines.len().saturating_sub(1)),
                    start: live_row,
                    end: live_row,
                    interaction: (live_row == 0 && self.live_thinking_clickable)
                        .then_some(InteractionTarget::Thinking),
                    synthetic: true,
                    clickable_width: if live_row == 0 {
                        UnicodeWidthStr::width(line.as_str()).min(self.width)
                    } else {
                        0
                    },
                }),
            );
            self.live_thinking_visual_start = (!lines.is_empty()).then_some(insertion);
        }
        self.live_thinking_rows = lines.len();
        self.live_thinking_lines = lines;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn set_live_thinking_title(&mut self, title: String) {
        let Some(first) = self.live_thinking_lines.first_mut() else {
            return;
        };
        if *first == title {
            return;
        }
        *first = title;
        if let Some(visual) = self
            .live_thinking_visual_start
            .and_then(|index| self.visual_lines.get_mut(index))
        {
            visual.clickable_width = UnicodeWidthStr::width(first.as_str()).min(self.width);
        }
    }

    pub fn hit_test(&self, column: u16, row: u16) -> Option<usize> {
        if self.viewport.width == 0
            || self.viewport.height == 0
            || column < self.viewport.x
            || column >= self.viewport.right()
            || row < self.viewport.y
            || row >= self.viewport.bottom()
        {
            return None;
        }
        let visual_row = self.scroll + usize::from(row - self.viewport.y);
        self.position_at_visual_row(visual_row, column.saturating_sub(self.viewport.x) as usize)
    }

    pub fn interaction_at(&self, column: u16, row: u16) -> Option<InteractionTarget> {
        if self.viewport.width == 0
            || self.viewport.height == 0
            || column < self.viewport.x
            || column >= self.viewport.right()
            || row < self.viewport.y
            || row >= self.viewport.bottom()
        {
            return None;
        }
        let visual_row = self.scroll + usize::from(row - self.viewport.y);
        let visual = self.visual_lines.get(visual_row)?;
        let target = visual.interaction.clone()?;
        let local_column = usize::from(column - self.viewport.x);
        (local_column < visual.clickable_width).then_some(target)
    }

    pub fn position_at_visual_row(&self, visual_row: usize, column: usize) -> Option<usize> {
        let row = self.visual_lines.get(visual_row)?;
        if row.synthetic {
            return None;
        }
        let line = self.lines.get(row.logical_line)?;
        let local_start = row.start.saturating_sub(line.start);
        let local_end = row.end.saturating_sub(line.start);
        let slice = &line.text[local_start..local_end];
        let mut used = 0usize;
        for (offset, grapheme) in slice.grapheme_indices(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if grapheme_width == 0 {
                continue;
            }
            let next = used.saturating_add(grapheme_width);
            if column < next {
                return Some(line.start + local_start + offset);
            }
            used = next;
        }
        Some(row.end)
    }

    pub fn selected_text(&self, selection: OutputSelection) -> Option<&str> {
        let (start, end) = selection.range()?;
        self.text.get(start..end)
    }
}

fn build_visual_lines(
    lines: &[LayoutLine],
    width: usize,
    live_thinking_before: Option<usize>,
    live_thinking_rows: usize,
) -> Vec<VisualLine> {
    let mut visual_lines = Vec::new();
    let insertion = live_thinking_before.map(|index| index.min(lines.len()));
    for logical_line in 0..=lines.len() {
        if crate::layout_work::cancelled() {
            break;
        }
        if insertion == Some(logical_line) {
            visual_lines.extend((0..live_thinking_rows).map(|live_row| VisualLine {
                logical_line: logical_line.min(lines.len().saturating_sub(1)),
                start: live_row,
                end: live_row,
                interaction: (live_row == 0).then_some(InteractionTarget::Thinking),
                synthetic: true,
                clickable_width: 0,
            }));
        }
        if let Some(line) = lines.get(logical_line) {
            visual_lines.extend(wrap_line(logical_line, line, width));
        }
    }
    visual_lines
}

fn wrap_line(logical_line: usize, line: &LayoutLine, width: usize) -> Vec<VisualLine> {
    if matches!(
        line.interaction,
        Some(
            InteractionTarget::Tool(_)
                | InteractionTarget::ThinkingSummary(_)
                | InteractionTarget::Todo(_)
                | InteractionTarget::TodoToggle
                | InteractionTarget::TodoClose,
        )
    ) {
        let mut used = 0usize;
        let mut end = 0usize;
        for (offset, grapheme) in line.text.grapheme_indices(true) {
            if crate::layout_work::cancelled() {
                break;
            }
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if used.saturating_add(grapheme_width) > width {
                break;
            }
            used = used.saturating_add(grapheme_width);
            end = offset + grapheme.len();
        }
        return vec![VisualLine {
            logical_line,
            start: line.start,
            end: line.start + end,
            interaction: line.interaction.clone(),
            synthetic: false,
            clickable_width: match line.interaction {
                Some(InteractionTarget::Todo(_)) => 1,
                _ => used,
            },
        }];
    }
    if line.text.is_empty() {
        return vec![VisualLine {
            logical_line,
            start: line.start,
            end: line.start,
            interaction: line.interaction.clone(),
            synthetic: false,
            clickable_width: 0,
        }];
    }

    let mut rows = Vec::new();
    let mut row_start = 0usize;
    let mut row_width = 0usize;
    for (offset, grapheme) in line.text.grapheme_indices(true) {
        if crate::layout_work::cancelled() {
            break;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if row_width > 0 && row_width.saturating_add(grapheme_width) > width {
            rows.push(VisualLine {
                logical_line,
                start: line.start + row_start,
                end: line.start + offset,
                interaction: if rows.is_empty() {
                    line.interaction.clone()
                } else {
                    None
                },
                synthetic: false,
                clickable_width: 0,
            });
            row_start = offset;
            row_width = 0;
        }
        row_width = row_width.saturating_add(grapheme_width);
    }
    rows.push(VisualLine {
        logical_line,
        start: line.start + row_start,
        end: line.start + line.text.len(),
        interaction: if rows.is_empty() {
            line.interaction.clone()
        } else {
            None
        },
        synthetic: false,
        clickable_width: 0,
    });
    rows
}

#[cfg(test)]
mod tests {
    use ratatui::{
        layout::Rect,
        text::{Line, Span},
    };

    use super::*;

    fn layout(text: &str, width: u16) -> MessageLayout {
        let lines = text
            .split('\n')
            .map(|line| Line::from(Span::raw(line.to_owned())))
            .collect();
        MessageLayout::new(lines, Rect::new(2, 3, width, 5), 0)
    }

    #[test]
    fn cached_segments_compose_to_the_same_text_and_visual_rows_as_a_full_layout() {
        let viewport = Rect::new(2, 3, 5, 5);
        let first_lines = vec![Line::from(Span::raw("alpha 中".to_owned()))];
        let first_interactions = vec![None];
        let second_lines = vec![
            Line::from(Span::raw("wide 🙂 text".to_owned())),
            Line::default(),
        ];
        let second_interactions = vec![None, None];
        let full = MessageLayout::new_with_interactions(
            [first_lines.clone(), second_lines.clone()].concat(),
            [first_interactions.clone(), second_interactions.clone()].concat(),
            viewport,
            0,
            None,
        );
        let segments = [
            CachedLayoutSegment::new(1, 5, 0, first_lines, first_interactions),
            CachedLayoutSegment::new(3, 5, 0, second_lines, second_interactions),
        ];
        let segmented = MessageLayout::from_segments(&segments, viewport, 0, None);

        assert_eq!(segmented.text, full.text);
        let line_shape = |layout: &MessageLayout| {
            layout
                .lines
                .iter()
                .map(|line| {
                    (
                        line.text.clone(),
                        line.style_runs.clone(),
                        line.start,
                        line.interaction.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(line_shape(&segmented), line_shape(&full));
        assert_eq!(segmented.visual_lines, full.visual_lines);
        assert_eq!(segmented.hit_test(3, 3), full.hit_test(3, 3));
    }

    #[test]
    fn segment_updates_preserve_prefix_and_match_fresh_layout_with_live_rows() {
        let viewport = Rect::new(2, 3, 7, 5);
        let segment = |revision, text: &str| {
            CachedLayoutSegment::new(
                revision,
                7,
                0,
                text.split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
                vec![],
            )
        };
        let mut segments = vec![segment(1, "stable 中文\n🙂"), segment(2, "old tail")];
        let mut incremental = MessageLayout::from_segments(&segments, viewport, 0, Some(2));
        let prefix_text = incremental.lines[0].text.as_ptr();
        incremental.set_live_thinking_lines(vec!["thinking".into(), "more".into()]);
        segments[1] = segment(3, "new tail\nlong e\u{301} 👩‍💻 text");
        incremental.update_segments(&segments, viewport, Some(2));
        assert_eq!(incremental.lines[0].text.as_ptr(), prefix_text);
        for change in 0..4 {
            match change {
                1 => {
                    segments.insert(0, segment(4, "prepended"));
                }
                2 => {
                    segments.truncate(1);
                }
                3 => {
                    segments.clear();
                }
                _ => {}
            }
            incremental.update_segments(&segments, viewport, Some(0));
            incremental.set_live_thinking_lines(vec!["thinking".into()]);
            let mut fresh = MessageLayout::from_segments(&segments, viewport, 0, Some(0));
            fresh.set_live_thinking_lines(vec!["thinking".into()]);
            assert_eq!(incremental.text, fresh.text);
            assert_eq!(incremental.visual_lines, fresh.visual_lines);
            assert_eq!(incremental.lines.len(), fresh.lines.len());
            for row in 0..fresh.visual_lines.len() {
                for column in 0..7 {
                    assert_eq!(
                        incremental.position_at_visual_row(row, column),
                        fresh.position_at_visual_row(row, column)
                    );
                }
            }
            assert_eq!(
                incremental.selected_text(OutputSelection {
                    anchor: 0,
                    active: incremental.text.len(),
                    dragging: false
                }),
                fresh.selected_text(OutputSelection {
                    anchor: 0,
                    active: fresh.text.len(),
                    dragging: false
                })
            );
        }
    }

    #[test]
    fn copies_logical_newlines_without_wrap_newlines() {
        let layout = layout("abcdef", 3);
        assert_eq!(layout.text, "abcdef");
        assert_eq!(layout.visual_lines.len(), 2);
        assert_eq!(
            layout.selected_text(OutputSelection {
                anchor: 1,
                active: 5,
                dragging: false
            }),
            Some("bcde")
        );
    }

    #[test]
    fn hit_testing_keeps_utf8_and_grapheme_boundaries() {
        let text = "a中e\u{301}🙂";
        let layout = layout(text, 20);
        let is_grapheme_boundary = |offset| {
            offset == text.len()
                || text
                    .grapheme_indices(true)
                    .any(|(start, _)| start == offset)
        };
        for column in 0..=6 {
            let offset = layout.hit_test(2 + column, 3).unwrap();
            assert!(layout.text.is_char_boundary(offset));
            assert!(is_grapheme_boundary(offset));
        }
        assert_eq!(layout.hit_test(2, 3), Some(0));
        assert_eq!(layout.hit_test(3, 3), Some(1));
        assert_eq!(layout.hit_test(4, 3), Some(1));
        assert_eq!(layout.hit_test(5, 3), Some("a中".len()));
        assert_eq!(layout.hit_test(6, 3), Some("a中e\u{301}".len()));
        assert_eq!(layout.hit_test(7, 3), Some("a中e\u{301}".len()));
        assert_eq!(layout.hit_test(8, 3), Some("a中e\u{301}🙂".len()));
        assert_eq!(layout.hit_test(9, 3), Some("a中e\u{301}🙂".len()));
    }

    #[test]
    fn hit_testing_handles_reverse_and_blank_lines() {
        let layout = layout("first\n\nlast", 20);
        let start = layout.hit_test(2, 3).unwrap();
        let end = layout.hit_test(2, 5).unwrap();
        assert_eq!(&layout.text[start..end], "first\n\n");
    }

    #[test]
    fn height_changes_preserve_wrapping_and_width_changes_reflow() {
        let mut layout = layout("abcdef", 3);
        let original_lines = layout.lines.len();
        let original_visual_lines = layout.visual_lines.clone();
        layout.update_viewport(Rect::new(2, 3, 3, 2));
        assert_eq!(layout.lines.len(), original_lines);
        assert_eq!(layout.visual_lines, original_visual_lines);

        let layout = layout.reflow(Rect::new(2, 3, 6, 2));
        assert_eq!(layout.lines.len(), original_lines);
        assert_eq!(layout.visual_lines.len(), 1);
        assert_eq!(layout.text, "abcdef");
    }

    #[test]
    fn scrolled_selection_preserves_chinese_and_emoji_boundaries() {
        let mut layout = MessageLayout::new(
            ["top", "中🙂", "bottom"]
                .into_iter()
                .map(|line| Line::from(Span::raw(line.to_owned())))
                .collect(),
            Rect::new(2, 3, 20, 1),
            0,
        );
        layout.set_scroll(1);
        let chinese = layout.hit_test(2, 3).unwrap();
        let inside_chinese = layout.hit_test(3, 3).unwrap();
        let emoji = layout.hit_test(4, 3).unwrap();
        let end = layout.hit_test(6, 3).unwrap();
        assert_eq!(chinese, inside_chinese);
        assert_eq!(&layout.text[chinese..emoji], "中");
        assert_eq!(&layout.text[emoji..end], "🙂");
        assert_eq!(
            layout.selected_text(OutputSelection {
                anchor: chinese,
                active: end,
                dragging: false,
            }),
            Some("中🙂")
        );
    }

    fn interactive_layout(viewport: Rect) -> MessageLayout {
        MessageLayout::new_with_interactions(
            vec![
                Line::from("▸ 文件读取  src/app.rs  ✓"),
                Line::from("▸ 文件搜索  thinking  ✓"),
                Line::from("▸ 文件修改  src/ui.rs  ✓"),
            ],
            vec![
                Some(InteractionTarget::Tool("first".into())),
                Some(InteractionTarget::Tool("second".into())),
                Some(InteractionTarget::Tool("third".into())),
            ],
            viewport,
            0,
            None,
        )
    }

    #[test]
    fn interaction_checks_real_viewport_rows_columns_and_wide_text() {
        let viewport = Rect::new(30, 1, 80, 18);
        let layout = interactive_layout(viewport);
        assert_eq!(
            layout.interaction_at(30, 1),
            Some(InteractionTarget::Tool("first".into()))
        );
        assert_eq!(
            layout.interaction_at(30, 2),
            Some(InteractionTarget::Tool("second".into()))
        );
        assert_eq!(
            layout.interaction_at(30, 3),
            Some(InteractionTarget::Tool("third".into()))
        );
        assert_eq!(layout.interaction_at(30, 0), None);
        assert_eq!(layout.interaction_at(30, 4), None);

        let visible_width = layout.visual_lines[0].clickable_width as u16;
        assert_eq!(
            layout.interaction_at(30 + visible_width - 1, 1),
            Some(InteractionTarget::Tool("first".into()))
        );
        assert_eq!(layout.interaction_at(30 + visible_width, 1), None);
        assert_eq!(layout.interaction_at(29, 1), None);
        assert_eq!(layout.interaction_at(110, 1), None);
    }

    #[test]
    fn interaction_maps_scrolled_rows_and_clipped_tool_width() {
        let mut layout = interactive_layout(Rect::new(0, 5, 12, 1));
        layout.set_scroll(1);
        assert_eq!(
            layout.interaction_at(0, 5),
            Some(InteractionTarget::Tool("second".into()))
        );
        assert_eq!(layout.visual_lines[1].clickable_width, 12);
        assert_eq!(
            layout.interaction_at(11, 5),
            Some(InteractionTarget::Tool("second".into()))
        );
        assert_eq!(layout.interaction_at(12, 5), None);
    }

    #[test]
    fn synthetic_thinking_click_width_updates_without_reflow() {
        let mut layout = MessageLayout::new_with_live_thinking(
            vec![Line::from("正文")],
            Rect::new(30, 1, 40, 5),
            0,
            Some(0),
        );
        layout.set_live_thinking_lines(vec!["⠋ 思考中  正在分析项目".into()]);
        let width = UnicodeWidthStr::width("⠋ 思考中  正在分析项目") as u16;
        assert_eq!(
            layout.interaction_at(30 + width - 1, 1),
            Some(InteractionTarget::Thinking)
        );
        assert_eq!(layout.interaction_at(30 + width, 1), None);

        layout.set_live_thinking_lines(vec!["▾ ⠙ 思考中".into(), "  正在分析项目".into()]);
        assert_eq!(
            layout.interaction_at(30, 1),
            Some(InteractionTarget::Thinking)
        );
        assert_eq!(layout.interaction_at(30, 2), None);
    }

    #[test]
    fn generating_tool_row_is_display_only_and_not_clickable() {
        let mut layout = MessageLayout::new_with_live_thinking(
            vec![Line::from("正文")],
            Rect::new(30, 1, 40, 5),
            0,
            Some(0),
        );
        // The facade clears clickability for the "generating tool call" row;
        // the fast path (unchanged row count) must keep it non-clickable.
        layout.set_live_thinking_clickable(false);
        layout.set_live_thinking_lines(vec!["⚙ ⠋ 生成工具调用 文件修改 · 9.0 KB".into()]);
        assert!(!layout.live_thinking_clickable);
        assert_eq!(
            layout.interaction_at(30, 1),
            None,
            "the generating row must not be clickable"
        );
        // Restoring clickability (thinking row returns) re-enables the target.
        layout.set_live_thinking_clickable(true);
        assert_eq!(
            layout.interaction_at(30, 1),
            Some(InteractionTarget::Thinking)
        );
    }

    #[test]
    fn thinking_visual_index_survives_row_count_changes_and_reflow() {
        let lines = (0..200)
            .map(|index| Line::from(format!("row {index}")))
            .collect();
        let mut layout =
            MessageLayout::new_with_live_thinking(lines, Rect::new(0, 0, 20, 8), 0, Some(100));
        let start = layout.live_thinking_visual_start.unwrap();
        assert_eq!(layout.visual_lines[start].logical_line, 100);
        layout.set_live_thinking_lines(vec!["思考中".into(), "第二行".into()]);
        assert_eq!(layout.live_thinking_visual_start, Some(start));
        layout.set_live_thinking_clickable(false);
        layout.set_live_thinking_title("分析中".into());
        assert_eq!(layout.visual_lines[start].interaction, None);
        assert_eq!(
            layout.visual_lines[start].clickable_width,
            UnicodeWidthStr::width("分析中")
        );
        layout = layout.reflow(Rect::new(0, 0, 10, 8));
        let after_reflow = layout.live_thinking_visual_start.unwrap();
        assert!(layout.visual_lines[after_reflow].synthetic);
        assert_eq!(layout.visual_lines[after_reflow].interaction, None);
        layout.set_live_thinking_lines(vec!["完成".into()]);
        assert_eq!(layout.live_thinking_visual_start, Some(after_reflow));
        assert!(layout.visual_lines[after_reflow].synthetic);
        assert!(!layout.visual_lines[after_reflow + 1].synthetic);
    }
}
