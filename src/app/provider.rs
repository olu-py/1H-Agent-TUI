use super::*;
use crate::ui_view_model::{
    THINKING_LEVEL_COLUMN_WIDTH, ThinkingMenuCell, ThinkingMenuColumn, thinking_menu_columns,
    thinking_menu_rows,
};

/// Model picker entries for the active provider.
///
/// Merge order follows the parity plan: core dynamic list, preset static
/// fallback, current model, then models saved on other profiles of the same
/// preset. Deduping keys off the model id; the decorated label is display-only.
pub(crate) fn model_choices(app: &App) -> Vec<ModelChoice> {
    let preset = app.active_preset();
    let mut choices: Vec<ModelChoice> = Vec::new();
    let mut push = |id: String, window: Option<u64>| {
        let id = id.trim().to_owned();
        if id.is_empty() || choices.iter().any(|choice| choice.id == id) {
            return;
        }
        choices.push(ModelChoice {
            label: model_choice_label(&id, window),
            id,
        });
    };
    if app.provider_models.preset == Some(preset) {
        for model in &app.provider_models.models {
            push(model.id.clone(), model.context_window_tokens);
        }
    }
    for model in preset.selectable_models() {
        push((*model).to_owned(), None);
    }
    let current = app.active_model().to_owned();
    let current_window = app
        .provider_models
        .models
        .iter()
        .find(|model| model.id == current)
        .and_then(|model| model.context_window_tokens);
    push(current, current_window);
    if let Some(settings) = &app.provider_settings {
        for profile in &settings.saved {
            if profile.preset == preset.key_id() {
                push(profile.model.clone(), None);
            }
        }
    }
    choices
}

/// Formats a model picker label with a short context-window suffix.
fn model_choice_label(id: &str, window: Option<u64>) -> String {
    match window {
        Some(window) if window > 0 => format!("{id} · {}", compact_window(window)),
        _ => id.to_owned(),
    }
}

fn compact_window(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        format!("{}m", tokens / 1_000_000)
    } else if tokens >= 1_000 && tokens % 1_000 == 0 {
        format!("{}k", tokens / 1_000)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Provider switcher entries, derived from the core-authoritative settings
/// view when available: registry order, then connected/saved/active presets.
/// The local config is only a startup fallback before the first core read.
pub(crate) fn provider_choices(app: &App) -> Vec<ProviderPreset> {
    let mut choices: Vec<ProviderPreset> = Vec::new();
    let mut push = |preset: ProviderPreset| {
        if !choices.contains(&preset) {
            choices.push(preset);
        }
    };
    if let Some(settings) = &app.provider_settings {
        for preset in ProviderPreset::ALL {
            let connected = settings.connected.iter().any(|id| id == preset.key_id());
            let saved = settings
                .saved
                .iter()
                .any(|profile| profile.preset == preset.key_id());
            let active = settings.active.preset == preset.key_id();
            if connected || saved || active {
                push(preset);
            }
        }
    } else {
        for provider in &app.config.providers {
            push(provider.preset);
        }
    }
    let active = app.active_preset();
    if !choices.contains(&active) {
        choices.insert(0, active);
    }
    choices
}

/// Reads the core provider settings view (cache-only, never a network call).
pub(super) async fn refresh_provider_settings(app: &mut App) -> Result<()> {
    match app.handle.provider_settings().await {
        Ok(settings) => {
            app.provider_settings = Some(settings);
            sync_local_config_from_provider_settings(app);
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!(secrets::redact(&error.message))),
    }
}

/// Mirrors the core's active provider profile onto the local display config so
/// the settings form and footer seed from the same authority. The DTO omits
/// thinking/retry fields, so those local values are preserved.
fn sync_local_config_from_provider_settings(app: &mut App) {
    let Some((preset, model, base_url, kind)) = app.provider_settings.as_ref().map(|settings| {
        (
            ProviderPreset::parse(&settings.active.preset),
            settings.active.model.clone(),
            settings.active.base_url.clone(),
            ProviderKind::parse_wire_tag(&settings.active.kind),
        )
    }) else {
        return;
    };
    if let Some(preset) = preset {
        app.config.provider.preset = preset;
    }
    app.config.provider.model = model;
    if !base_url.trim().is_empty() {
        app.config.provider.base_url = base_url;
    }
    if let Some(kind) = kind {
        app.config.provider.kind = kind;
    }
}

/// Reads the provider model list. `refresh = false` is cache-only and safe to
/// await inline; `refresh = true` must go through [`spawn_model_refresh`].
pub(super) async fn load_provider_models(app: &mut App, refresh: bool) -> Result<()> {
    let preset = app.active_preset();
    let result = app.handle.provider_models(refresh).await;
    match result {
        Ok(models) => {
            app.provider_models = ProviderModelsState {
                preset: Some(preset),
                models: models.models,
                fetched_at: models.fetched_at,
                loading: false,
                last_error: None,
            };
            Ok(())
        }
        Err(error) => {
            let message = secrets::redact(&error.message);
            app.provider_models.loading = false;
            app.provider_models.last_error = Some(message.clone());
            Err(anyhow::anyhow!(message))
        }
    }
}

/// Starts a background `provider_models(true)` refresh so the terminal event
/// loop never blocks on the provider network round trip.
pub(super) fn spawn_model_refresh(app: &mut App) {
    if app.model_refresh_task.is_some() {
        return;
    }
    app.model_refresh_generation = app.model_refresh_generation.wrapping_add(1);
    let generation = app.model_refresh_generation;
    let preset = app.active_preset();
    app.provider_models.loading = true;
    app.provider_models.last_error = None;
    app.current.status = "模型列表刷新中……".into();
    let handle = app.handle.clone();
    let sender = app.model_refresh_tx.clone();
    app.model_refresh_task = Some(tokio::spawn(async move {
        let result = handle
            .provider_models(true)
            .await
            .map_err(|error| secrets::redact(&error.message));
        let _ = sender
            .send(ModelRefreshResult {
                generation,
                preset,
                result,
            })
            .await;
    }));
}

pub(super) fn apply_model_refresh_result(app: &mut App, refresh: ModelRefreshResult) {
    if refresh.generation == app.model_refresh_generation {
        // The send completed, so the task has no more work. Dropping the
        // handle releases it before a subsequent refresh is accepted.
        app.model_refresh_task.take();
    } else {
        // Never let an old task clear loading state belonging to a newer
        // provider/model selection.
        return;
    }
    // A provider switch may have landed while the refresh was in flight; a
    // stale answer must never overwrite the new provider's model list.
    if app.active_preset() != refresh.preset {
        return;
    }
    match refresh.result {
        Ok(models) => {
            app.provider_models = ProviderModelsState {
                preset: Some(refresh.preset),
                models: models.models,
                fetched_at: models.fetched_at,
                loading: false,
                last_error: None,
            };
            app.current.status = "模型列表已刷新".into();
        }
        Err(error) => {
            app.provider_models.loading = false;
            app.provider_models.last_error = Some(error);
            app.current.status = "模型刷新失败，已保留现有列表".into();
        }
    }
}

/// Which footer picker a click or key acts on. Only one picker is open at a
/// time, and opening one closes the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FooterMenu {
    Provider,
    Model,
    Thinking,
}

/// Closes every footer picker and drops the painted geometry with it, so a
/// rectangle from before a resize can never be hit-tested again.
pub(super) fn close_footer_menus(app: &mut App) {
    app.thinking_menu_open = false;
    app.thinking_menu_geometry = None;
    app.provider_menu_open = false;
    app.provider_menu_geometry = None;
    app.model_menu_open = false;
    app.model_menu_geometry = None;
}

/// Opens one footer picker and parks its cursor on the value that is applied
/// today, after re-reading the state the list is derived from. Enter then always
/// confirms a visible row instead of a leftover index from another provider or
/// model, and a click on the row the user sees is that same row.
pub(super) async fn open_footer_menu(app: &mut App, menu: FooterMenu) -> Result<()> {
    close_footer_menus(app);
    match menu {
        FooterMenu::Provider => {
            let _ = refresh_provider_settings(app).await;
            let active = app.active_preset();
            app.provider_menu_selected = provider_choices(app)
                .iter()
                .position(|preset| *preset == active)
                .unwrap_or(0);
            app.provider_menu_open = true;
        }
        FooterMenu::Model => {
            let _ = load_provider_models(app, false).await;
            let model = app.active_model().to_owned();
            app.model_menu_selected = model_choices(app)
                .iter()
                .position(|choice| choice.id == model)
                .unwrap_or(0);
            app.model_menu_open = true;
        }
        FooterMenu::Thinking => {
            let columns = thinking_menu_columns(app);
            let row = columns
                .first()
                .and_then(|column| column.cells.iter().position(|cell| cell.active))
                .unwrap_or(0);
            app.thinking_menu_cursor = ThinkingMenuCursor { row, column: 0 }
                .clamped(thinking_menu_rows(&columns), columns.len());
            app.thinking_menu_open = true;
        }
    }
    Ok(())
}

/// Moves a picker cursor with wrap-around, and clamps a stale index back onto
/// the live list so Enter can never index past the end of it.
fn move_menu_cursor(selected: &mut usize, code: KeyCode, items: usize) {
    if items == 0 {
        *selected = 0;
        return;
    }
    let last = items - 1;
    if *selected > last {
        // The list shrank while the picker stayed open: the next key reels the
        // cursor back onto the live list instead of wrapping it away from the
        // row the painter highlights, so Enter can never address a blank row.
        *selected = last;
        return;
    }
    *selected = match code {
        KeyCode::Up => (*selected + last) % items,
        KeyCode::Down => (*selected + 1) % items,
        _ => *selected,
    };
}

/// True when the click landed on a footer control whose text is fully visible
/// in this frame: a clipped label never opens a picker.
fn footer_control_hit(app: &App, mouse: &crossterm::event::MouseEvent, rect: Option<Rect>) -> bool {
    !app.current.busy
        && !app.has_pending_approval()
        && rect.is_some_and(|rect| point_in_rect(mouse.column, mouse.row, rect))
}

pub(super) async fn handle_provider_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return Ok(app.provider_menu_open.then(EventOutcome::default));
    }
    if app.provider_menu_open {
        let selected = app
            .provider_menu_geometry
            .filter(|picker| picker.contains(mouse.column, mouse.row))
            .and_then(|picker| provider_menu_selection(app, picker, mouse.column, mouse.row));
        close_footer_menus(app);
        if let Some(preset) = selected {
            app.apply_provider_choice(preset).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    let control = app.provider_control_rect;
    if footer_control_hit(app, &mouse, control) {
        open_footer_menu(app, FooterMenu::Provider).await?;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

/// Provider on the painted row under the cursor.
pub(super) fn provider_menu_selection(
    app: &App,
    picker: PickerGeometry,
    column: u16,
    row: u16,
) -> Option<ProviderPreset> {
    picker
        .item_at(column, row)
        .and_then(|index| provider_choices(app).get(index).copied())
}

pub(super) fn provider_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter
    )
}

pub(super) async fn handle_provider_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    if code == KeyCode::Esc {
        close_footer_menus(app);
        return Ok(());
    }
    let choices = provider_choices(app);
    move_menu_cursor(&mut app.provider_menu_selected, code, choices.len());
    if code == KeyCode::Enter {
        if let Some(preset) = choices.get(app.provider_menu_selected).copied() {
            close_footer_menus(app);
            app.apply_provider_choice(preset).await?;
        }
    }
    Ok(())
}

pub(super) async fn handle_model_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return Ok(app.model_menu_open.then(EventOutcome::default));
    }
    if app.model_menu_open {
        let selected = app
            .model_menu_geometry
            .filter(|picker| picker.contains(mouse.column, mouse.row))
            .and_then(|picker| model_menu_selection(app, picker, mouse.column, mouse.row));
        close_footer_menus(app);
        if let Some(model) = selected {
            app.apply_model_choice(model).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    let control = app.model_control_rect;
    if footer_control_hit(app, &mouse, control) {
        open_footer_menu(app, FooterMenu::Model).await?;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

/// Model on the painted row under the cursor: the picker scrolls once the list
/// is longer than the window, so the row has to be resolved through the same
/// window the painter used or a click would apply a different model.
pub(super) fn model_menu_selection(
    app: &App,
    picker: PickerGeometry,
    column: u16,
    row: u16,
) -> Option<String> {
    picker.item_at(column, row).and_then(|index| {
        model_choices(app)
            .get(index)
            .map(|choice| choice.id.clone())
    })
}

pub(super) fn model_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('r')
    )
}

pub(super) async fn handle_model_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    if code == KeyCode::Char('r') {
        // `r` refreshes the dynamic list in the background; the picker stays
        // open and repaints when the answer arrives.
        spawn_model_refresh(app);
        return Ok(());
    }
    if code == KeyCode::Esc {
        close_footer_menus(app);
        return Ok(());
    }
    let choices = model_choices(app);
    move_menu_cursor(&mut app.model_menu_selected, code, choices.len());
    if code == KeyCode::Enter {
        if let Some(choice) = choices.get(app.model_menu_selected) {
            let model = choice.id.clone();
            close_footer_menus(app);
            app.apply_model_choice(model).await?;
        }
    }
    Ok(())
}

pub(super) fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

/// Thinking cell on the painted row under the cursor. The level column owns the
/// left [`THINKING_LEVEL_COLUMN_WIDTH`] cells of a row and everything right of
/// it is the Qwen3.7 budget column, so both columns resolve through the same row
/// window the painter used.
pub(super) fn thinking_menu_selection(
    app: &App,
    picker: PickerGeometry,
    column: u16,
    row: u16,
) -> Option<ThinkingMenuCell> {
    let index = picker.item_at(column, row)?;
    let columns = thinking_menu_columns(app);
    let budget = usize::from(
        columns.len() > 1 && column >= picker.inner().x.saturating_add(THINKING_LEVEL_COLUMN_WIDTH),
    );
    thinking_menu_cell(&columns, budget, index).cloned()
}

/// Cell at (`column`, `row`) of the thinking table, or `None` where a shorter
/// column leaves a blank row that paints no cell.
fn thinking_menu_cell(
    columns: &[ThinkingMenuColumn],
    column: usize,
    row: usize,
) -> Option<&ThinkingMenuCell> {
    columns.get(column)?.cells.get(row)
}

pub(super) fn thinking_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Enter
    )
}

/// ↑/↓ walk the cells of the highlighted column, ←/→ switch between the thinking
/// level and the Qwen3.7 budget column, Enter applies the highlighted cell and
/// Esc dismisses the picker.
pub(super) async fn handle_thinking_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    if code == KeyCode::Esc {
        close_footer_menus(app);
        return Ok(());
    }
    let columns = thinking_menu_columns(app);
    if columns.is_empty() {
        close_footer_menus(app);
        return Ok(());
    }
    let mut cursor = app
        .thinking_menu_cursor
        .clamped(thinking_menu_rows(&columns), columns.len());
    match code {
        KeyCode::Up | KeyCode::Down => {
            let cells = columns[cursor.column].cells.len();
            if cells > 0 {
                let row = cursor.row.min(cells - 1);
                cursor.row = if code == KeyCode::Up {
                    (row + cells - 1) % cells
                } else {
                    (row + 1) % cells
                };
            }
        }
        KeyCode::Left if cursor.column > 0 => {
            let target = cursor.column - 1;
            move_thinking_column(&mut cursor, &columns, target);
        }
        KeyCode::Right => {
            let target = cursor.column + 1;
            if target < columns.len() {
                move_thinking_column(&mut cursor, &columns, target);
            }
        }
        KeyCode::Enter => {
            let Some(cell) = thinking_menu_cell(&columns, cursor.column, cursor.row).cloned()
            else {
                return Ok(());
            };
            close_footer_menus(app);
            return apply_thinking_choice(app, &cell).await;
        }
        _ => {}
    }
    app.thinking_menu_cursor = cursor;
    Ok(())
}

/// Switches the thinking cursor to another column, keeping the row inside it.
fn move_thinking_column(
    cursor: &mut ThinkingMenuCursor,
    columns: &[ThinkingMenuColumn],
    target: usize,
) {
    cursor.column = target.min(columns.len() - 1);
    cursor.row = cursor
        .row
        .min(columns[cursor.column].cells.len().saturating_sub(1));
}

pub(super) async fn handle_thinking_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return Ok(app.thinking_menu_open.then(EventOutcome::default));
    }
    if app.thinking_menu_open {
        let selected = app
            .thinking_menu_geometry
            .filter(|picker| picker.contains(mouse.column, mouse.row))
            .and_then(|picker| thinking_menu_selection(app, picker, mouse.column, mouse.row));
        close_footer_menus(app);
        if let Some(cell) = selected {
            apply_thinking_choice(app, &cell).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    let control = app.thinking_control_rect;
    if footer_control_hit(app, &mouse, control) {
        open_footer_menu(app, FooterMenu::Thinking).await?;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

/// Applies one thinking cell through the core. A rejected profile is reported on
/// the status line instead of bubbling out of the terminal event loop, which
/// would tear the whole session down over a single picker selection.
pub(super) async fn apply_thinking_choice(app: &mut App, cell: &ThinkingMenuCell) -> Result<()> {
    let mut provider = app.config.provider.clone();
    provider.thinking_level = cell.level;
    provider.thinking_budget_tokens = cell.budget;
    provider.normalize_thinking();
    app.cancel_model_refresh();
    if let Err(error) = app.handle.set_provider_config(provider.clone()).await {
        app.current.status = format!(
            "思考{}设置失败：{}",
            cell.kind.noun(),
            secrets::redact(&error.message)
        );
        return Ok(());
    }
    app.config.provider = provider;
    app.current.status = format!("思考{}已设为 {}", cell.kind.noun(), cell.label);
    let _ = refresh_provider_settings(app).await;
    app.sync_all().await?;
    Ok(())
}

/// True while any footer picker is painted over the transcript.
fn any_footer_menu_open(app: &App) -> bool {
    app.thinking_menu_open || app.provider_menu_open || app.model_menu_open
}

/// Routes one terminal event to the footer picker that owns the screen, then to
/// the footer controls that open a picker. Keeping the routing here means the
/// three pickers share one dismiss, one click and one key contract.
pub(super) async fn handle_footer_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if app.thinking_menu_open {
        return handle_thinking_mouse(app, mouse).await;
    }
    if app.provider_menu_open {
        return handle_provider_mouse(app, mouse).await;
    }
    if app.model_menu_open {
        return handle_model_mouse(app, mouse).await;
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) || any_footer_menu_open(app) {
        return Ok(None);
    }
    for (menu, control) in [
        (FooterMenu::Thinking, app.thinking_control_rect),
        (FooterMenu::Provider, app.provider_control_rect),
        (FooterMenu::Model, app.model_control_rect),
    ] {
        if footer_control_hit(app, &mouse, control) {
            open_footer_menu(app, menu).await?;
            return Ok(Some(EventOutcome::redraw()));
        }
    }
    Ok(None)
}

/// Routes one key to the open footer picker. A key the picker does not use
/// dismisses it the same way a click outside the frame does, so the next
/// keystroke reaches the composer instead of vanishing behind the popup.
pub(super) async fn handle_footer_menu_key(app: &mut App, code: KeyCode) -> Result<EventOutcome> {
    let handled = if app.provider_menu_open {
        provider_menu_key_handled(code)
    } else if app.model_menu_open {
        model_menu_key_handled(code)
    } else {
        thinking_menu_key_handled(code)
    };
    if !handled {
        close_footer_menus(app);
        return Ok(EventOutcome::redraw());
    }
    if app.provider_menu_open {
        handle_provider_menu_key(app, code).await?;
    } else if app.model_menu_open {
        handle_model_menu_key(app, code).await?;
    } else {
        handle_thinking_menu_key(app, code).await?;
    }
    Ok(EventOutcome::redraw())
}
