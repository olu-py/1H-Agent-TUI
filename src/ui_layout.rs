use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders},
};

pub const WIDE_MIN_WIDTH: u16 = 100;
pub const STANDARD_MIN_WIDTH: u16 = 70;
pub const SHORT_MAX_HEIGHT: u16 = 15;
pub const SIDEBAR_WIDTH: u16 = 30;
pub const INPUT_HEIGHT: u16 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    Compact,
    Standard,
    Wide,
}

impl Density {
    pub fn from_width(width: u16) -> Self {
        if width >= WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= STANDARD_MIN_WIDTH {
            Self::Standard
        } else {
            Self::Compact
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightClass {
    Short,
    Normal,
}

impl HeightClass {
    pub fn from_height(height: u16) -> Self {
        if height <= SHORT_MAX_HEIGHT {
            Self::Short
        } else {
            Self::Normal
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiLayout {
    pub sessions: Option<Rect>,
    pub messages_outer: Rect,
    pub messages_inner: Rect,
    pub input: Rect,
    pub footer: Rect,
}

pub fn message_block() -> Block<'static> {
    Block::default().title(" 任务 ").borders(Borders::BOTTOM)
}

pub fn compute_layout(area: Rect, density: Density, height: HeightClass) -> UiLayout {
    let footer_height = if height == HeightClass::Short { 1 } else { 2 };
    let input_height = INPUT_HEIGHT.min(area.height.saturating_sub(footer_height));
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(input_height),
            Constraint::Length(footer_height.min(area.height)),
        ])
        .split(area);
    let content = vertical[0];
    let (sessions, messages_outer) = if density == Density::Wide {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_WIDTH.min(content.width)),
                Constraint::Min(0),
            ])
            .split(content);
        (Some(horizontal[0]), horizontal[1])
    } else {
        (None, content)
    };
    let messages_inner = message_block().inner(messages_outer);
    UiLayout {
        sessions,
        messages_outer,
        messages_inner,
        input: vertical[1],
        footer: vertical[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn responsive_rects_are_bounded_and_use_block_inner() {
        for area in [
            Rect::new(0, 0, 120, 30),
            Rect::new(0, 0, 80, 20),
            Rect::new(0, 0, 44, 14),
            Rect::new(0, 0, 2, 2),
            Rect::new(0, 0, 0, 0),
        ] {
            let density = Density::from_width(area.width);
            let height = HeightClass::from_height(area.height);
            let layout = compute_layout(area, density, height);
            assert!(inside(layout.messages_outer, area));
            assert!(inside(layout.messages_inner, layout.messages_outer));
            assert!(inside(layout.input, area));
            assert!(inside(layout.footer, area));
            assert_eq!(
                layout.messages_inner,
                message_block().inner(layout.messages_outer)
            );
            assert!(layout.messages_outer.bottom() <= layout.input.y);
            assert!(layout.input.bottom() <= layout.footer.y);
            assert_eq!(layout.sessions.is_some(), density == Density::Wide);
            assert_eq!(
                layout.footer.height,
                if height == HeightClass::Short { 1 } else { 2 }.min(area.height)
            );
        }
    }

    #[test]
    fn density_thresholds_are_centralized() {
        assert_eq!(Density::from_width(69), Density::Compact);
        assert_eq!(Density::from_width(70), Density::Standard);
        assert_eq!(Density::from_width(99), Density::Standard);
        assert_eq!(Density::from_width(100), Density::Wide);
    }
}

/// Item rows a footer picker paints before its window starts scrolling.
pub const PICKER_MAX_ROWS: usize = 12;

/// Border rows (top + bottom) a picker popup always spends.
const PICKER_BORDER_ROWS: u16 = 2;

/// Geometry of a footer picker popup (provider, model, thinking level).
///
/// A picker is anchored to its footer control and grows upward, so it can
/// never cover the footer it belongs to nor run past the top of the screen.
/// `scroll`/`visible` describe the only item window that is painted: the
/// painter and the mouse hit-test both resolve rows through here, because a
/// rendered row and a clicked row drift apart as soon as the list scrolls or
/// the window is shorter than the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickerGeometry {
    /// Bordered rectangle that is cleared and painted for the popup.
    pub area: Rect,
    /// Index of the first painted item.
    pub scroll: usize,
    /// Number of painted item rows.
    pub visible: usize,
}

impl PickerGeometry {
    /// Builds the popup window for `items` entries anchored at `anchor_x`
    /// (the control's left column) keeping `selected` inside the window.
    pub fn new(
        screen: Rect,
        footer: Rect,
        anchor_x: u16,
        width: u16,
        items: usize,
        selected: usize,
    ) -> Self {
        let width = width.min(screen.width);
        let rows = footer.y.saturating_sub(screen.y) as usize;
        let visible = items
            .min(PICKER_MAX_ROWS)
            .min(rows.saturating_sub(usize::from(PICKER_BORDER_ROWS)));
        let scroll = selected
            .saturating_sub(visible.saturating_sub(1))
            .min(items.saturating_sub(visible));
        let height = (visible as u16).saturating_add(PICKER_BORDER_ROWS);
        Self {
            area: Rect::new(
                anchor_x
                    .max(screen.x)
                    .min(screen.right().saturating_sub(width)),
                footer.y.saturating_sub(height),
                width,
                height,
            ),
            scroll,
            visible,
        }
    }

    /// The bordered block's content rows: exactly the rows items paint on.
    pub fn inner(&self) -> Rect {
        Block::default().borders(Borders::ALL).inner(self.area)
    }

    /// True when the terminal cell touches the popup at all: a click on the
    /// frame dismisses the picker without applying anything.
    pub fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.area.x
            && column < self.area.right()
            && row >= self.area.y
            && row < self.area.bottom()
    }

    /// Absolute index of the item painted at (`column`, `row`), or `None` when
    /// the cell is outside the popup or on a row that paints no item.
    pub fn item_at(&self, column: u16, row: u16) -> Option<usize> {
        let inner = self.inner();
        if !self.contains(column, row)
            || column < inner.x
            || column >= inner.right()
            || row < inner.y
            || row >= inner.bottom()
        {
            return None;
        }
        // `visible` is capped by the item count, so a painted row always
        // addresses a real item.
        let offset = (row - inner.y) as usize;
        (offset < self.visible).then(|| self.scroll + offset)
    }
}

#[cfg(test)]
mod picker_geometry_tests {
    use super::{PICKER_MAX_ROWS, PickerGeometry};
    use ratatui::layout::Rect;

    fn popup(items: usize, selected: usize) -> PickerGeometry {
        // A 100x20 screen whose footer owns rows 18/19, anchored at column 40.
        PickerGeometry::new(
            Rect::new(0, 0, 100, 20),
            Rect::new(0, 18, 100, 2),
            40,
            24,
            items,
            selected,
        )
    }

    #[test]
    fn a_short_popup_stays_above_its_footer_and_inside_the_screen() {
        let picker = popup(5, 0);
        assert_eq!(picker.visible, 5, "a short list paints every row");
        assert_eq!(picker.area.bottom(), 18, "the footer is never covered");
        assert!(picker.area.y > 0);
        assert_eq!(picker.area.x, 40);
        assert!(picker.area.right() <= 100);
    }

    #[test]
    fn item_at_addresses_the_row_the_painter_scrolled_into_view() {
        let picker = popup(20, 19);
        let inner = picker.inner();
        assert_eq!(picker.visible, PICKER_MAX_ROWS);
        assert_eq!(picker.scroll, 20 - PICKER_MAX_ROWS);
        assert_eq!(picker.item_at(inner.x, inner.y), Some(8));
        assert_eq!(picker.item_at(inner.x, inner.bottom() - 1), Some(19));
        assert_eq!(
            picker.item_at(inner.right() - 1, inner.bottom() - 1),
            Some(19)
        );
    }

    #[test]
    fn cells_outside_the_item_window_resolve_nothing() {
        let picker = popup(20, 3);
        let inner = picker.inner();
        assert_eq!(picker.scroll, 0);
        assert_eq!(
            picker.item_at(inner.x, inner.bottom()),
            None,
            "bottom frame"
        );
        assert_eq!(
            picker.item_at(picker.area.x, picker.area.y),
            None,
            "top frame"
        );
        assert_eq!(
            picker.item_at(inner.x - 1, inner.y),
            None,
            "left of the frame"
        );
        assert_eq!(
            picker.item_at(inner.right(), inner.y),
            None,
            "right of the frame"
        );
        assert!(!picker.contains(picker.area.right(), inner.y));
    }

    #[test]
    fn a_screen_too_short_for_one_row_resolves_no_items() {
        // A two-row terminal leaves no room above the footer: the popup must
        // stay on screen and offer nothing clickable instead of painting rows
        // that do not exist.
        let picker =
            PickerGeometry::new(Rect::new(0, 0, 60, 2), Rect::new(0, 1, 60, 1), 20, 30, 8, 3);
        assert_eq!(picker.visible, 0);
        assert!(picker.area.bottom() <= 2, "the popup stays on screen");
        assert!((0..60).all(|row| (0..60).all(|column| { picker.item_at(column, row).is_none() })));
    }

    #[test]
    fn one_row_window_still_paints_the_cursor_on_a_stumped_screen() {
        // Only the footer plus a single item row fits above it.
        let picker = PickerGeometry::new(
            Rect::new(0, 0, 60, 4),
            Rect::new(0, 3, 60, 1),
            20,
            30,
            40,
            39,
        );
        assert_eq!(picker.visible, 1);
        assert_eq!(picker.area.bottom(), 3);
        assert_eq!(picker.item_at(picker.inner().x, picker.inner().y), Some(39));
    }
}
