use super::*;

pub(super) async fn open_settings(app: &mut App) {
    // Provider settings are core-authoritative; the local config is only a
    // fallback until the first read lands.
    let _ = refresh_provider_settings(app).await;
    app.settings = Some(provider_list_state(app));
    app.settings_field_index = 0;
    app.context_window_input.clear();
    let _ = load_provider_models(app, false).await;
    app.current.status = "已连接的供应商".into();
}

/// Converts the core settings DTO back into the display/edit shape the
/// existing settings UI consumes. Secrets never cross this boundary.
fn provider_configs_from_settings(app: &App) -> Vec<crate::config::ProviderConfig> {
    let Some(settings) = &app.provider_settings else {
        return app.config.providers.clone();
    };
    let mut providers = settings
        .saved
        .iter()
        .map(|profile| provider_config_from_profile(app, profile))
        .collect::<Vec<_>>();
    // The active profile is always editable even when it has never been
    // explicitly saved (for example the built-in default provider). Keyed by
    // id so several custom providers never collapse into one row.
    if !providers
        .iter()
        .any(|provider| provider.id() == settings.active.id)
        && let Some(active) = provider_config_from_active(app, settings)
    {
        providers.insert(0, active);
    }
    providers
}

fn provider_config_from_active(
    app: &App,
    settings: &ProviderSettingsDto,
) -> Option<crate::config::ProviderConfig> {
    let preset = ProviderPreset::parse(&settings.active.preset)?;
    let mut config = if settings.active.id == app.config.provider.id() {
        app.config.provider.clone()
    } else {
        app.config
            .provider_for_id(&settings.active.id)
            .unwrap_or_else(|| preset.defaults())
    };
    config.id = settings.active.id.clone();
    config.name = settings.active.name.clone();
    config.enabled_models = settings.active.enabled_models.clone();
    config.preset = preset;
    config.model = settings.active.model.clone();
    if !settings.active.base_url.trim().is_empty() {
        config.base_url = settings.active.base_url.clone();
    }
    if let Some(kind) = ProviderKind::parse_wire_tag(&settings.active.kind) {
        config.kind = kind;
    }
    Some(config)
}

fn provider_config_from_profile(
    app: &App,
    profile: &ProviderProfileDto,
) -> crate::config::ProviderConfig {
    let preset = ProviderPreset::parse(&profile.preset).unwrap_or_default();
    // The DTO intentionally omits thinking/retry fields. For the profile the
    // local config still tracks, start from that richer copy so editing the
    // active provider does not silently reset those customizations; the core
    // merge keeps them on apply anyway.
    let mut config = if profile.id == app.config.provider.id() {
        app.config.provider.clone()
    } else {
        app.config
            .provider_for_id(&profile.id)
            .unwrap_or_else(|| preset.defaults())
    };
    config.id = profile.id.clone();
    config.name = profile.name.clone();
    config.enabled_models = profile.enabled_models.clone();
    config.preset = preset;
    config.model = profile.model.clone();
    if !profile.base_url.trim().is_empty() {
        config.base_url = profile.base_url.clone();
    }
    if let Some(kind) = ProviderKind::parse_wire_tag(&profile.kind) {
        config.kind = kind;
    }
    config
}

/// Connected provider ids from the core settings view. Falls back to the local
/// secret cache only before the first core read (e.g. during tests).
fn available_key_ids(app: &App) -> HashSet<String> {
    if let Some(settings) = &app.provider_settings {
        return settings.connected.iter().cloned().collect();
    }
    ProviderPreset::ALL
        .iter()
        .filter(|preset| secrets::api_key_cached_only(**preset, preset.key_id()).is_ok())
        .map(|preset| preset.key_id().to_owned())
        .collect()
}

fn provider_form(app: &App, provider: crate::config::ProviderConfig) -> SettingsForm {
    let available = available_key_ids(app);
    let existing_key_id = available
        .contains(provider.id())
        .then(|| provider.id().to_owned());
    let mut form = SettingsForm::new(provider, existing_key_id);
    form.set_available_key_ids(available);
    form
}

/// Builds the settings list with `connected` sourced from the core DTO (the
/// local `SettingsState::list` constructor would re-derive it from the cache).
fn provider_list_state(app: &App) -> SettingsState {
    let providers = provider_configs_from_settings(app);
    let mut state = SettingsState::list(providers, app.active_provider_id());
    if let SettingsState::List(list) = &mut state
        && let Some(settings) = &app.provider_settings
    {
        list.connected = settings.connected.iter().cloned().collect();
    }
    state
}

fn reopen_provider_list(app: &mut App) {
    app.settings = Some(provider_list_state(app));
    app.settings_field_index = 0;
    app.context_window_input.clear();
}

fn open_provider_form(app: &mut App, provider: crate::config::ProviderConfig) {
    // The core profile DTO intentionally omits the explicit context window, so
    // the safe default is "inherit the merged profile value". Typing a number
    // sends an override the core clamps; leaving it empty keeps whatever the
    // core already has (including edits made by WebUI).
    app.context_window_input.clear();
    app.settings_field_index = 0;
    app.settings = Some(SettingsState::Form(provider_form(app, provider)));
}

pub(super) fn open_template_picker(app: &mut App) {
    if let Some(settings) = &mut app.settings {
        settings.open_templates();
        app.current.status = "选择供应商模板".into();
    }
}

pub(super) fn open_selected_profile(app: &mut App) {
    if let Some(provider) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_profile)
    {
        open_provider_form(app, provider);
        app.current.status = "编辑供应商".into();
    }
}

pub(super) fn open_selected_template(app: &mut App) {
    if let Some(preset) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_template)
    {
        let mut provider = preset.defaults();
        if preset == ProviderPreset::Custom {
            // A new custom provider is addressed by an empty id: the core mints
            // a fresh `custom-<uuid>` on apply, so every create is a new row.
            provider.id = String::new();
            provider.name = String::new();
        }
        open_provider_form(app, provider);
        app.current.status = format!("添加 {}", preset.label());
    }
}

/// Total selectable rows in the settings form: the core `FIELDS` registry plus
/// two TUI-only synthetic rows (the provider display name and the
/// context-window override).
pub(super) fn settings_row_count() -> usize {
    FIELDS.len() + 2
}

/// TUI row index of the synthetic provider-name input, rendered immediately
/// after the (read-only) preset/template row.
pub(super) fn name_row() -> usize {
    1
}

/// TUI row index of the synthetic context-window override, rendered immediately
/// after Thinking and before the write-only API key.
pub(super) fn context_window_row() -> usize {
    FIELDS.len()
}

/// TUI row index of a core field. The synthetic name row shifts every field
/// after Preset by one; the context-window row shifts ApiKey by one more.
pub(super) fn tui_field_index(field: SettingsField) -> usize {
    let index = FIELDS
        .iter()
        .position(|spec| spec.field == field)
        .unwrap_or(0);
    let name_shift = usize::from(index >= name_row());
    let window_shift = usize::from(index >= FIELDS.len().saturating_sub(1));
    index + name_shift + window_shift
}

pub(super) fn settings_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    let paste_shortcut = code == KeyCode::Char('v')
        && modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::META);
    paste_shortcut
        || matches!(
            code,
            KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Enter
        )
        || (code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL))
        || matches!(code, KeyCode::Char(_) if !modifiers.contains(KeyModifiers::CONTROL))
}

pub(super) fn palette_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Up | KeyCode::Down | KeyCode::Backspace
    ) || matches!(code, KeyCode::Char(_) if !modifiers.contains(KeyModifiers::CONTROL))
}

pub(super) fn paste_text_into_settings(app: &mut App, text: &str) -> bool {
    if app.settings_field_index == name_row() {
        let sanitized = text.replace(['\r', '\n'], "");
        if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
            form.provider.name = sanitized;
            return true;
        }
        return false;
    }
    if app.settings_field_index == context_window_row() {
        let sanitized = text.replace(['\r', '\n'], "");
        if sanitized
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            app.context_window_input = sanitized;
            return true;
        }
        return false;
    }
    let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) else {
        return false;
    };
    let field = form.field();
    if !matches!(
        field,
        SettingsField::Model | SettingsField::BaseUrl | SettingsField::ApiKey
    ) {
        return false;
    }
    let sanitized = text.replace(['\r', '\n'], "");
    let mut sanitized = sanitized.as_str();
    if sanitized.len() > crate::clipboard::MAX_CLIPBOARD_BYTES {
        let mut end = crate::clipboard::MAX_CLIPBOARD_BYTES;
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized = &sanitized[..end];
    }
    form.paste(field, sanitized);
    true
}

pub(super) async fn handle_settings_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            if matches!(app.settings, Some(SettingsState::List(_))) {
                app.settings = None;
                app.current.status = "设置已取消".into();
            } else {
                reopen_provider_list(app);
                app.current.status = "已返回供应商列表".into();
            }
        }
        KeyCode::Tab | KeyCode::Down => {
            if app
                .settings
                .as_ref()
                .is_some_and(|settings| settings.form().is_some())
            {
                app.settings_field_index = (app.settings_field_index + 1) % settings_row_count();
                sync_form_selection(app);
            } else if let Some(settings) = &mut app.settings {
                settings.move_selection(1);
            }
        }
        KeyCode::BackTab | KeyCode::Up => {
            if app
                .settings
                .as_ref()
                .is_some_and(|settings| settings.form().is_some())
            {
                app.settings_field_index =
                    (app.settings_field_index + settings_row_count() - 1) % settings_row_count();
                sync_form_selection(app);
            } else if let Some(settings) = &mut app.settings {
                settings.move_selection(-1);
            }
        }
        KeyCode::Left | KeyCode::Right => {
            let direction = if code == KeyCode::Right { 1 } else { -1 };
            if app.settings_field_index == context_window_row() {
                return;
            }
            if app.settings_field_index == name_row() {
                // The name is free text; there is nothing to cycle.
                return;
            }
            if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.cycle(field, direction);
            }
        }
        KeyCode::Backspace => {
            if app.settings_field_index == name_row() {
                if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                    form.provider.name.pop();
                }
            } else if app.settings_field_index == context_window_row() {
                app.context_window_input.pop();
            } else if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.edit(field, None);
            }
        }
        KeyCode::Char('v')
            if modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::META) =>
        {
            match crate::clipboard::read_text() {
                Ok(text) => {
                    if paste_text_into_settings(app, &text) {
                        app.current.status = "已粘贴剪贴板内容".into();
                    } else {
                        app.current.status = "当前字段不支持粘贴".into();
                    }
                }
                Err(error) => {
                    app.current.status = format!("无法读取系统剪贴板：{}", secrets::redact(&error));
                }
            }
        }
        KeyCode::Delete | KeyCode::Char('d')
            if matches!(app.settings, Some(SettingsState::Form(_)))
                && (code == KeyCode::Delete || modifiers.contains(KeyModifiers::CONTROL)) =>
        {
            if app.settings_field_index == name_row() {
                if code == KeyCode::Delete
                    && let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut)
                {
                    form.provider.name.clear();
                }
            } else if app.settings_field_index == context_window_row() {
                if code == KeyCode::Delete {
                    app.context_window_input.clear();
                }
            } else if let Err(error) = remove_settings_provider(app).await {
                app.current.status = format!("移除失败：{}", secrets::redact(&error.to_string()));
            }
        }
        KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if app.settings_field_index == name_row() {
                if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                    form.provider.name.push(character);
                }
            } else if app.settings_field_index == context_window_row() {
                if character.is_ascii_digit() {
                    app.context_window_input.push(character);
                }
            } else if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.edit(field, Some(character));
            }
        }
        KeyCode::Enter => match app.settings.as_ref() {
            Some(settings) if settings.on_add_row() => open_template_picker(app),
            Some(SettingsState::List(_)) => open_selected_profile(app),
            Some(SettingsState::Templates(_)) => open_selected_template(app),
            Some(SettingsState::Form(_)) => {
                if let Err(error) = apply_settings(app).await {
                    app.current.status =
                        format!("设置错误：{}", secrets::redact(&error.to_string()));
                }
            }
            None => {}
        },
        _ => {}
    }
}

/// Mirrors the TUI row index onto the core form's `FIELDS` selection. The two
/// synthetic rows (name, context window) have no core `SettingsField`, so the
/// core selection clamps to the last real field while either is highlighted;
/// their edits are intercepted before this mapping is consulted.
fn sync_form_selection(app: &mut App) {
    // TUI rows: [Preset, Name, Protocol, Model, BaseUrl, Thinking,
    // ContextWindow, ApiKey]. Every core field after the name row is shifted by
    // one; ApiKey is shifted by the context-window row as well.
    if app.settings_field_index == name_row() {
        return;
    }
    let core_field = if app.settings_field_index == context_window_row() {
        SettingsField::ApiKey
    } else {
        FIELDS
            .iter()
            .map(|spec| spec.field)
            .find(|field| tui_field_index(*field) == app.settings_field_index)
            .unwrap_or(SettingsField::Preset)
    };
    let index = FIELDS
        .iter()
        .position(|spec| spec.field == core_field)
        .unwrap_or(0);
    if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
        form.selected = index;
    }
}

async fn apply_settings(app: &mut App) -> Result<()> {
    let (
        provider_id,
        preset,
        name,
        model,
        base_url,
        kind,
        entered_key,
        context_window,
        form_provider,
    ) = {
        let form = app
            .settings
            .as_ref()
            .and_then(SettingsState::form)
            .context("provider editor is not open")?;
        // A new custom provider carries an empty *raw* id (the core mints
        // one); every other row keeps its stable id so edits never fork.
        // `id()` must not be used here: it falls back to `custom` for a blank
        // id, which would silently address (or delete) the legacy profile.
        let provider_id = form.provider.id.trim().to_owned();
        let creating = provider_id.is_empty();
        let name = form.provider.name.trim().to_owned();
        if creating && name.is_empty() {
            anyhow::bail!("自定义供应商名称不能为空");
        }
        let provider = form.prepare()?;
        let context_window = parse_context_window_override(&app.context_window_input)?;
        (
            provider_id,
            provider.preset,
            name,
            provider.model.clone(),
            provider.base_url.clone(),
            provider.kind,
            form.api_key.trim().to_owned(),
            context_window,
            provider,
        )
    };

    // Ordering matters: `store_api_key_cached` seeds the in-process cache even
    // when the OS keyring write fails, so the core's rebuilt runner can pick
    // the new key up immediately. The warning below marks the degraded case.
    // For a brand-new custom provider the id does not exist yet, so the key is
    // staged under the family id and the core promotes it while creating.
    let key_stage_id = if provider_id.is_empty() {
        preset.key_id().to_owned()
    } else {
        provider_id.clone()
    };
    let key_warning = if entered_key.is_empty() {
        None
    } else {
        secrets::store_api_key_cached(preset, &key_stage_id, &entered_key)
            .err()
            .map(|error| {
                format!(
                    "API Key 仅本次运行有效：{}",
                    secrets::redact(&error.to_string())
                )
            })
    };

    // Thinking is an advanced full-profile field the profile endpoint does not
    // carry. When the edited provider is active and the user changed it in the
    // form, commit the merged full profile once instead of saving twice; for a
    // non-active provider we keep the merge endpoint and warn that thinking is
    // edited via the top-level menu once the provider is active.
    // Only an *existing* profile can be the active one: a new custom provider
    // always goes through the create path so the core mints its id (the full
    // config path would instead stamp the family key and collide with a legacy
    // profile).
    let active = !provider_id.is_empty() && app.config.provider.id() == provider_id;
    let thinking_changed = active
        && (form_provider.thinking != app.config.provider.thinking
            || form_provider.thinking_level != app.config.provider.thinking_level
            || form_provider.thinking_budget_tokens != app.config.provider.thinking_budget_tokens);
    let thinking_warning = (!active
        && (form_provider.thinking != preset.defaults().thinking
            || form_provider.thinking_level != preset.defaults().thinking_level
            || form_provider.thinking_budget_tokens != preset.defaults().thinking_budget_tokens))
        .then(|| "思考设置请在切换为当前供应商后通过顶部菜单修改".to_owned());

    if active {
        app.cancel_model_refresh();
    }
    if thinking_changed {
        let mut provider = app.config.provider.clone();
        provider.id = provider_id.clone();
        provider.name = name.clone();
        provider.preset = preset;
        provider.model = model.clone();
        if !base_url.trim().is_empty() {
            provider.base_url = base_url.clone();
        }
        provider.kind = kind;
        if let Some(window) = context_window {
            provider.context_window_tokens = Some(window);
        }
        provider.thinking = form_provider.thinking;
        provider.thinking_level = form_provider.thinking_level;
        provider.thinking_budget_tokens = form_provider.thinking_budget_tokens;
        provider.normalize_thinking();
        if let Err(error) = app.handle.set_provider_config(provider).await {
            return Err(anyhow::anyhow!(secrets::redact(&error.message)));
        }
    } else if let Err(error) = app
        .handle
        .set_provider_profile(
            &provider_id,
            preset,
            Some(name.as_str()),
            &model,
            (!base_url.trim().is_empty()).then_some(base_url.as_str()),
            Some(kind),
            context_window,
            None,
        )
        .await
    {
        return Err(anyhow::anyhow!(secrets::redact(&error.message)));
    }

    // The core persisted the profile once; refresh every derived surface from
    // the core instead of writing the local config a second time.
    let _ = refresh_provider_settings(app).await;
    let _ = load_provider_models(app, false).await;
    app.current.context_limit_tokens = None;
    app.sync_all().await?;
    reopen_provider_list(app);
    let warnings = [key_warning, thinking_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    let label = if name.is_empty() {
        preset.label().to_owned()
    } else {
        name.clone()
    };
    app.current.status = format!(
        "就绪 | {label} | {model}{}",
        if warnings.is_empty() {
            String::new()
        } else {
            format!(" | {warnings}")
        },
    );
    Ok(())
}

/// Parses the optional context-window override. Empty inherits the merged
/// profile value; a number is passed to the core, which clamps it.
fn parse_context_window_override(input: &str) -> Result<Option<u64>> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let value = input
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("上下文窗口必须是正整数"))?;
    if value == 0 {
        return Err(anyhow::anyhow!("上下文窗口必须是正整数"));
    }
    Ok(Some(value))
}

async fn remove_settings_provider(app: &mut App) -> Result<()> {
    let provider_id = app
        .settings
        .as_ref()
        .and_then(SettingsState::form)
        .map(|form| form.provider.id.trim().to_owned())
        .context("provider editor is not open")?;
    if provider_id.is_empty() {
        anyhow::bail!("新建的供应商尚未保存，无需移除");
    }
    app.cancel_model_refresh();
    app.handle.remove_provider(&provider_id).await?;
    // Converge from the core view; never reload config as the authority.
    let _ = refresh_provider_settings(app).await;
    app.provider_models = ProviderModelsState::default();
    let _ = load_provider_models(app, false).await;
    app.sync_all().await?;
    reopen_provider_list(app);
    app.current.status = "供应商已移除；API Key 已保留在系统钥匙串".into();
    Ok(())
}
