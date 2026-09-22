use super::*;

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

pub(super) async fn handle_provider_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return Ok(app.provider_menu_open.then(EventOutcome::default));
    }
    if app.provider_menu_open {
        let selected = app
            .provider_menu_rect
            .filter(|rect| point_in_rect(mouse.column, mouse.row, *rect))
            .and_then(|rect| provider_menu_selection(app, rect, mouse.column, mouse.row));
        app.provider_menu_open = false;
        app.force_full_redraw = true;
        if let Some(preset) = selected {
            app.apply_provider_choice(preset).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    if !app.current.busy
        && !app.has_pending_approval()
        && app
            .provider_control_rect
            .is_some_and(|rect| point_in_rect(mouse.column, mouse.row, rect))
    {
        app.model_menu_open = false;
        app.model_menu_rect = None;
        app.provider_menu_open = true;
        let _ = refresh_provider_settings(app).await;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

pub(super) fn provider_menu_selection(
    app: &App,
    rect: Rect,
    column: u16,
    row: u16,
) -> Option<ProviderPreset> {
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(column, row, inner) {
        return None;
    }
    let index = row.saturating_sub(inner.y) as usize;
    provider_choices(app).get(index).copied()
}

pub(super) fn provider_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter
    )
}

pub(super) async fn handle_provider_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    let choices = provider_choices(app);
    if choices.is_empty() {
        return Ok(());
    }
    match code {
        KeyCode::Esc => {
            app.provider_menu_open = false;
            app.provider_menu_rect = None;
        }
        KeyCode::Up => {
            app.provider_menu_selected =
                (app.provider_menu_selected + choices.len() - 1) % choices.len();
        }
        KeyCode::Down => {
            app.provider_menu_selected = (app.provider_menu_selected + 1) % choices.len();
        }
        KeyCode::Enter => {
            app.apply_provider_choice(choices[app.provider_menu_selected])
                .await?;
            app.provider_menu_open = false;
            app.provider_menu_rect = None;
        }
        _ => {}
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
            .model_menu_rect
            .filter(|rect| point_in_rect(mouse.column, mouse.row, *rect))
            .and_then(|rect| model_menu_selection(app, rect, mouse.column, mouse.row));
        app.model_menu_open = false;
        app.force_full_redraw = true;
        if let Some(model) = selected {
            app.apply_model_choice(model).await?;
        }
        return Ok(Some(EventOutcome::redraw()));
    }
    if !app.current.busy
        && !app.has_pending_approval()
        && app
            .model_control_rect
            .is_some_and(|rect| point_in_rect(mouse.column, mouse.row, rect))
    {
        app.provider_menu_open = false;
        app.provider_menu_rect = None;
        app.model_menu_open = true;
        let _ = load_provider_models(app, false).await;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

pub(super) fn model_menu_selection(app: &App, rect: Rect, column: u16, row: u16) -> Option<String> {
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(column, row, inner) {
        return None;
    }
    let index = row.saturating_sub(inner.y) as usize;
    model_choices(app)
        .get(index)
        .map(|choice| choice.id.clone())
}

pub(super) fn model_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('r')
    )
}

pub(super) async fn handle_model_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    if code == KeyCode::Char('r') {
        spawn_model_refresh(app);
        return Ok(());
    }
    let choices = model_choices(app);
    if choices.is_empty() {
        return Ok(());
    }
    match code {
        KeyCode::Esc => {
            app.model_menu_open = false;
            app.model_menu_rect = None;
        }
        KeyCode::Up => {
            app.model_menu_selected = (app.model_menu_selected + choices.len() - 1) % choices.len();
        }
        KeyCode::Down => {
            app.model_menu_selected = (app.model_menu_selected + 1) % choices.len();
        }
        KeyCode::Enter => {
            let model = choices[app.model_menu_selected].id.clone();
            app.apply_model_choice(model).await?;
            app.model_menu_open = false;
            app.model_menu_rect = None;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

pub(super) fn thinking_menu_selection(
    app: &App,
    rect: Rect,
    column: u16,
    row: u16,
) -> Option<(ThinkingLevel, Option<u32>)> {
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(column, row, inner) {
        return None;
    }
    let profile = app.thinking_profile();
    let index = row.saturating_sub(inner.y) as usize;
    if profile.kind == ThinkingProfileKind::Qwen37 && column >= inner.x.saturating_add(8) {
        const BUDGETS: [Option<u32>; 6] = [
            None,
            Some(1024),
            Some(4096),
            Some(8192),
            Some(16384),
            Some(32768),
        ];
        return BUDGETS
            .get(index)
            .copied()
            .map(|budget| (ThinkingLevel::Enabled, budget));
    }
    profile.options.get(index).copied().map(|level| {
        let budget = (level == ThinkingLevel::Enabled)
            .then_some(app.config.provider.thinking_budget_tokens)
            .flatten();
        (level, budget)
    })
}

pub(super) async fn apply_thinking_selection(
    app: &mut App,
    level: ThinkingLevel,
    budget: Option<u32>,
) -> Result<()> {
    let mut provider = app.config.provider.clone();
    provider.thinking_level = level;
    provider.thinking_budget_tokens = budget;
    provider.normalize_thinking();
    app.cancel_model_refresh();
    app.handle.set_provider_config(provider.clone()).await?;
    app.config.provider = provider;
    app.current.status = format!("思考强度已设为 {}", level.label());
    let _ = refresh_provider_settings(app).await;
    app.sync_all().await?;
    Ok(())
}
