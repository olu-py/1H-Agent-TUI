use super::*;

pub(super) async fn handle_terminal_event(app: &mut App, event: Event) -> Result<EventOutcome> {
    if let Event::Paste(text) = &event {
        if app.settings.is_some() {
            return Ok(if paste_text_into_settings(app, text) {
                EventOutcome::redraw()
            } else {
                EventOutcome::default()
            });
        }
        if !app.current.busy && app.palette.is_none() {
            app.input.insert_str(text);
            update_file_suggestions(app);
            return Ok(EventOutcome::redraw());
        }
        return Ok(EventOutcome::default());
    }
    if let Event::Mouse(mouse) = event {
        if let Some(outcome) = handle_settings_mouse(app, mouse)? {
            return Ok(outcome);
        }
        if let Some(outcome) = handle_thinking_mouse(app, mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = handle_provider_mouse(app, mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = handle_model_mouse(app, mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = handle_navigation_mouse(app, mouse).await? {
            return Ok(outcome);
        }
        if output_mouse_event_allowed(
            mouse.kind,
            app.settings.is_some(),
            app.palette.is_some(),
            app.has_pending_approval(),
        ) {
            return Ok(handle_output_mouse(app, mouse));
        }
        return Ok(EventOutcome::default());
    }
    if matches!(event, Event::Resize(_, _)) {
        if app.has_pending_approval()
            || app.settings.is_some()
            || app.palette.is_some()
            || app.thinking_menu_open
            || app.provider_menu_open
            || app.model_menu_open
        {
            app.force_full_redraw = true;
        }
        return Ok(EventOutcome::redraw());
    }
    let Event::Key(key) = event else {
        return Ok(EventOutcome::default());
    };
    if key.kind != KeyEventKind::Press {
        return Ok(EventOutcome::default());
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return Ok(EventOutcome::default());
    }
    if app.has_pending_approval() {
        let redraw = matches!(
            key.code,
            KeyCode::Char('y')
                | KeyCode::Char('Y')
                | KeyCode::Char('n')
                | KeyCode::Char('N')
                | KeyCode::Char('a')
                | KeyCode::Char('A')
                | KeyCode::Esc
        );
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.resolve_approval(ApprovalChoice::Approve).await?
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.resolve_approval(ApprovalChoice::Reject).await?
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                app.resolve_approval(ApprovalChoice::AlwaysSession).await?
            }
            _ => {}
        }
        return Ok(EventOutcome {
            redraw,
            osc52: None,
        });
    }
    if app.settings.is_some() {
        let redraw = settings_key_handled(key.code, key.modifiers);
        handle_settings_key(app, key.code, key.modifiers).await;
        return Ok(EventOutcome {
            redraw,
            osc52: None,
        });
    }
    if app.provider_menu_open {
        let redraw = provider_menu_key_handled(key.code);
        handle_provider_menu_key(app, key.code).await?;
        return Ok(EventOutcome {
            redraw,
            osc52: None,
        });
    }
    if app.model_menu_open {
        let redraw = model_menu_key_handled(key.code);
        handle_model_menu_key(app, key.code).await?;
        return Ok(EventOutcome {
            redraw,
            osc52: None,
        });
    }
    if app.palette.is_some() {
        let redraw = palette_key_handled(key.code, key.modifiers);
        handle_palette_key(app, key.code, key.modifiers).await;
        return Ok(EventOutcome {
            redraw,
            osc52: None,
        });
    }
    if app.thinking_menu_open {
        let selected = app
            .thinking_menu_rect
            .and_then(|rect| thinking_menu_selection(app, rect, u16::MAX, u16::MAX));
        app.thinking_menu_open = false;
        app.force_full_redraw = true;
        if let Some((level, budget)) = selected {
            apply_thinking_selection(app, level, budget).await?;
        }
        return Ok(EventOutcome::redraw());
    }

    let redraw = match key.code {
        KeyCode::Char('p' | 'x')
            if key.modifiers.contains(KeyModifiers::CONTROL) && !app.current.busy =>
        {
            open_palette(app);
            true
        }
        KeyCode::PageUp if !app.current.busy => app.current.scroll_messages(5),
        KeyCode::PageDown if !app.current.busy => app.current.scroll_messages(-5),
        KeyCode::Up if !app.current.busy && key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.current.scroll_messages(3)
        }
        KeyCode::Down if !app.current.busy && key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.current.scroll_messages(-3)
        }
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.current.scroll_to_bottom();
            true
        }
        KeyCode::PageUp if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.current.scroll_messages(5)
        }
        KeyCode::PageDown if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.current.scroll_messages(-5)
        }
        KeyCode::Char('s')
            if key.modifiers.contains(KeyModifiers::CONTROL) && !app.current.busy =>
        {
            open_settings(app).await;
            true
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.create_session().await?;
            true
        }
        KeyCode::Up if session_switch_direction(&key) == Some(-1) => {
            app.switch_session_direction(-1).await?;
            true
        }
        KeyCode::Down if session_switch_direction(&key) == Some(1) => {
            app.switch_session_direction(1).await?;
            true
        }
        KeyCode::Esc => {
            if app.current.busy || app.current.pending_approval.is_some() {
                app.cancel().await?;
            }
            true
        }
        KeyCode::Tab if !app.current.busy && !app.file_suggestions.is_empty() => {
            apply_file_completion(app);
            true
        }
        KeyCode::Enter if !app.current.busy && key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.insert('\n');
            app.file_suggestions.clear();
            true
        }
        KeyCode::Char('j')
            if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input.insert('\n');
            app.file_suggestions.clear();
            true
        }
        KeyCode::Enter if !app.current.busy => {
            app.submit_current().await?;
            true
        }
        KeyCode::Backspace if !app.current.busy => {
            app.input.backspace();
            update_file_suggestions(app);
            true
        }
        KeyCode::Delete if !app.current.busy => {
            app.input.delete();
            update_file_suggestions(app);
            true
        }
        KeyCode::Left if !app.current.busy && key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.select_left();
            true
        }
        KeyCode::Right if !app.current.busy && key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.select_right();
            true
        }
        KeyCode::Char('a')
            if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input.select_all();
            true
        }
        KeyCode::Left if !app.current.busy => {
            app.input.move_left();
            true
        }
        KeyCode::Right if !app.current.busy => {
            app.input.move_right();
            true
        }
        KeyCode::Home if !app.current.busy => {
            app.input.move_home();
            true
        }
        KeyCode::End if !app.current.busy => {
            app.input.move_end();
            true
        }
        KeyCode::Up
            if !app.current.busy
                && key.modifiers.is_empty()
                && !app.file_suggestions.is_empty() =>
        {
            app.file_selected = app.file_selected.saturating_sub(1);
            true
        }
        KeyCode::Down
            if !app.current.busy
                && key.modifiers.is_empty()
                && !app.file_suggestions.is_empty() =>
        {
            app.file_selected =
                (app.file_selected + 1).min(app.file_suggestions.len().saturating_sub(1));
            true
        }
        KeyCode::Up if !app.current.busy && key.modifiers.is_empty() => {
            if app.input.is_empty() {
                app.input.history_previous();
            } else {
                app.input.move_up();
            }
            true
        }
        KeyCode::Down if !app.current.busy && key.modifiers.is_empty() => {
            if app.input.is_empty() {
                app.input.history_next();
            } else {
                app.input.move_down();
            }
            true
        }
        KeyCode::Char('w')
            if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input.delete_word_left();
            true
        }
        KeyCode::Char('u')
            if !app.current.busy && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input.clear();
            true
        }
        KeyCode::Char(character)
            if !app.current.busy
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.input.insert(character);
            update_file_suggestions(app);
            true
        }
        _ => false,
    };
    Ok(EventOutcome {
        redraw,
        osc52: None,
    })
}

fn handle_settings_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    let Some(settings) = app.settings.as_mut() else {
        return Ok(None);
    };
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return Ok(Some(EventOutcome::default()));
    }
    let Some(rect) = app.settings_rect else {
        return Ok(Some(EventOutcome::default()));
    };
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(mouse.column, mouse.row, inner) {
        return Ok(Some(EventOutcome::default()));
    }
    let relative_row = mouse.row.saturating_sub(inner.y) as usize;
    match settings {
        SettingsState::List(list) => {
            let profile_start = 2usize;
            if relative_row >= profile_start && relative_row < profile_start + list.providers.len()
            {
                list.selected = relative_row - profile_start;
                open_selected_profile(app);
            } else if relative_row == profile_start + list.providers.len() + 1 {
                list.selected = list.providers.len();
                open_template_picker(app);
            }
        }
        SettingsState::Templates(templates) => {
            let start = 2usize;
            if relative_row >= start && relative_row < start + templates.presets.len() {
                templates.selected = relative_row - start;
                open_selected_template(app);
            }
        }
        SettingsState::Form(_) => {}
    }
    Ok(Some(EventOutcome::redraw()))
}

async fn handle_thinking_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return Ok(app.thinking_menu_open.then(EventOutcome::default));
    }
    if app.thinking_menu_open {
        let selected = app
            .thinking_menu_rect
            .filter(|rect| point_in_rect(mouse.column, mouse.row, *rect))
            .and_then(|rect| thinking_menu_selection(app, rect, mouse.column, mouse.row));
        app.thinking_menu_open = false;
        app.force_full_redraw = true;
        if let Some((level, budget)) = selected {
            apply_thinking_selection(app, level, budget).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    if !app.current.busy
        && !app.has_pending_approval()
        && app
            .thinking_control_rect
            .is_some_and(|rect| point_in_rect(mouse.column, mouse.row, rect))
    {
        app.model_menu_open = false;
        app.model_menu_rect = None;
        app.provider_menu_open = false;
        app.provider_menu_rect = None;
        app.thinking_menu_open = true;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}
