use super::provider::{compact_window, spawn_model_refresh};
use super::*;
use crate::config::ProviderConfig;

/// Mirrors the core's `MAX_PROVIDER_NAME_CHARS` (config/provider.rs). The core
/// stays the authority and re-validates on apply; the panel only needs the same
/// bound so an over-long name is refused inline instead of after a round trip.
const PROVIDER_NAME_MAX_CHARS: usize = 64;

/// Which pane owns the keyboard. Only one of them answers to `↑`/`↓` at a time:
/// the left pane selects *what* is edited, the right pane edits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorPane {
    Providers,
    Fields,
}

/// Right-pane rows in painting order.
///
/// The core's `FIELDS` registry owns the editable values (cycling, masking,
/// paste, `prepare`); this list only fixes the reading order and adds the three
/// rows the registry has no slot for: the display name, the read-only template
/// the profile was created from, and the explicit context-window override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorField {
    Name,
    Template,
    Protocol,
    Model,
    BaseUrl,
    ContextWindow,
    Thinking,
    ApiKey,
}

pub(crate) const EDITOR_FIELDS: [EditorField; 8] = [
    EditorField::Name,
    EditorField::Template,
    EditorField::Protocol,
    EditorField::Model,
    EditorField::BaseUrl,
    EditorField::ContextWindow,
    EditorField::Thinking,
    EditorField::ApiKey,
];

impl EditorField {
    /// The core field this row edits, when the core owns the value. `Template`
    /// is read-only by design (a profile's preset is its identity) and the
    /// other two are TUI-only, so `None` means "handled before the core form".
    pub(crate) fn core(self) -> Option<SettingsField> {
        match self {
            Self::Protocol => Some(SettingsField::Protocol),
            Self::Model => Some(SettingsField::Model),
            Self::BaseUrl => Some(SettingsField::BaseUrl),
            Self::Thinking => Some(SettingsField::Thinking),
            Self::ApiKey => Some(SettingsField::ApiKey),
            Self::Name | Self::Template | Self::ContextWindow => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "名称",
            Self::Template => "模板",
            Self::Protocol => "协议",
            Self::Model => "模型",
            Self::BaseUrl => "接口地址",
            Self::ContextWindow => "上下文窗口",
            Self::Thinking => "思考能力",
            Self::ApiKey => "API Key",
        }
    }

    /// Whether typing rewrites this row in place.
    pub(crate) fn is_text(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Model | Self::BaseUrl | Self::ContextWindow | Self::ApiKey
        )
    }
}

/// One row of the left pane.
#[derive(Clone, Debug)]
pub(crate) struct ProviderRow {
    /// Raw profile id. An empty id is the *create* address: the core mints a
    /// `custom-<uuid>` on apply, so a blank id never addresses the legacy
    /// `custom` profile.
    pub(crate) id: String,
    pub(crate) preset: ProviderPreset,
    pub(crate) label: String,
    pub(crate) saved: bool,
    pub(crate) active: bool,
    pub(crate) connected: bool,
}

impl ProviderRow {
    /// What the row *is*, independent of key state.
    pub(crate) fn badge(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.active {
            parts.push("当前");
        }
        if self.saved {
            parts.push("已配置");
        }
        if self.connected {
            parts.push("●");
        }
        parts.join(" ")
    }

    /// The key state, read straight off the core's `connected` set so the left
    /// pane and the footer pill can never disagree about it.
    pub(crate) fn key_state(&self) -> &'static str {
        if self.connected {
            "已连接"
        } else {
            "需要 API Key"
        }
    }
}

/// The editor's pinned action row. Each entry carries its own rectangle, so a
/// click resolves exactly the word the frame painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Apply,
    Delete,
    Cancel,
}

impl EditorAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Apply => "应用",
            Self::Delete => "删除",
            Self::Cancel => "取消",
        }
    }
}

/// A provider profile as the left pane needs it, from whichever authority is
/// already available: the core settings view, or the local config before the
/// first read.
struct RowSource {
    id: String,
    preset: ProviderPreset,
    name: String,
    saved: bool,
}

fn row_sources(app: &App) -> Vec<RowSource> {
    if let Some(settings) = &app.provider_settings {
        return settings
            .saved
            .iter()
            .map(|profile| RowSource {
                id: profile.id.clone(),
                preset: ProviderPreset::parse(&profile.preset).unwrap_or_default(),
                name: profile.name.clone(),
                saved: true,
            })
            .collect();
    }
    app.config
        .providers
        .iter()
        .map(|provider| RowSource {
            id: provider.id().to_owned(),
            preset: provider.preset,
            name: provider.name.clone(),
            saved: true,
        })
        .collect()
}

/// Left-pane rows: every built-in family (always listed, so no template picker
/// stands between the user and a provider), then each saved custom profile in
/// saved order. The trailing "add" command is not a row: it is address
/// `rows.len()`, which keeps a profile row and the create command from ever
/// being confused for each other.
pub(crate) fn provider_rows(app: &App) -> Vec<ProviderRow> {
    let sources = row_sources(app);
    let active = app.active_provider_id();
    let connected = |id: &str| {
        app.provider_settings
            .as_ref()
            .is_some_and(|settings| settings.connected.iter().any(|entry| entry == id))
    };
    let mut rows = Vec::with_capacity(sources.len() + ProviderPreset::ALL.len());
    for preset in ProviderPreset::ALL {
        if preset == ProviderPreset::Custom {
            continue;
        }
        let id = preset.key_id().to_owned();
        let source = sources.iter().find(|source| source.preset == preset);
        rows.push(ProviderRow {
            saved: source.is_some_and(|source| source.saved),
            label: source
                .map(|source| source.name.trim())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| preset.label().to_owned()),
            active: active == id,
            connected: connected(&id),
            preset,
            id,
        });
    }
    for source in sources
        .iter()
        .filter(|source| source.preset == ProviderPreset::Custom)
    {
        rows.push(ProviderRow {
            id: source.id.clone(),
            preset: ProviderPreset::Custom,
            label: if source.name.trim().is_empty() {
                ProviderPreset::Custom.label().to_owned()
            } else {
                source.name.trim().to_owned()
            },
            saved: source.saved,
            active: active == source.id,
            connected: connected(&source.id),
        });
    }
    // The active provider must always be reachable, even when it is a profile
    // neither the core view nor the local config lists.
    if !rows.iter().any(|row| row.id == active) {
        rows.push(ProviderRow {
            id: active.clone(),
            preset: app.active_preset(),
            label: app.provider_label().to_owned(),
            saved: false,
            active: true,
            connected: connected(&active),
        });
    }
    rows
}

/// The in-panel model picker. The footer's picker applies a model to the
/// *active* provider; this one only fills the draft, so it keeps its own cursor
/// and geometry.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EditorModelPicker {
    pub(crate) open: bool,
    pub(crate) selected: usize,
    pub(crate) geometry: Option<PickerGeometry>,
}

/// One model the draft can adopt, with the metadata its row renders.
#[derive(Clone, Debug)]
pub(crate) struct EditorModelChoice {
    pub(crate) id: String,
    /// `id · 128k` when the provider reported a window.
    pub(crate) label: String,
    pub(crate) window: Option<u64>,
    pub(crate) max_output: Option<u64>,
}

/// TUI-only state of the provider panel: which profile is being edited, the
/// draft, and every rectangle the frame painted for it.
///
/// The draft is the *core's* [`SettingsForm`]: validation, enum cycling, the
/// write-only key mask and paste all come from there, so the panel cannot drift
/// from the core's own field semantics. Everything this struct adds is
/// presentation and navigation — and it is dropped together with its rectangles
/// when the panel closes, so a rectangle from before a resize can never be
/// hit-tested again.
pub struct ProviderEditor {
    pub(crate) rows: Vec<ProviderRow>,
    /// `0..rows.len()` selects a profile; `rows.len()` is the create command.
    pub(crate) selected_row: usize,
    pub(crate) pane: EditorPane,
    pub(crate) form: SettingsForm,
    pub(crate) field_index: usize,
    /// Write-only explicit-window buffer. Empty means "inherit the merged
    /// profile value"; digits are passed to the core, which clamps them.
    pub(crate) window_input: String,
    pub(crate) window_note: Option<String>,
    pub(crate) window_fetch_pending: bool,
    pub(crate) delete_confirm: bool,
    /// Inline failure from the last apply or delete; cleared on the next edit.
    pub(crate) error: Option<String>,
    pub(crate) model_picker: EditorModelPicker,
    pub(crate) rect: Rect,
    pub(crate) row_rects: Vec<Rect>,
    pub(crate) field_rects: Vec<Rect>,
    pub(crate) action_rects: Vec<(EditorAction, Rect)>,
    pub(crate) model_picker_rect: Option<Rect>,
}

impl ProviderEditor {
    /// Builds the editor with its draft already loaded, so no frame and no key
    /// ever observes "a row is focused but nothing is being edited".
    fn new(app: &App, rows: Vec<ProviderRow>, selected_row: usize) -> Self {
        let selected_row = selected_row.min(rows.len().saturating_sub(1));
        let config = rows
            .get(selected_row)
            .map(|row| draft_config_for_row(app, row))
            .unwrap_or_else(|| app.config.provider.clone());
        Self {
            form: draft_form(app, config),
            rows,
            selected_row,
            pane: EditorPane::Providers,
            field_index: 0,
            window_input: String::new(),
            window_note: None,
            window_fetch_pending: false,
            delete_confirm: false,
            error: None,
            model_picker: EditorModelPicker::default(),
            rect: Rect::default(),
            row_rects: Vec::new(),
            field_rects: Vec::new(),
            action_rects: Vec::new(),
            model_picker_rect: None,
        }
    }

    pub(crate) fn field(&self) -> EditorField {
        EDITOR_FIELDS[self.field_index.min(EDITOR_FIELDS.len() - 1)]
    }

    pub(crate) fn selected_provider(&self) -> Option<&ProviderRow> {
        self.rows.get(self.selected_row)
    }

    /// Whether the highlighted left-pane address is the create command.
    pub(crate) fn on_add_row(&self) -> bool {
        self.selected_row >= self.rows.len()
    }

    /// The draft is a create when its raw id is empty. `id()` must not be used
    /// here: it falls back to `custom` for a blank id, which would silently
    /// address (or delete) the legacy profile.
    pub(crate) fn creating(&self) -> bool {
        self.form.provider.id.trim().is_empty()
    }

    /// Why "应用" is refused, if it is. Both reasons are ones the core would
    /// reject anyway; catching them here keeps the message inside the panel
    /// instead of in the status line the panel covers.
    pub(crate) fn blocked_reason(&self) -> Option<String> {
        let name = self.form.provider.name.trim();
        if self.creating() && name.is_empty() {
            return Some("自定义供应商名称不能为空".to_owned());
        }
        if name.chars().count() > PROVIDER_NAME_MAX_CHARS {
            return Some(format!("供应商名称最多 {PROVIDER_NAME_MAX_CHARS} 个字符"));
        }
        if self.form.provider.model.trim().is_empty() {
            return Some("模型不能为空".to_owned());
        }
        None
    }

    /// The window the provider endpoint reported for the draft's model, when
    /// the draft is the profile that list belongs to.
    fn fetched_window(&self, app: &App) -> Option<u64> {
        if !fetched_applies(app, &self.form) {
            return None;
        }
        self.model_choices(app)
            .into_iter()
            .find(|choice| choice.id == self.form.provider.model.trim())
            .and_then(|choice| choice.window)
    }

    /// Whether anything already resolves the draft model's window. Matches the
    /// WebUI's rule: a model in the built-in registry counts even without a
    /// fetch, because the core resolves it from its own tables.
    pub(crate) fn window_known(&self, app: &App) -> bool {
        let model = self.form.provider.model.trim();
        if model.is_empty() {
            return false;
        }
        self.fetched_window(app).is_some()
            || self
                .form
                .provider
                .preset
                .selectable_models()
                .contains(&model)
    }

    /// Whether `g` can do anything: only the *active* profile's base URL can be
    /// queried, because that is the URL the core fetches `/models` from.
    pub(crate) fn window_fetch_available(&self, app: &App) -> bool {
        fetched_applies(app, &self.form)
    }

    /// The value the context-window row shows: the override while it is being
    /// typed, else what the profile inherits.
    pub(crate) fn window_value(&self, app: &App) -> String {
        if !self.window_input.is_empty() {
            return self.window_input.clone();
        }
        match self.fetched_window(app) {
            Some(window) => format!("（继承 {}）", compact_window(window)),
            None => "（继承）".to_owned(),
        }
    }

    /// The model row's value: the draft's model plus the window the provider
    /// endpoint reported for it, when the dynamic list describes this profile.
    pub(crate) fn model_value(&self, app: &App) -> String {
        model_label(self.form.provider.model.trim(), self.fetched_window(app))
    }

    /// The line under the context-window row: the explicit hint while nothing
    /// resolves the window, and the outcome of `g` once it has run.
    pub(crate) fn window_hint(&self, app: &App) -> Option<String> {
        if let Some(note) = &self.window_note {
            return Some(note.clone());
        }
        if self.window_fetch_pending {
            return Some("正在获取该模型的窗口……".to_owned());
        }
        (!self.window_known(app)).then(|| {
            "接口与内置注册表均未报告该模型的窗口；显式值优先生效（4096–10000000）。".to_owned()
        })
    }

    /// Model picker rows for the *draft*: the dynamic list when it belongs to
    /// this profile, then the preset's static list, then the current value and
    /// the profile's own saved model, deduped by id.
    pub(crate) fn model_choices(&self, app: &App) -> Vec<EditorModelChoice> {
        let mut choices: Vec<EditorModelChoice> = Vec::new();
        let mut push = |id: &str, window: Option<u64>, max_output: Option<u64>| {
            let id = id.trim();
            if id.is_empty() || choices.iter().any(|choice| choice.id == id) {
                return;
            }
            choices.push(EditorModelChoice {
                label: model_label(id, window),
                id: id.to_owned(),
                window,
                max_output,
            });
        };
        if fetched_applies(app, &self.form) {
            for model in &app.provider_models.models {
                push(
                    &model.id,
                    model.context_window_tokens,
                    model.max_output_tokens.map(u64::from),
                );
            }
        }
        for model in self.form.provider.preset.selectable_models() {
            push(model, None, None);
        }
        push(&self.form.provider.model, None, None);
        let id = self.form.provider.id.trim();
        if !id.is_empty()
            && let Some(profile) = app
                .provider_settings
                .as_ref()
                .and_then(|settings| settings.saved.iter().find(|profile| profile.id == id))
        {
            push(&profile.model, None, None);
        }
        choices
    }

    /// Mirrors the highlighted TUI row onto the core form's own `FIELDS`
    /// cursor, which is what `cycle`/`edit`/`value` read.
    fn sync_field_selection(&mut self) {
        if let Some(field) = self.field().core() {
            let index = FIELDS
                .iter()
                .position(|spec| spec.field == field)
                .unwrap_or(0);
            self.form.selected = index;
        }
    }
}

/// `id · 128k` when a window is known, otherwise the bare id.
fn model_label(id: &str, window: Option<u64>) -> String {
    match window.filter(|window| *window > 0) {
        Some(window) => format!("{id} · {}", compact_window(window)),
        None => id.to_owned(),
    }
}

/// Whether the dynamic model list describes the *draft*. The core fetches
/// `GET {base_url}/models` for the active provider, so the answer only applies
/// while the draft is that same profile and still points at that same URL.
fn fetched_applies(app: &App, form: &SettingsForm) -> bool {
    let Some(settings) = &app.provider_settings else {
        return false;
    };
    let id = form.provider.id.trim();
    !id.is_empty()
        && settings.active.id == id
        && settings.active.base_url == form.provider.base_url.trim()
}

/// Rebuilds a draft from a settings profile. The DTO carries only the
/// connection fields, so a profile the local config still tracks starts from
/// that richer copy and keeps its thinking/retry customizations.
fn provider_config_from_profile(app: &App, profile: &ProviderProfileDto) -> ProviderConfig {
    let preset = ProviderPreset::parse(&profile.preset).unwrap_or_default();
    let mut config = local_seed(app, &profile.id, preset);
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

/// The same conversion for the core's *active* profile, which stays editable
/// even when it has never been saved explicitly (a built-in default is the
/// common case).
fn provider_config_from_active(
    app: &App,
    settings: &ProviderSettingsDto,
) -> Option<ProviderConfig> {
    let preset = ProviderPreset::parse(&settings.active.preset)?;
    let mut config = local_seed(app, &settings.active.id, preset);
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

/// The richest local copy of `id`: the live active config, else whatever the
/// config file holds for it, else the template defaults.
fn local_seed(app: &App, id: &str, preset: ProviderPreset) -> ProviderConfig {
    if id == app.config.provider.id() {
        app.config.provider.clone()
    } else {
        app.config
            .provider_for_id(id)
            .unwrap_or_else(|| preset.defaults())
    }
}

/// The draft for one left-pane row: the saved profile, else the active view,
/// else the local config, else the template defaults.
fn draft_config_for_row(app: &App, row: &ProviderRow) -> ProviderConfig {
    if let Some(settings) = &app.provider_settings {
        if !row.id.is_empty()
            && let Some(profile) = settings.saved.iter().find(|profile| profile.id == row.id)
        {
            return provider_config_from_profile(app, profile);
        }
        if settings.active.id == row.id
            && let Some(active) = provider_config_from_active(app, settings)
        {
            return active;
        }
    }
    if let Some(provider) = app.config.provider_for_id(&row.id) {
        return provider;
    }
    let mut provider = row.preset.defaults();
    provider.id = row.id.clone();
    provider
}

/// A draft plus the key state the core reports for it. Nothing here reads a
/// key: the form only learns *which* ids already have one, which is what drives
/// the `********` placeholder.
fn draft_form(app: &App, config: ProviderConfig) -> SettingsForm {
    let connected = connected_ids(app);
    let existing = connected
        .contains(config.id())
        .then(|| config.id().to_owned());
    let mut form = SettingsForm::new(config, existing);
    form.set_available_key_ids(connected);
    form
}

fn connected_ids(app: &App) -> HashSet<String> {
    app.provider_settings
        .as_ref()
        .map(|settings| settings.connected.iter().cloned().collect())
        .unwrap_or_default()
}

/// Opens the panel on the active provider. Provider settings are
/// core-authoritative, so the local config is only a fallback until the first
/// read lands.
pub(crate) async fn open_settings(app: &mut App) {
    open_settings_at(app, None).await;
}

/// Opens the panel with `focus` selected, which is how the switcher sends a
/// user who picked a provider without a key straight to the row that fixes it.
pub(crate) async fn open_settings_at(app: &mut App, focus: Option<&str>) {
    let _ = refresh_provider_settings(app).await;
    // The panel is where the model list becomes useful, so its cache-only read
    // happens on open rather than on every frame.
    let _ = load_provider_models(app, false).await;
    let rows = provider_rows(app);
    let selected_row = focus
        .and_then(|id| rows.iter().position(|row| row.id == id))
        .or_else(|| rows.iter().position(|row| row.active))
        .unwrap_or(0);
    app.provider_editor = Some(ProviderEditor::new(app, rows, selected_row));
    app.current.status = "供应商设置".into();
}

/// Runs `body` with the editor borrowed mutably alongside a read-only view of
/// the facade. Taking the editor out and putting it back is what lets the draw
/// and hit-test paths read `App` while writing the editor's own rectangles.
pub(crate) fn with_editor<R>(
    app: &mut App,
    body: impl FnOnce(&App, &mut ProviderEditor) -> R,
) -> Option<R> {
    let mut editor = app.provider_editor.take()?;
    let result = body(app, &mut editor);
    app.provider_editor = Some(editor);
    Some(result)
}

/// Loads a left-pane row into the draft. Switching rows drops the previous
/// draft's unsaved edits along with every note that described them; keeping a
/// draft alive across rows would let one profile's typing land on another.
fn focus_row(app: &App, editor: &mut ProviderEditor, index: usize) {
    if editor.rows.is_empty() {
        return;
    }
    editor.selected_row = index.min(editor.rows.len() - 1);
    let config = match editor.rows.get(editor.selected_row) {
        Some(row) => draft_config_for_row(app, row),
        None => return,
    };
    editor.form = draft_form(app, config);
    editor.reset_transient();
}

impl ProviderEditor {
    /// Clears the per-draft state: notes, errors, the window buffer and the
    /// picker. Called whenever the draft's subject changes.
    fn reset_transient(&mut self) {
        self.window_input.clear();
        self.window_note = None;
        self.window_fetch_pending = false;
        self.delete_confirm = false;
        self.error = None;
        self.model_picker = EditorModelPicker::default();
        self.sync_field_selection();
    }
}

/// Starts a create draft. Every create is addressed by an empty id, so the core
/// mints a fresh `custom-<uuid>` and two creates can never collapse into one.
fn begin_create(app: &App, editor: &mut ProviderEditor) {
    let mut config = ProviderPreset::Custom.defaults();
    config.id = String::new();
    config.name = String::new();
    config.model = String::new();
    editor.form = draft_form(app, config);
    editor.field_index = EDITOR_FIELDS
        .iter()
        .position(|field| *field == EditorField::Name)
        .unwrap_or(0);
    editor.pane = EditorPane::Fields;
    editor.reset_transient();
}

/// Moves the highlight inside whichever pane owns it.
fn move_selection(app: &App, editor: &mut ProviderEditor, direction: i32) {
    if editor.pane == EditorPane::Fields {
        let len = EDITOR_FIELDS.len() as i32;
        editor.field_index = (editor.field_index as i32 + direction).rem_euclid(len) as usize;
        editor.sync_field_selection();
        return;
    }
    // The create command is the last address, one past the last profile.
    let len = editor.rows.len() as i32 + 1;
    let next = (editor.selected_row as i32 + direction).rem_euclid(len) as usize;
    if next < editor.rows.len() {
        focus_row(app, editor, next);
    } else {
        // Highlighting the create command only moves the highlight: the pane on
        // the right keeps editing the profile its title names, so scrolling
        // past the command can never silently discard a draft.
        editor.selected_row = next;
    }
}

fn toggle_pane(app: &mut App) {
    with_editor(app, |_, editor| {
        editor.pane = match editor.pane {
            EditorPane::Providers => EditorPane::Fields,
            EditorPane::Fields => EditorPane::Providers,
        };
    });
}

fn escape_editor(app: &mut App) {
    let pane = app.provider_editor.as_ref().map(|editor| editor.pane);
    match pane {
        // A case-back keeps the draft: leaving the fields by `Esc` is the same
        // as moving the highlight off them, not a discard.
        Some(EditorPane::Fields) => {
            with_editor(app, |_, editor| editor.pane = EditorPane::Providers);
        }
        Some(EditorPane::Providers) => {
            app.provider_editor = None;
            app.current.status = "设置已取消".into();
        }
        None => {}
    }
}

/// `←`/`→` on an enumerated row. The core's `cycle` owns the semantics and is a
/// no-op for the text rows, so this only has to stay off the rows whose value
/// is typed.
fn cycle_field(app: &mut App, direction: i32) {
    with_editor(app, |_, editor| {
        if editor.pane != EditorPane::Fields {
            return;
        }
        if !matches!(
            editor.field(),
            EditorField::Name | EditorField::ContextWindow
        ) && let Some(field) = editor.field().core()
        {
            editor.form.cycle(field, direction);
            editor.error = None;
        }
    });
}

/// Appends (`Some`) or deletes (`None`) one character of the highlighted row.
fn edit_field(app: &mut App, character: Option<char>) {
    let field = match app.provider_editor.as_ref() {
        Some(editor) if editor.pane == EditorPane::Fields => editor.field(),
        _ => return,
    };
    if !field.is_text() {
        return;
    }
    with_editor(app, |_, editor| {
        editor.error = None;
        match field {
            EditorField::Name => match character {
                Some(character)
                    if editor.form.provider.name.chars().count() < PROVIDER_NAME_MAX_CHARS =>
                {
                    editor.form.provider.name.push(character);
                }
                Some(_) => {}
                None => {
                    editor.form.provider.name.pop();
                }
            },
            EditorField::ContextWindow => match character {
                Some(character) if character.is_ascii_digit() => {
                    editor.window_input.push(character);
                }
                Some(_) => {}
                None => {
                    editor.window_input.pop();
                }
            },
            _ => {
                if let Some(field) = field.core() {
                    editor.form.edit(field, character);
                }
            }
        }
    });
}

/// `Delete` / `Ctrl+U`: empties the highlighted text row. Base URL and model are
/// replaced rather than appended to, because appending to a default would build
/// a value the core then rejects.
fn clear_field(app: &mut App) {
    let field = match app.provider_editor.as_ref() {
        Some(editor) if editor.pane == EditorPane::Fields => editor.field(),
        _ => return,
    };
    if !field.is_text() {
        return;
    }
    with_editor(app, |_, editor| {
        editor.error = None;
        match field {
            EditorField::Name => editor.form.provider.name.clear(),
            EditorField::ContextWindow => {
                editor.window_input.clear();
                editor.window_note = None;
            }
            _ => {
                if let Some(field) = field.core() {
                    editor.form.paste(field, "");
                }
            }
        }
    });
}

/// `Ctrl+R` on the model row: refetches the provider's `/models` in the
/// background, so the terminal loop never blocks on the network round trip.
///
/// The refresh is a modified key on purpose: a bare `r` would be claimed by the
/// shortcut instead of landing in the model id, and model ids are full of `r`.
fn refresh_models(app: &mut App) {
    if on_field(app, EditorField::Model) {
        spawn_model_refresh(app);
    }
}

/// `Ctrl+G` on the context-window row: forces the metadata refresh the core also
/// consults for models.dev, then fills the buffer when the answer lands.
fn fetch_window(app: &mut App) {
    if !on_field(app, EditorField::ContextWindow) {
        return;
    }
    let eligible = app
        .provider_editor
        .as_ref()
        .is_some_and(|editor| editor.window_fetch_available(app));
    if !eligible {
        with_editor(app, |_, editor| {
            editor.window_note = Some("只能获取当前生效供应商的模型窗口".to_owned());
        });
        return;
    }
    with_editor(app, |_, editor| {
        editor.window_fetch_pending = true;
        editor.window_note = None;
    });
    spawn_model_refresh(app);
}

/// Whether the field pane holds the keyboard and is sitting on `field`.
fn on_field(app: &App, field: EditorField) -> bool {
    app.provider_editor
        .as_ref()
        .is_some_and(|editor| editor.pane == EditorPane::Fields && editor.field() == field)
}

/// Fills the window buffer from a landed refresh. Called from the one place
/// that consumes `ModelRefreshResult`, so the panel never polls.
pub(super) fn fill_window_from_models(app: &mut App) {
    with_editor(app, |app, editor| {
        if !editor.window_fetch_pending {
            return;
        }
        editor.window_fetch_pending = false;
        if !fetched_applies(app, &editor.form) {
            editor.window_note = Some("只能获取当前生效供应商的模型窗口".to_owned());
            return;
        }
        let window = editor
            .model_choices(app)
            .into_iter()
            .find(|choice| choice.id == editor.form.provider.model.trim())
            .and_then(|choice| choice.window);
        editor.window_note = Some(match window {
            Some(window) => {
                editor.window_input = window.to_string();
                "已获取；保留则作为显式值优先生效，清空则交给自动解析。".to_owned()
            }
            None if app.provider_models.last_error.is_some() => {
                "刷新失败：网关不可达或密钥未配置。".to_owned()
            }
            None => "接口与内置注册表均未报告该模型窗口，请手填。".to_owned(),
        });
    });
}

fn begin_delete(app: &mut App) {
    let eligible = app.provider_editor.as_ref().is_some_and(|editor| {
        !editor.creating() && editor.selected_provider().is_some_and(|row| row.saved)
    });
    with_editor(app, |_, editor| {
        if eligible {
            editor.delete_confirm = true;
            editor.error = None;
        } else {
            editor.error = Some(if editor.creating() {
                "新建的供应商尚未保存，无需删除".to_owned()
            } else {
                "内置模板尚未保存，无需删除".to_owned()
            });
        }
    });
}

pub(crate) fn editor_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    let paste = code == KeyCode::Char('v')
        && modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::META);
    paste
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
        || (control && matches!(code, KeyCode::Char('s' | 'd' | 'u' | 'r' | 'g')))
        || matches!(code, KeyCode::Char(_) if !control)
}

pub(crate) async fn handle_editor_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    // The delete confirmation is modal: only its two answers and `Esc` act, so a
    // stray keystroke can never be read as "yes".
    if app
        .provider_editor
        .as_ref()
        .is_some_and(|editor| editor.delete_confirm)
    {
        match code {
            KeyCode::Char('y' | 'Y') => confirm_delete(app).await,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                with_editor(app, |_, editor| editor.delete_confirm = false);
            }
            _ => {}
        }
        return;
    }
    if app
        .provider_editor
        .as_ref()
        .is_some_and(|editor| editor.model_picker.open)
    {
        // The picker owns the keys it defines; anything else closes it and then
        // acts on the field, which is how the trailing "custom model" row hands
        // typing back to the text box.
        if matches!(
            code,
            KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter
        ) || (control && code == KeyCode::Char('r'))
        {
            handle_model_picker_key(app, code).await;
            return;
        }
        with_editor(app, |_, editor| editor.model_picker.open = false);
    }
    match code {
        KeyCode::Esc => escape_editor(app),
        KeyCode::Tab | KeyCode::BackTab => toggle_pane(app),
        KeyCode::Up => {
            with_editor(app, |app, editor| move_selection(app, editor, -1));
        }
        KeyCode::Down => {
            with_editor(app, |app, editor| move_selection(app, editor, 1));
        }
        KeyCode::Left => cycle_field(app, -1),
        KeyCode::Right => cycle_field(app, 1),
        KeyCode::Enter => enter(app).await,
        KeyCode::Backspace => edit_field(app, None),
        KeyCode::Delete => clear_field(app),
        KeyCode::Char('u') if control => clear_field(app),
        KeyCode::Char('s') if control => apply_editor(app).await,
        KeyCode::Char('d') if control => begin_delete(app),
        KeyCode::Char('r') if control => refresh_models(app),
        KeyCode::Char('g') if control => fetch_window(app),
        KeyCode::Char(character) if !control => edit_field(app, Some(character)),
        _ => {}
    }
}

/// `Enter`: on the left pane it loads (or creates) the highlighted profile and
/// moves to the fields; on the model row it opens the picker. Applying is
/// `Ctrl+S`, so Enter can never commit an edit by accident.
async fn enter(app: &mut App) {
    let state = app.provider_editor.as_ref().map(|editor| {
        (
            editor.pane,
            editor.on_add_row(),
            editor.selected_row,
            editor.field(),
        )
    });
    match state {
        Some((EditorPane::Providers, true, _, _)) => {
            with_editor(app, begin_create);
        }
        Some((EditorPane::Providers, false, index, _)) => {
            with_editor(app, |app, editor| {
                focus_row(app, editor, index);
                editor.pane = EditorPane::Fields;
            });
        }
        Some((EditorPane::Fields, _, _, EditorField::Model)) => open_model_picker(app),
        _ => {}
    }
}

/// Opens the in-panel picker with the draft's current model highlighted, or the
/// trailing "custom model" row when the value is not one of the offers.
fn open_model_picker(app: &mut App) {
    with_editor(app, |app, editor| {
        let choices = editor.model_choices(app);
        let current = editor.form.provider.model.trim().to_owned();
        editor.model_picker.selected = choices
            .iter()
            .position(|choice| choice.id == current)
            .unwrap_or(choices.len());
        editor.model_picker.open = true;
    });
}

async fn handle_model_picker_key(app: &mut App, code: KeyCode) {
    if code == KeyCode::Char('r') {
        spawn_model_refresh(app);
        return;
    }
    if code == KeyCode::Esc {
        with_editor(app, |_, editor| editor.model_picker.open = false);
        return;
    }
    let picked = with_editor(app, |app, editor| {
        let len = editor.model_choices(app).len() + 1;
        if len == 0 {
            editor.model_picker.selected = 0;
            return None;
        }
        let last = len - 1;
        if editor.model_picker.selected > last {
            // The list shrank while the picker stayed open: the next key reels
            // the cursor back onto the live list instead of wrapping away from
            // the row the painter highlights.
            editor.model_picker.selected = last;
            return None;
        }
        editor.model_picker.selected = match code {
            KeyCode::Up => (editor.model_picker.selected + last) % len,
            KeyCode::Down => (editor.model_picker.selected + 1) % len,
            _ => editor.model_picker.selected,
        };
        (code == KeyCode::Enter).then_some(editor.model_picker.selected)
    });
    let Some(Some(index)) = picked else {
        return;
    };
    with_editor(app, |app, editor| {
        let choices = editor.model_choices(app);
        match choices.get(index) {
            Some(choice) => {
                editor.form.provider.model = choice.id.clone();
                editor.error = None;
                editor.model_picker.open = false;
            }
            None => {
                // The trailing row: the value is kept and the Model row stays
                // focused, so the next keystroke edits it in place.
                editor.field_index = EDITOR_FIELDS
                    .iter()
                    .position(|field| *field == EditorField::Model)
                    .unwrap_or(0);
                editor.sync_field_selection();
                editor.model_picker.open = false;
            }
        }
    });
}

/// The model row the cursor is over, resolved through the same window the
/// painter used. `None` means the click missed the item window; an index equal
/// to the choice count is the trailing "custom model" row.
pub(super) fn model_picker_selection(
    app: &App,
    editor: &ProviderEditor,
    column: u16,
    row: u16,
) -> Option<usize> {
    let geometry = editor.model_picker.geometry?;
    let index = geometry.item_at(column, row)?;
    (index <= editor.model_choices(app).len()).then_some(index)
}

pub(crate) fn paste_into_editor(app: &mut App, text: &str) -> bool {
    let field = match app.provider_editor.as_ref() {
        Some(editor) if editor.pane == EditorPane::Fields && !editor.model_picker.open => {
            editor.field()
        }
        _ => return false,
    };
    let sanitized = text.replace(['\r', '\n'], "");
    let mut sanitized = sanitized.as_str();
    if sanitized.len() > crate::clipboard::MAX_CLIPBOARD_BYTES {
        let mut end = crate::clipboard::MAX_CLIPBOARD_BYTES;
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized = &sanitized[..end];
    }
    match field {
        EditorField::Name => {
            let name = sanitized
                .chars()
                .take(PROVIDER_NAME_MAX_CHARS)
                .collect::<String>();
            with_editor(app, |_, editor| {
                editor.form.provider.name = name;
                editor.error = None;
            })
            .is_some()
        }
        EditorField::ContextWindow => {
            if !sanitized
                .chars()
                .all(|character| character.is_ascii_digit())
            {
                return false;
            }
            let window = sanitized.to_owned();
            with_editor(app, |_, editor| {
                editor.window_input = window;
                editor.window_note = None;
                editor.error = None;
            })
            .is_some()
        }
        EditorField::Model | EditorField::BaseUrl | EditorField::ApiKey => {
            let text = sanitized.to_owned();
            let field = field.core().expect("core-owned row");
            with_editor(app, |_, editor| {
                // Paste replaces: appending to a default base URL or model would
                // produce a value the core then rejects.
                editor.form.paste(field, &text);
                editor.error = None;
            })
            .is_some()
        }
        EditorField::Template | EditorField::Protocol | EditorField::Thinking => false,
    }
}

/// Mouse handling for the panel. The model picker is resolved first, then the
/// pinned action row, then the left rows and the field rows — each from the
/// rectangle the frame painted for it, in the same order the painter laid them
/// out.
pub(super) async fn handle_editor_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Option<EventOutcome> {
    app.provider_editor.as_ref()?;
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return Some(EventOutcome::default());
    }
    if app
        .provider_editor
        .as_ref()
        .is_some_and(|editor| editor.model_picker.open)
    {
        let hit = with_editor(app, |app, editor| {
            model_picker_selection(app, editor, mouse.column, mouse.row)
        })
        .flatten();
        match hit {
            Some(index) => {
                with_editor(app, |app, editor| {
                    match editor.model_choices(app).get(index) {
                        Some(choice) => editor.form.provider.model = choice.id.clone(),
                        // The trailing row keeps the value and only hands focus
                        // back to the Model field, so the next keystroke edits
                        // the model in place.
                        None => {
                            editor.field_index = EDITOR_FIELDS
                                .iter()
                                .position(|field| *field == EditorField::Model)
                                .unwrap_or(0);
                            editor.sync_field_selection();
                        }
                    }
                    editor.error = None;
                    editor.model_picker.open = false;
                });
            }
            None => {
                with_editor(app, |_, editor| editor.model_picker.open = false);
            }
        }
        return Some(EventOutcome::redraw());
    }
    let action = with_editor(app, |_, editor| {
        editor
            .action_rects
            .iter()
            .find(|(_, rect)| point_in_rect(mouse.column, mouse.row, *rect))
            .map(|(action, _)| *action)
    })
    .flatten();
    if let Some(action) = action {
        match action {
            EditorAction::Apply => apply_editor(app).await,
            EditorAction::Delete => begin_delete(app),
            EditorAction::Cancel => escape_editor(app),
        }
        return Some(EventOutcome::redraw());
    }
    let row = with_editor(app, |_, editor| {
        editor
            .row_rects
            .iter()
            .position(|rect| point_in_rect(mouse.column, mouse.row, *rect))
    })
    .flatten();
    if let Some(index) = row {
        if index < provider_rows_len(app) {
            with_editor(app, |app, editor| {
                focus_row(app, editor, index);
                editor.pane = EditorPane::Fields;
            });
        } else {
            with_editor(app, begin_create);
        }
        return Some(EventOutcome::redraw());
    }
    let field = with_editor(app, |_, editor| {
        editor
            .field_rects
            .iter()
            .position(|rect| point_in_rect(mouse.column, mouse.row, *rect))
    })
    .flatten();
    if let Some(index) = field {
        with_editor(app, |_, editor| {
            editor.field_index = index.min(EDITOR_FIELDS.len() - 1);
            editor.pane = EditorPane::Fields;
            editor.sync_field_selection();
        });
        return Some(EventOutcome::redraw());
    }
    // A click anywhere else inside the panel is swallowed rather than passed
    // through, so it can never reach a control the panel covers.
    Some(EventOutcome::default())
}

fn provider_rows_len(app: &App) -> usize {
    app.provider_editor
        .as_ref()
        .map(|editor| editor.rows.len())
        .unwrap_or(0)
}

/// `Ctrl+S` inside the panel: commit the draft through the core's profile
/// endpoint, which is the same call the WebUI makes.
async fn apply_editor(app: &mut App) {
    let Some(mut editor) = app.provider_editor.take() else {
        return;
    };
    if let Some(reason) = editor.blocked_reason() {
        editor.error = Some(reason);
        app.provider_editor = Some(editor);
        return;
    }
    if let Err(error) = commit_editor(app, &mut editor).await {
        editor.error = Some(secrets::redact(&error.to_string()));
        // A failed apply keeps the panel, and every edit the user made with it.
        app.provider_editor = Some(editor);
    }
    // On success the taken-out draft is dropped rather than restored:
    // `commit_editor` has already installed the editor it reloaded from the
    // core, and putting the stale draft back would silently undo that.
}

async fn commit_editor(app: &mut App, editor: &mut ProviderEditor) -> Result<()> {
    let provider_id = editor.form.provider.id.trim().to_owned();
    let creating = provider_id.is_empty();
    let name = editor.form.provider.name.trim().to_owned();
    let provider = editor.form.prepare()?;
    let context_window = parse_window_override(&editor.window_input)?;
    let entered_key = editor.form.api_key.trim().to_owned();

    // Ordering matters: `store_api_key_cached` seeds the in-process cache even
    // when the OS keyring write fails, so the core's rebuilt runner can pick the
    // new key up immediately. The warning below marks the degraded case. For a
    // brand-new custom provider the id does not exist yet, so the key is staged
    // under the family id and the core promotes it while creating.
    let key_stage_id = if creating {
        provider.preset.key_id().to_owned()
    } else {
        provider_id.clone()
    };
    let key_warning = if entered_key.is_empty() {
        None
    } else {
        secrets::store_api_key_cached(provider.preset, &key_stage_id, &entered_key)
            .err()
            .map(|error| {
                format!(
                    "API Key 仅本次运行有效：{}",
                    secrets::redact(&error.to_string())
                )
            })
    };

    // Thinking is a full-profile field the profile endpoint does not carry.
    // While the edited provider is active and the user changed it, commit the
    // merged full profile once instead of saving twice; for a non-active
    // provider the merge endpoint is the only path, and thinking is edited
    // through the top-level menu once that provider is active. Only an
    // *existing* profile can be active: a create always goes through the create
    // path so the core mints its id.
    let active = !creating && app.config.provider.id() == provider_id;
    let thinking_changed = active
        && (provider.thinking != app.config.provider.thinking
            || provider.thinking_level != app.config.provider.thinking_level
            || provider.thinking_budget_tokens != app.config.provider.thinking_budget_tokens);
    let thinking_warning = (!active
        && (provider.thinking != provider.preset.defaults().thinking
            || provider.thinking_level != provider.preset.defaults().thinking_level
            || provider.thinking_budget_tokens
                != provider.preset.defaults().thinking_budget_tokens))
        .then(|| "思考设置请在切换为当前供应商后通过顶部菜单修改".to_owned());

    if active {
        app.cancel_model_refresh();
    }
    if thinking_changed {
        let mut merged = app.config.provider.clone();
        merged.id = provider_id.clone();
        merged.name = name.clone();
        merged.preset = provider.preset;
        merged.model = provider.model.clone();
        if !provider.base_url.trim().is_empty() {
            merged.base_url = provider.base_url.clone();
        }
        merged.kind = provider.kind;
        if let Some(window) = context_window {
            merged.context_window_tokens = Some(window);
        }
        merged.thinking = provider.thinking;
        merged.thinking_level = provider.thinking_level;
        merged.thinking_budget_tokens = provider.thinking_budget_tokens;
        merged.normalize_thinking();
        if let Err(error) = app.handle.set_provider_config(merged).await {
            return Err(anyhow::anyhow!(secrets::redact(&error.message)));
        }
    } else if let Err(error) = app
        .handle
        .set_provider_profile(
            &provider_id,
            provider.preset,
            Some(name.as_str()),
            &provider.model,
            (!provider.base_url.trim().is_empty()).then_some(provider.base_url.as_str()),
            Some(provider.kind),
            context_window,
            None,
        )
        .await
    {
        return Err(anyhow::anyhow!(secrets::redact(&error.message)));
    }

    // The core persisted the profile once; every derived surface is reread from
    // it instead of writing the local config a second time.
    let _ = refresh_provider_settings(app).await;
    let _ = load_provider_models(app, false).await;
    app.current.context_limit_tokens = None;
    app.sync_all().await?;

    // Reselect what was just written. A create has no id yet, so its new row is
    // found by the name the core stored.
    let rows = provider_rows(app);
    let index = rows
        .iter()
        .position(|row| !provider_id.is_empty() && row.id == provider_id)
        .or_else(|| {
            rows.iter().position(|row| {
                row.preset == provider.preset && row.label == name && !name.is_empty()
            })
        })
        .or_else(|| rows.iter().position(|row| row.active))
        .unwrap_or(0);
    let mut reloaded = ProviderEditor::new(app, rows, index);
    reloaded.pane = EditorPane::Fields;
    let warnings = [key_warning, thinking_warning]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    if !warnings.is_empty() {
        reloaded.error = Some(warnings);
    }
    let label = if name.is_empty() {
        provider.preset.label().to_owned()
    } else {
        name.clone()
    };
    app.provider_editor = Some(reloaded);
    app.current.status = format!("已保存 | {label} | {}", provider.model);
    Ok(())
}

/// Parses the optional context-window override. Empty inherits the merged
/// profile value; a number goes to the core, which clamps it.
fn parse_window_override(input: &str) -> Result<Option<u64>> {
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

/// `Ctrl+D` confirmed: drops the profile through the core, which keeps the
/// keyring entry — the key is a credential, not part of the profile.
async fn confirm_delete(app: &mut App) {
    let Some(editor) = app.provider_editor.take() else {
        return;
    };
    let id = editor
        .selected_provider()
        .map(|row| row.id.clone())
        .unwrap_or_default();
    let result = if id.is_empty() {
        Ok(())
    } else {
        app.cancel_model_refresh();
        match app.handle.remove_provider(&id).await {
            Ok(()) => {
                let _ = refresh_provider_settings(app).await;
                app.provider_models = ProviderModelsState::default();
                let _ = load_provider_models(app, false).await;
                app.sync_all().await
            }
            Err(error) => Err(anyhow::anyhow!(secrets::redact(&error.message))),
        }
    };
    match result {
        Ok(()) => {
            let rows = provider_rows(app);
            let index = rows.iter().position(|row| row.active).unwrap_or(0);
            app.provider_editor = Some(ProviderEditor::new(app, rows, index));
            app.current.status = "供应商已移除；API Key 已保留在系统钥匙串".into();
        }
        Err(error) => {
            let mut editor = editor;
            editor.delete_confirm = false;
            editor.error = Some(secrets::redact(&error.to_string()));
            app.provider_editor = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_pane_rows_cover_every_editable_field_once() {
        let mut seen: Vec<EditorField> = Vec::new();
        for field in EDITOR_FIELDS {
            assert!(!field.label().is_empty());
            assert!(!seen.contains(&field), "{field:?} is listed twice");
            seen.push(field);
        }
        // Every core field is reachable except the preset, which the template
        // row shows read-only: a profile's preset is its identity.
        let core: Vec<SettingsField> = EDITOR_FIELDS
            .iter()
            .filter_map(|field| field.core())
            .collect();
        assert_eq!(core.len(), FIELDS.len() - 1);
        assert!(!core.contains(&SettingsField::Preset));
        assert_eq!(EDITOR_FIELDS[0], EditorField::Name);
        assert_eq!(EDITOR_FIELDS[EDITOR_FIELDS.len() - 1], EditorField::ApiKey);
    }

    #[test]
    fn typing_rewrites_exactly_the_text_rows() {
        let text: Vec<EditorField> = EDITOR_FIELDS
            .iter()
            .copied()
            .filter(|field: &EditorField| field.is_text())
            .collect();
        assert_eq!(
            text,
            vec![
                EditorField::Name,
                EditorField::Model,
                EditorField::BaseUrl,
                EditorField::ContextWindow,
                EditorField::ApiKey,
            ]
        );
    }

    #[test]
    fn model_labels_only_decorate_a_real_window() {
        assert_eq!(model_label("gpt-5", None), "gpt-5");
        assert_eq!(model_label("gpt-5", Some(0)), "gpt-5");
        assert_eq!(model_label("gpt-5", Some(128_000)), "gpt-5 · 128k");
        assert_eq!(model_label("gpt-5", Some(1_000_000)), "gpt-5 · 1m");
    }

    #[test]
    fn the_window_override_only_accepts_a_positive_integer() {
        assert_eq!(parse_window_override("").unwrap(), None);
        assert_eq!(parse_window_override(" 200000 ").unwrap(), Some(200_000));
        assert!(parse_window_override("0").is_err());
        assert!(parse_window_override("12k").is_err());
    }
}
