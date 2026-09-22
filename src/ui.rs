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

mod modal;
#[cfg(test)]
use modal::{SettingsRow, approval_lines, palette_item_text, settings_rows};
use modal::{
    argument_label, centered_rect, draw_approval, draw_file_suggestions, draw_palette,
    draw_settings, fit_text, human_argument,
};

mod chat;
use chat::draw_messages;
pub(crate) use chat::tool_display_name;
#[cfg(test)]
use chat::{
    render_diff, render_markdown, render_thinking_summary, render_tool, render_visual_line,
    wrap_grapheme_lines,
};

mod status;
#[cfg(test)]
use status::input_mode_rect;
use status::{draw_footer, draw_input, draw_model_menu, draw_provider_menu, draw_thinking_menu};

mod sidebar;
#[cfg(test)]
use sidebar::session_window_start;
use sidebar::{draw_sessions, draw_todo_window};
pub(crate) use sidebar::{
    flatten_session_tree, session_index_at, todo_control_columns, todo_task_index_at_row,
};

#[cfg(test)]
mod tests;
