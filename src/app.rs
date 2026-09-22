use std::{
    collections::{HashMap, HashSet},
    future::pending,
    io,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    style::Print,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ignore::WalkBuilder;
use protium_core::{
    agent::ChildSessionProgress,
    commands::{self, AgentMode, Command, TodoCommand},
    config::{
        Config, ProviderKind, ProviderPreset, ThinkingLevel, ThinkingProfile, ThinkingProfileKind,
        thinking_profile,
    },
    protocol::{
        AppSnapshotV2, ApprovalDto, Envelope, Event as ProtocolEvent, ProviderModelDto,
        ProviderModelsDto, ProviderProfileDto, ProviderSettingsDto,
    },
    secrets,
    security::Workspace,
    service::{AppHandle, AppService, CoreConfig},
    settings::{FIELDS, SettingsField, SettingsForm, SettingsState},
    storage::SessionSummary,
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use crate::{
    home::{self, HomeAction, HomeSelection, HomeState, RECENT_SESSION_LIMIT},
    input::InputBuffer,
    output::{EdgeScroll, InteractionTarget, OutputSelection},
    projection::{ApprovalDisplay, TuiSessionProjection},
    ui,
};

const MOUSE_WHEEL_SCROLL_LINES: isize = 1;
const DEFERRED_REDRAW_INTERVAL: Duration = Duration::from_millis(16);

pub use protium_core::model::{
    AgentPhase, DisplayContent, DisplayEntry, DisplayKind, ModelPhase, PendingApproval,
    ThinkingDisplay, ThinkingResult, TodoDisplay, TodoStatus, TodoTask, ToolDisplay,
    ToolDisplayStatus,
};

/// How the user answered a pending approval prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalChoice {
    Approve,
    Reject,
    AlwaysSession,
}

#[derive(Clone, Debug)]
pub struct CommandPaletteState {
    pub query: String,
    pub selected: usize,
}

/// Outcome of handling a terminal event: whether to redraw and whether an
/// OSC 52 clipboard sequence should be emitted.
struct EventOutcome {
    redraw: bool,
    osc52: Option<String>,
}

impl EventOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            osc52: None,
        }
    }

    fn default() -> Self {
        Self {
            redraw: false,
            osc52: None,
        }
    }
}

/// TUI cache of the core's dynamic provider model list. The core remains the
/// source of truth; this only mirrors the last `provider_models` answer for the
/// active preset so menus can render without another round trip.
#[derive(Clone, Debug, Default)]
pub struct ProviderModelsState {
    pub preset: Option<ProviderPreset>,
    pub models: Vec<ProviderModelDto>,
    pub fetched_at: Option<i64>,
    pub loading: bool,
    pub last_error: Option<String>,
}

/// Result of a background `provider_models(true)` refresh.
#[derive(Debug)]
pub(crate) struct ModelRefreshResult {
    pub generation: u64,
    pub preset: ProviderPreset,
    pub result: std::result::Result<ProviderModelsDto, String>,
}

/// One model picker entry: the stable id plus its display label. Deduping and
/// selection always key off `id`, never the decorated label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    pub id: String,
    pub label: String,
}

/// TUI facade. Owns the terminal shell, the display projection of the active
/// session, and every TUI-only interaction state (menus, scroll, layout,
/// mouse). All mutation is delegated to [`AppHandle`]; this struct never
/// touches the core's internal runtime, storage, registry or approvals.
pub struct App {
    pub handle: AppHandle,
    pub workspace: PathBuf,
    pub workspace_security: Workspace,
    /// Shared configuration for display and the settings page. The core owns
    /// the authoritative copy; every mutation goes through the handle.
    pub config: Config,
    /// Core-authoritative provider settings view: active profile, saved
    /// profiles and the presets with a currently resolvable API key.
    pub provider_settings: Option<ProviderSettingsDto>,
    /// Last `provider_models(false)` answer for the active preset.
    pub provider_models: ProviderModelsState,
    /// Background model-refresh result channel (sender side).
    pub(crate) model_refresh_tx: tokio::sync::mpsc::Sender<ModelRefreshResult>,
    /// Background model-refresh result channel (receiver side; taken by the
    /// event loop so its `select!` branch never borrows the whole facade).
    pub(crate) model_refresh_rx: Option<tokio::sync::mpsc::Receiver<ModelRefreshResult>>,
    /// At most one provider model refresh may be in flight. The handle also
    /// gives provider changes and shutdown an explicit cancellation path.
    model_refresh_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonic identity for refresh requests; results from older provider
    /// state are ignored even if cancellation races with completion.
    model_refresh_generation: u64,
    /// TUI-only settings selection: the core `FIELDS` rows followed by the
    /// synthetic context-window override row.
    pub settings_field_index: usize,
    /// Write-only context-window override buffer. Empty means "inherit the
    /// merged profile value"; digits are passed to the core, which clamps.
    pub context_window_input: String,
    pub input: InputBuffer,
    pub context_meter_enabled: bool,
    pub settings: Option<SettingsState>,
    pub settings_rect: Option<Rect>,
    pub palette: Option<CommandPaletteState>,
    pub thinking_menu_open: bool,
    pub thinking_control_rect: Option<Rect>,
    pub thinking_menu_rect: Option<Rect>,
    pub session_panel_rect: Option<Rect>,
    pub input_mode_rect: Option<Rect>,
    pub provider_control_rect: Option<Rect>,
    pub model_control_rect: Option<Rect>,
    pub provider_menu_open: bool,
    pub provider_menu_rect: Option<Rect>,
    pub provider_menu_selected: usize,
    pub model_menu_open: bool,
    pub model_menu_rect: Option<Rect>,
    pub model_menu_selected: usize,
    pub todo_window_rect: Option<Rect>,
    pub force_full_redraw: bool,
    pub mouse_press_target: Option<InteractionTarget>,
    pub mouse_press_position: Option<(u16, u16)>,
    pub mouse_dragged: bool,
    pub layout_restore_anchor: Option<(InteractionTarget, usize)>,
    pub file_suggestions: Vec<String>,
    pub file_selected: usize,
    pub sessions: Vec<SessionSummary>,
    pub expanded_sessions: HashSet<String>,
    pub child_status: HashMap<String, ChildSessionProgress>,
    pub child_batches: HashMap<String, HashSet<String>>,
    pub active_session: String,
    /// Display projection of the active session.
    pub current: TuiSessionProjection,
    /// The globally oldest pending approval surfaced by the snapshot.
    pub approval: Option<ApprovalDisplay>,
    /// Live event stream cursor; events after it are replayed then live.
    pub event_cursor: u64,
    pub should_quit: bool,
    /// True while a snapshot/message refetch is pending; suppresses extra
    /// refetches from bursty events.
    pub sync_pending: bool,
    /// The request sequence of the last submit the facade started, per session.
    /// Passed to `cancel` so a stale cancel never aborts a newer request.
    pub request_ids: HashMap<String, u64>,
    /// Set when the user scrolled to the top of the loaded history and older
    /// messages exist; the event loop drains it into `load_older_history`.
    pub history_load_pending: bool,
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(task) = self.model_refresh_task.take() {
            task.abort();
        }
    }
}

pub async fn run(workspace_path: PathBuf, mut config: Config) -> Result<()> {
    let _workspace = Workspace::new(&workspace_path)?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("cannot create data directory {}", config.data_dir.display()))?;

    let core_config = CoreConfig {
        workspace: workspace_path.clone(),
        config: config.clone(),
        data_dir: config.data_dir.clone(),
        event_capacity: config.server.event_buffer,
        event_max_bytes: config.server.event_max_bytes,
        approval_timeout: Duration::from_secs(config.server.approval_timeout_seconds),
        message_page_size: 100,
    };
    let handle = AppService::start(core_config).await?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    ) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    // Best-effort kitty keyboard protocol enhancement. Terminals without
    // support ignore it and the legacy Windows console API returns
    // Unsupported, so a failure here must not prevent startup.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    );
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                PopKeyboardEnhancementFlags,
                LeaveAlternateScreen,
                DisableMouseCapture,
                DisableBracketedPaste
            );
            return Err(error.into());
        }
    };
    if let Err(error) = terminal.clear() {
        let _ = disable_raw_mode();
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
        let _ = execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        return Err(error.into());
    }

    let result = async {
        let snapshot = handle.snapshot().await?;
        let recent_sessions = snapshot
            .sessions
            .iter()
            .filter(|session| session.parent_id.is_none())
            .take(RECENT_SESSION_LIMIT)
            .map(session_summary)
            .collect();
        let mut home = HomeState::new(
            &workspace_path,
            config.provider.clone(),
            config.providers.clone(),
            recent_sessions,
        );
        let action = home_event_loop(&mut terminal, &mut home).await?;
        if action == HomeAction::Quit {
            handle.shutdown().await?;
            return Ok(());
        }
        let selection = matches!(action, HomeAction::StartNew(_)).then(|| home.selection());
        home.set_loading();
        terminal.draw(|frame| home::draw(frame, &mut home))?;
        let Some((session_id, first_prompt)) =
            resolve_home_action(&handle, &mut config, action).await?
        else {
            handle.shutdown().await?;
            return Ok(());
        };
        if let Some(selection) = selection {
            apply_home_selection(&handle, &config, &session_id, &selection).await?;
        }
        drop(home);
        let snapshot = handle.snapshot().await?;
        let mut app = build_app(handle, snapshot, workspace_path.clone(), config).await?;
        if let Some(prompt) = first_prompt {
            app.input.set(prompt);
            app.submit_current().await?;
        }
        event_loop(&mut terminal, &mut app).await
    }
    .await;

    // Best-effort teardown so the terminal is restored even on error.
    let raw_mode_result = disable_raw_mode();
    let screen_result = execute!(
        terminal.backend_mut(),
        PopKeyboardEnhancementFlags,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    );
    let cursor_result = terminal.show_cursor();
    result?;
    raw_mode_result?;
    screen_result?;
    cursor_result?;
    Ok(())
}

fn session_summary(session: &protium_core::protocol::SessionStateDto) -> SessionSummary {
    SessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        parent_id: session.parent_id.clone(),
    }
}

/// Creates or resumes the session chosen on the home page. Provider selection
/// is applied to the core before the session exists so the first request uses
/// the chosen provider; the mode is applied to the freshly created session.
async fn resolve_home_action(
    handle: &AppHandle,
    _config: &mut Config,
    action: HomeAction,
) -> Result<Option<(String, Option<String>)>> {
    match action {
        HomeAction::StartNew(prompt) => {
            // The session is created by the core on the first command.
            handle.execute_command(None, "/new").await?;
            let snapshot = handle.snapshot().await?;
            let session_id = snapshot
                .active_session
                .context("no active session after creating one")?;
            Ok(Some((session_id, Some(prompt))))
        }
        HomeAction::Resume(session_id) => {
            handle.activate_session(&session_id).await?;
            Ok(Some((session_id, None)))
        }
        HomeAction::Quit => Ok(None),
    }
}

/// Applies the home page's provider and mode selections through the core.
async fn apply_home_selection(
    handle: &AppHandle,
    config: &Config,
    session_id: &str,
    selection: &HomeSelection,
) -> Result<()> {
    if selection.provider.preset != config.provider.preset {
        let _ = secrets::api_key_cached(selection.provider.preset);
        handle
            .set_provider_config(selection.provider.clone())
            .await?;
    }
    let mode_command = match selection.mode {
        AgentMode::Build => "/build",
        AgentMode::Plan => "/plan",
        AgentMode::Explore => "/explore",
        AgentMode::Cluster => "/cluster",
    };
    handle
        .execute_command(Some(session_id.to_owned()), mode_command)
        .await?;
    Ok(())
}

async fn build_app(
    handle: AppHandle,
    snapshot: AppSnapshotV2,
    workspace: PathBuf,
    config: Config,
) -> Result<App> {
    let active_session = snapshot.active_session.clone().unwrap_or_default();
    let mode = AgentMode::parse(&snapshot.mode).unwrap_or_default();
    // Context capacity is core-authoritative: seed from the snapshot budget,
    // never from local config inference.
    let context_limit_tokens = snapshot
        .context
        .as_ref()
        .and_then(|context| context.context_window_tokens);
    let mut current = TuiSessionProjection::new(active_session.clone(), mode, context_limit_tokens);
    if let Some(session) = snapshot
        .sessions
        .iter()
        .find(|session| session.id == active_session)
    {
        current.title = session.title.clone();
        current.parent_id = session.parent_id.clone();
        current.status = session.status.clone();
        current.busy = session.busy;
        if let Ok(phase) = parse_phase(&session.phase) {
            current.agent_phase = phase;
        }
    }
    let sessions = snapshot.sessions.iter().map(session_summary).collect();
    let workspace_security = Workspace::new(&workspace)?;
    let context_meter_enabled = config.ui.context_meter;
    let (model_refresh_tx, model_refresh_rx) = tokio::sync::mpsc::channel(1);
    let mut app = App {
        handle,
        workspace,
        workspace_security,
        config,
        provider_settings: None,
        provider_models: ProviderModelsState::default(),
        model_refresh_tx,
        model_refresh_rx: Some(model_refresh_rx),
        model_refresh_task: None,
        model_refresh_generation: 0,
        settings_field_index: 0,
        context_window_input: String::new(),
        input: InputBuffer::new(),
        context_meter_enabled,
        settings: None,
        settings_rect: None,
        palette: None,
        thinking_menu_open: false,
        thinking_control_rect: None,
        thinking_menu_rect: None,
        session_panel_rect: None,
        input_mode_rect: None,
        provider_control_rect: None,
        model_control_rect: None,
        provider_menu_open: false,
        provider_menu_rect: None,
        provider_menu_selected: 0,
        model_menu_open: false,
        model_menu_rect: None,
        model_menu_selected: 0,
        todo_window_rect: None,
        force_full_redraw: true,
        mouse_press_target: None,
        mouse_press_position: None,
        mouse_dragged: false,
        layout_restore_anchor: None,
        file_suggestions: Vec::new(),
        file_selected: 0,
        sessions,
        expanded_sessions: HashSet::new(),
        child_status: HashMap::new(),
        child_batches: HashMap::new(),
        active_session: active_session.clone(),
        current,
        approval: None,
        event_cursor: snapshot.event_cursor,
        should_quit: false,
        sync_pending: false,
        request_ids: HashMap::new(),
        history_load_pending: false,
    };
    app.sync_from_snapshot(&snapshot);
    // Seed the core-authoritative provider surfaces before the first draw.
    // Both calls are cache-only and never block on the network.
    let _ = refresh_provider_settings(&mut app).await;
    let _ = load_provider_models(&mut app, false).await;
    app.load_history().await?;
    Ok(app)
}

fn parse_phase(phase: &str) -> std::result::Result<AgentPhase, ()> {
    // The snapshot carries the `AgentPhase::label()` output (uppercase, e.g.
    // "THINKING" / "STREAMING_TOOL_CALL"); compare case-insensitively so the
    // snapshot merge never resets a live phase to Idle. Unknown phases keep the
    // current projection state (Err) instead of clobbering it.
    let normalized = phase.to_ascii_lowercase();
    match normalized.as_str() {
        "idle" => Ok(AgentPhase::Idle),
        "thinking" => Ok(AgentPhase::Thinking),
        "streaming_text" => Ok(AgentPhase::StreamingText),
        "streaming_tool_call" => Ok(AgentPhase::StreamingToolCall),
        "waiting_approval" => Ok(AgentPhase::WaitingApproval),
        "tool_running" => Ok(AgentPhase::ToolRunning),
        "completed" => Ok(AgentPhase::Completed),
        "failed" => Ok(AgentPhase::Failed),
        _ => Err(()),
    }
}

async fn home_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    home: &mut HomeState,
) -> Result<HomeAction> {
    let mut terminal_events = EventStream::new();
    terminal.draw(|frame| home::draw(frame, home))?;
    loop {
        let Some(Ok(event)) = terminal_events.next().await else {
            return Ok(HomeAction::Quit);
        };
        let outcome = home.handle_event(event);
        if let Some(action) = outcome.action {
            return Ok(action);
        }
        if outcome.redraw {
            terminal.draw(|frame| home::draw(frame, home))?;
        }
    }
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut terminal_events = EventStream::new();
    let mut live = subscribe_live(app).await?;
    let mut model_refresh_rx = app
        .model_refresh_rx
        .take()
        .context("model refresh receiver already taken")?;
    let mut edge_scroll_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut thinking_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut deferred_redraw_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    terminal.draw(|frame| ui::draw(frame, app))?;

    while !app.should_quit {
        if app
            .current
            .output_selection
            .is_some_and(|selection| selection.dragging)
            && app.current.edge_scroll.direction != 0
            && edge_scroll_timer.is_none()
        {
            edge_scroll_timer = Some(Box::pin(tokio::time::sleep(
                std::time::Duration::from_millis(80),
            )));
        }
        if !app
            .current
            .output_selection
            .is_some_and(|selection| selection.dragging)
            || app.current.edge_scroll.direction == 0
        {
            edge_scroll_timer = None;
        }
        if app.current.thinking_active && thinking_timer.is_none() {
            thinking_timer = Some(Box::pin(tokio::time::sleep(
                std::time::Duration::from_millis(100),
            )));
        }
        if !app.current.thinking_active {
            thinking_timer = None;
        }
        let edge_scroll_tick = async {
            if let Some(timer) = edge_scroll_timer.as_mut() {
                timer.await;
            } else {
                pending::<()>().await;
            }
        };
        let thinking_tick = async {
            if let Some(timer) = thinking_timer.as_mut() {
                timer.await;
            } else {
                pending::<()>().await;
            }
        };
        let deferred_redraw_tick = async {
            if let Some(timer) = deferred_redraw_timer.as_mut() {
                timer.await;
            } else {
                pending::<()>().await;
            }
        };
        let mut redraw = false;
        tokio::select! {
            refresh = model_refresh_rx.recv() => {
                if let Some(refresh) = refresh {
                    apply_model_refresh_result(app, refresh);
                    redraw = true;
                } else {
                    break;
                }
            }
            _ = deferred_redraw_tick => {
                deferred_redraw_timer = None;
                redraw = true;
            }
            _ = edge_scroll_tick => {
                edge_scroll_timer = None;
                auto_scroll_selection(app);
                redraw = true;
            }
            _ = thinking_tick => {
                thinking_timer = None;
                app.current.thinking_animation_frame =
                    app.current.thinking_animation_frame.wrapping_add(1);
                redraw = true;
            }
            terminal_event = terminal_events.next() => {
                match terminal_event {
                    Some(Ok(event)) => {
                        let coalesce = should_coalesce_terminal_redraw(&event);
                        let outcome = handle_terminal_event(app, event).await?;
                        redraw = outcome.redraw;
                        if redraw && coalesce {
                            schedule_deferred_redraw(&mut deferred_redraw_timer);
                            redraw = false;
                        }
                        if let Some(sequence) = outcome.osc52 {
                            execute!(terminal.backend_mut(), Print(sequence))?;
                        }
                        // Scrolling to the top of the loaded history requests
                        // the previous page; drained in the sync phase below.
                        if app.current.at_history_top() {
                            app.history_load_pending = true;
                        }
                    }
                    Some(Err(error)) => return Err(error.into()),
                    None => break,
                }
            }
            envelope = live.recv() => {
                match envelope {
                    Ok(envelope) => {
                        let coalesce =
                            should_coalesce_stream_redraw(&app.active_session, &envelope);
                        if let Some(fresh_redraw) = accept_envelope(app, &envelope) {
                            redraw = fresh_redraw;
                            if redraw && coalesce {
                                schedule_deferred_redraw(&mut deferred_redraw_timer);
                                redraw = false;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // The receiver fell behind the live broadcast. Refetch
                        // snapshot + message page and subscribe with a fresh
                        // receiver from the new cursor; never keep consuming the
                        // stale one.
                        app.sync_pending = false;
                        app.resync_all().await?;
                        live = subscribe_live(app).await?;
                        redraw = true;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
        if app.sync_pending {
            app.sync_pending = false;
            app.sync_all().await?;
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
        if app.history_load_pending {
            app.history_load_pending = false;
            app.load_older_history().await?;
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
    }
    app.cancel_model_refresh();
    Ok(())
}

/// Establishes a live subscription from the facade's last-processed cursor.
/// The bridge subscribes atomically (live first, then ring snapshot), so every
/// event pushed between the snapshot and the subscription is replayed exactly
/// once. When the cursor has been evicted the facade refetches the snapshot and
/// message page and retries from the fresh cursor. Returns a new live receiver.
async fn subscribe_live(app: &mut App) -> Result<tokio::sync::broadcast::Receiver<Arc<Envelope>>> {
    loop {
        match app.handle.subscribe_from(app.event_cursor) {
            Ok(subscription) => {
                // Replay buffered events first, advancing the processed cursor
                // so overlapping live deliveries are deduplicated.
                for envelope in &subscription.replay {
                    if envelope.cursor > app.event_cursor {
                        handle_envelope(app, envelope);
                        app.event_cursor = envelope.cursor;
                    }
                }
                return Ok(subscription.live);
            }
            Err(protium_core::bridge::ResyncRequired) => {
                // The requested cursor is gone; rebuild from a fresh snapshot
                // and message page before subscribing again.
                app.resync_all().await?;
            }
        }
    }
}

/// Whether a stream event for the active session may be batched into the 16ms
/// coalescing window instead of forcing an immediate repaint.
///
/// `ReasoningCompleted` is a phase barrier: it retires the thinking view and
/// must be drawn on its own so the user sees a frame with only the finished
/// thinking summary before the answer body starts streaming below it. It is
/// therefore never coalesced — the frame is painted right away.
fn should_coalesce_stream_redraw(active_session: &str, envelope: &Envelope) -> bool {
    if envelope.session_id != active_session {
        return false;
    }
    match &envelope.event {
        ProtocolEvent::TextDelta { .. } | ProtocolEvent::ReasoningDelta { .. } => true,
        ProtocolEvent::ReasoningCompleted => false,
        _ => false,
    }
}

fn should_coalesce_terminal_redraw(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse)
            if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
    )
}

fn schedule_deferred_redraw(timer: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    if timer.is_none() {
        *timer = Some(Box::pin(tokio::time::sleep(DEFERRED_REDRAW_INTERVAL)));
    }
}

async fn handle_terminal_event(app: &mut App, event: Event) -> Result<EventOutcome> {
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
async fn refresh_provider_settings(app: &mut App) -> Result<()> {
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
async fn load_provider_models(app: &mut App, refresh: bool) -> Result<()> {
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
fn spawn_model_refresh(app: &mut App) {
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

fn apply_model_refresh_result(app: &mut App, refresh: ModelRefreshResult) {
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

async fn handle_provider_mouse(
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

fn provider_menu_selection(app: &App, rect: Rect, column: u16, row: u16) -> Option<ProviderPreset> {
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(column, row, inner) {
        return None;
    }
    let index = row.saturating_sub(inner.y) as usize;
    provider_choices(app).get(index).copied()
}

fn provider_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter
    )
}

async fn handle_provider_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
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

async fn handle_model_mouse(
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

fn model_menu_selection(app: &App, rect: Rect, column: u16, row: u16) -> Option<String> {
    let inner = ratatui::widgets::Block::bordered().inner(rect);
    if !point_in_rect(column, row, inner) {
        return None;
    }
    let index = row.saturating_sub(inner.y) as usize;
    model_choices(app)
        .get(index)
        .map(|choice| choice.id.clone())
}

fn model_menu_key_handled(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('r')
    )
}

async fn handle_model_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
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

fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

fn thinking_menu_selection(
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

async fn apply_thinking_selection(
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

async fn handle_navigation_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
) -> Result<Option<EventOutcome>> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || app.settings.is_some()
        || app.palette.is_some()
        || app.has_pending_approval()
    {
        return Ok(None);
    }
    if let Some(area) = app.session_panel_rect {
        let rows = ui::flatten_session_tree(&app.sessions, &app.expanded_sessions);
        let current = rows
            .iter()
            .position(|row| row.id == app.current.session_id)
            .unwrap_or(0);
        if let Some(index) =
            ui::session_index_at(area, mouse.column, mouse.row, rows.len(), current)
        {
            let row = &rows[index];
            if row.has_children {
                if !app.expanded_sessions.insert(row.id.clone()) {
                    app.expanded_sessions.remove(&row.id);
                }
                return Ok(Some(EventOutcome::redraw()));
            }
            if row.id == app.current.session_id {
                return Ok(Some(EventOutcome::redraw()));
            }
            app.activate_session(&row.id).await?;
            return Ok(Some(EventOutcome::redraw()));
        }
    }
    if let Some(rect) = app.input_mode_rect
        && point_in_rect(mouse.column, mouse.row, rect)
    {
        if app.current.busy {
            app.current.status = "请求运行中，无法切换模式".into();
            return Ok(Some(EventOutcome::redraw()));
        }
        app.switch_mode(next_mode(app.current.mode)).await?;
        return Ok(Some(EventOutcome::redraw()));
    }
    Ok(None)
}

fn todo_interaction_at(app: &App, column: u16, row: u16) -> Option<InteractionTarget> {
    let rect = app.todo_window_rect?;
    if row == rect.y
        && let Some((toggle_column, close_column)) = ui::todo_control_columns(rect)
    {
        if column == toggle_column {
            return Some(InteractionTarget::TodoToggle);
        }
        if column == close_column {
            return Some(InteractionTarget::TodoClose);
        }
    }
    if row <= rect.y || row >= rect.bottom() || column != rect.x + 1 {
        return None;
    }
    let content_row = usize::from(row - rect.y - 1);
    let visible_rows = usize::from(rect.height.saturating_sub(2));
    let index = ui::todo_task_index_at_row(
        &app.current.todos,
        visible_rows,
        content_row,
        app.current.todo_collapsed,
    )?;
    app.current
        .todos
        .get(index)
        .map(|task| InteractionTarget::Todo(task.id.clone()))
}

fn handle_output_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> EventOutcome {
    match mouse.kind {
        MouseEventKind::ScrollUp => EventOutcome {
            redraw: app.current.scroll_messages(MOUSE_WHEEL_SCROLL_LINES),
            osc52: None,
        },
        MouseEventKind::ScrollDown => EventOutcome {
            redraw: app.current.scroll_messages(-MOUSE_WHEEL_SCROLL_LINES),
            osc52: None,
        },
        MouseEventKind::Down(MouseButton::Left) => {
            app.mouse_dragged = false;
            app.mouse_press_position = Some((mouse.column, mouse.row));
            app.mouse_press_target =
                todo_interaction_at(app, mouse.column, mouse.row).or_else(|| {
                    app.current
                        .message_layout
                        .as_ref()
                        .and_then(|layout| layout.interaction_at(mouse.column, mouse.row))
                });
            if app.mouse_press_target.is_some() {
                app.current.clear_output_selection();
                app.current.edge_scroll = EdgeScroll::default();
                return EventOutcome::redraw();
            }
            let Some(offset) = app
                .current
                .message_layout
                .as_ref()
                .and_then(|layout| layout.hit_test(mouse.column, mouse.row))
            else {
                app.current.clear_output_selection();
                return EventOutcome::redraw();
            };
            if app.current.follow_output {
                app.current.output_scroll_top = app
                    .current
                    .message_layout
                    .as_ref()
                    .map(|layout| layout.scroll);
                if let Some(layout) = &app.current.message_layout {
                    app.current.message_scroll = layout.max_scroll().saturating_sub(layout.scroll);
                }
            }
            app.current.follow_output = false;
            app.current.output_selection = Some(OutputSelection::new(offset));
            update_edge_scroll(app, mouse.column, mouse.row);
            EventOutcome::redraw()
        }
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
            if app.mouse_press_target.is_some() {
                let moved = matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left))
                    || app.mouse_press_position.is_some_and(|(column, row)| {
                        column.abs_diff(mouse.column) > 1 || row.abs_diff(mouse.row) > 1
                    });
                if moved {
                    app.mouse_dragged = true;
                    app.mouse_press_target = None;
                    if let Some((column, row)) = app.mouse_press_position
                        && let Some(offset) = app
                            .current
                            .message_layout
                            .as_ref()
                            .and_then(|layout| layout.hit_test(column, row))
                    {
                        app.current.output_selection = Some(OutputSelection::new(offset));
                    }
                    update_drag_position(app, mouse.column, mouse.row);
                    return EventOutcome::redraw();
                }
                return EventOutcome::default();
            }
            if app
                .current
                .output_selection
                .is_some_and(|selection| selection.dragging)
            {
                update_drag_position(app, mouse.column, mouse.row);
                EventOutcome::redraw()
            } else {
                EventOutcome::default()
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.current.edge_scroll = EdgeScroll::default();
            let pressed_target = app.mouse_press_target.take();
            app.mouse_press_position = None;
            if let Some(target) = pressed_target {
                let released_target =
                    todo_interaction_at(app, mouse.column, mouse.row).or_else(|| {
                        app.current
                            .message_layout
                            .as_ref()
                            .and_then(|layout| layout.interaction_at(mouse.column, mouse.row))
                    });
                if !app.mouse_dragged && released_target.as_ref() == Some(&target) {
                    let todo_target = matches!(
                        &target,
                        InteractionTarget::Todo(_)
                            | InteractionTarget::TodoToggle
                            | InteractionTarget::TodoClose
                    );
                    if !app.current.follow_output && !todo_target {
                        app.layout_restore_anchor =
                            app.current.message_layout.as_ref().and_then(|layout| {
                                layout
                                    .visual_lines
                                    .iter()
                                    .position(|line| line.interaction.as_ref() == Some(&target))
                                    .map(|visual_row| {
                                        (target.clone(), visual_row.saturating_sub(layout.scroll))
                                    })
                            });
                    }
                    let live_thinking_target = matches!(&target, InteractionTarget::Thinking);
                    match target {
                        InteractionTarget::Tool(call_id) => {
                            if !app.current.expanded_tools.insert(call_id.clone()) {
                                app.current.expanded_tools.remove(&call_id);
                            }
                        }
                        InteractionTarget::Thinking => {
                            app.current.thinking_expanded = !app.current.thinking_expanded;
                        }
                        InteractionTarget::ThinkingSummary(id) => {
                            if !app.current.expanded_thinking.insert(id.clone()) {
                                app.current.expanded_thinking.remove(&id);
                            }
                        }
                        InteractionTarget::Todo(task_id) => {
                            let tasks = app.current.todos.clone();
                            if let Some(index) = tasks.iter().position(|task| task.id == task_id)
                                && let Some(task) = tasks.get(index)
                            {
                                let command = match task.status.next() {
                                    TodoStatus::Pending => "/todo undo",
                                    TodoStatus::InProgress => "/todo doing",
                                    TodoStatus::Done => "/todo done",
                                };
                                let text = format!("{command} {}", index + 1);
                                let session = app.current.session_id.clone();
                                let handle = app.handle.clone();
                                tokio::spawn(async move {
                                    let _ = handle.execute_command(Some(session), &text).await;
                                });
                            }
                        }
                        InteractionTarget::TodoToggle => {
                            app.current.todo_collapsed = !app.current.todo_collapsed;
                        }
                        InteractionTarget::TodoClose => {
                            app.current.todo_hidden = true;
                            app.todo_window_rect = None;
                        }
                    }
                    if !live_thinking_target && !todo_target {
                        app.current.invalidate_output_layout();
                    }
                }
                app.mouse_dragged = false;
                return EventOutcome::redraw();
            }
            app.mouse_dragged = false;
            let Some(mut selection) = app.current.output_selection else {
                return EventOutcome::default();
            };
            selection.dragging = false;
            let Some((start, end)) = selection.range() else {
                app.current.output_selection = None;
                return EventOutcome::redraw();
            };
            app.current.output_selection = Some(selection);
            let Some(text) = app
                .current
                .message_layout
                .as_ref()
                .and_then(|layout| layout.text.get(start..end))
                .map(str::to_owned)
            else {
                app.current.status = "复制失败：选区位置已失效".into();
                return EventOutcome::redraw();
            };
            match crate::clipboard::copy_text(&text) {
                crate::clipboard::CopyResult::Native => {
                    app.current.status = "系统剪贴板已复制".into();
                    EventOutcome::redraw()
                }
                crate::clipboard::CopyResult::Osc52Requested(sequence) => {
                    app.current.status = "已向终端发送复制请求".into();
                    EventOutcome {
                        redraw: true,
                        osc52: Some(sequence),
                    }
                }
                crate::clipboard::CopyResult::Error(error) => {
                    app.current.status = format!("复制失败：{error}");
                    EventOutcome::redraw()
                }
            }
        }
        _ => EventOutcome::default(),
    }
}

fn update_drag_position(app: &mut App, column: u16, row: u16) {
    update_edge_scroll(app, column, row);
    let Some(offset) = app.current.message_layout.as_ref().and_then(|layout| {
        let clamped_row = row
            .max(layout.viewport.y)
            .min(layout.viewport.bottom().saturating_sub(1));
        layout.hit_test(column, clamped_row)
    }) else {
        return;
    };
    if let Some(selection) = &mut app.current.output_selection {
        selection.active = offset;
    }
}

fn update_edge_scroll(app: &mut App, column: u16, row: u16) {
    let Some(layout) = &app.current.message_layout else {
        return;
    };
    let direction = edge_scroll_direction(row, layout.viewport);
    app.current.edge_scroll = EdgeScroll { direction, column };
}

fn auto_scroll_selection(app: &mut App) {
    let direction = app.current.edge_scroll.direction;
    if direction == 0
        || !app
            .current
            .output_selection
            .is_some_and(|selection| selection.dragging)
    {
        return;
    }
    let _ = app
        .current
        .scroll_messages(if direction < 0 { 1 } else { -1 });
    let Some(layout) = &app.current.message_layout else {
        return;
    };
    let scroll = app.current.output_scroll_top.unwrap_or(layout.scroll);
    let row = if direction < 0 {
        scroll
    } else {
        scroll.saturating_add(layout.viewport.height.saturating_sub(1) as usize)
    };
    let column = relative_output_column(app.current.edge_scroll.column, layout.viewport);
    if let Some(offset) = layout.position_at_visual_row(row, column)
        && let Some(selection) = &mut app.current.output_selection
    {
        selection.active = offset;
    }
}

fn output_mouse_event_allowed(
    kind: MouseEventKind,
    settings_open: bool,
    palette_open: bool,
    approval_open: bool,
) -> bool {
    !settings_open
        && !palette_open
        && !approval_open
        && matches!(
            kind,
            MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
                | MouseEventKind::Moved
        )
}

fn relative_output_column(column: u16, viewport: ratatui::layout::Rect) -> usize {
    column.saturating_sub(viewport.x) as usize
}

const EDGE_SCROLL_ROWS: u16 = 1;

fn edge_scroll_direction(row: u16, viewport: ratatui::layout::Rect) -> i8 {
    if viewport.height == 0 {
        return 0;
    }
    let top_edge = viewport
        .y
        .saturating_add(EDGE_SCROLL_ROWS.saturating_sub(1));
    let bottom_edge = viewport.bottom().saturating_sub(EDGE_SCROLL_ROWS);
    if row <= top_edge {
        -1
    } else if row >= bottom_edge {
        1
    } else {
        0
    }
}

impl App {
    fn cancel_model_refresh(&mut self) {
        self.model_refresh_generation = self.model_refresh_generation.wrapping_add(1);
        if let Some(task) = self.model_refresh_task.take() {
            task.abort();
        }
        self.provider_models.loading = false;
    }

    /// Core-authoritative active preset, falling back to the local config
    /// snapshot before the first `provider_settings()` read.
    pub(crate) fn active_preset(&self) -> ProviderPreset {
        self.provider_settings
            .as_ref()
            .and_then(|settings| ProviderPreset::parse(&settings.active.preset))
            .unwrap_or(self.config.provider.preset)
    }

    /// Core-authoritative active model id.
    pub(crate) fn active_model(&self) -> &str {
        self.provider_settings
            .as_ref()
            .map(|settings| settings.active.model.as_str())
            .filter(|model| !model.is_empty())
            .unwrap_or(&self.config.provider.model)
    }

    pub(crate) fn provider_label(&self) -> &'static str {
        self.active_preset().label()
    }

    pub(crate) fn model_name(&self) -> &str {
        self.active_model()
    }

    pub(crate) fn thinking_level(&self) -> ThinkingLevel {
        self.config.provider.thinking_level
    }

    pub(crate) fn thinking_budget_tokens(&self) -> Option<u32> {
        self.config.provider.thinking_budget_tokens
    }

    pub(crate) fn thinking_profile(&self) -> ThinkingProfile {
        thinking_profile(self.config.provider.preset, &self.config.provider.model)
    }

    pub(crate) fn has_pending_approval(&self) -> bool {
        self.approval.is_some()
    }

    pub(crate) fn pending_approval(&self) -> Option<&ApprovalDisplay> {
        self.approval.as_ref()
    }

    /// True when the given session owns the approval currently being
    /// displayed (the global oldest). Matches the old "global oldest"
    /// semantics and lets the session panel mark the source session.
    pub(crate) fn session_waiting_approval(&self, session_id: &str) -> bool {
        self.approval
            .as_ref()
            .is_some_and(|approval| approval.source_session_id.as_deref() == Some(session_id))
    }

    async fn cancel(&mut self) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if !session_id.is_empty() {
            let request_seq = self.request_ids.get(&session_id).copied();
            self.handle.cancel(&session_id, request_seq).await?;
        }
        self.current.reset_thinking_state();
        self.current.busy = false;
        self.current.agent_phase = AgentPhase::Idle;
        self.current.model_phase = ModelPhase::Idle;
        self.current.status = "已取消当前请求".into();
        self.current.push_entry(DisplayEntry {
            kind: DisplayKind::System,
            content: DisplayContent::Markdown("当前请求已取消。".into()),
        });
        self.approval = None;
        self.force_full_redraw = true;
        Ok(())
    }

    async fn submit_current(&mut self) -> Result<()> {
        // Custom commands expand to a prompt, which may itself expand again;
        // loop instead of recursing so the future stays a fixed size.
        loop {
            let input = self.input.as_str().trim().to_owned();
            if input.is_empty() {
                return Ok(());
            }
            self.input.push_history();
            if let Some(command) = input
                .strip_prefix('!')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                self.input.clear();
                return self.request_shell_approval(command.to_owned()).await;
            }
            if input.starts_with('/') {
                if let Some(command) = commands::parse(&input) {
                    self.input.clear();
                    return self.execute_command(command).await;
                }
                if let Some(prompt) = expand_custom_command(self, &input) {
                    self.input.set(prompt);
                    continue;
                }
                self.input.clear();
                self.current.push_entry(DisplayEntry {
                    kind: DisplayKind::Error,
                    content: DisplayContent::Markdown(format!(
                        "未知命令，请使用 /help 查看命令：{input}"
                    )),
                });
                return Ok(());
            }
            let session_id = self.current.session_id.clone();
            // Do not clear the input until the core has accepted the message:
            // on a busy session, missing API key or rejected command the user
            // keeps their text.
            self.file_suggestions.clear();
            match self.handle.submit(Some(session_id.clone()), &input).await {
                Ok(request_id) => {
                    self.request_ids.insert(session_id, request_id);
                }
                Err(error) => {
                    self.current.status = secrets::redact(&error.message);
                    return Ok(());
                }
            }
            self.input.clear();
            self.sync_all().await?;
            return Ok(());
        }
    }

    async fn request_shell_approval(&mut self, command: String) -> Result<()> {
        if !self.current.session_id.is_empty() {
            let text = format!("!{command}");
            self.handle
                .submit(Some(self.current.session_id.clone()), &text)
                .await?;
        }
        Ok(())
    }

    async fn create_session(&mut self) -> Result<()> {
        self.handle.execute_command(None, "/new").await?;
        self.sync_all().await?;
        self.input.clear();
        Ok(())
    }

    async fn switch_session_direction(&mut self, direction: isize) -> Result<()> {
        let rows = ui::flatten_session_tree(&self.sessions, &self.expanded_sessions);
        if rows.is_empty() {
            return Ok(());
        }
        let current = rows
            .iter()
            .position(|row| row.id == self.current.session_id)
            .unwrap_or(0);
        let next = if direction > 0 {
            (current + 1) % rows.len()
        } else {
            (current + rows.len() - 1) % rows.len()
        };
        let target = rows[next].id.clone();
        if target != self.current.session_id {
            self.activate_session(&target).await?;
        }
        Ok(())
    }

    async fn activate_session(&mut self, session_id: &str) -> Result<()> {
        if session_id == self.current.session_id && !self.current.session_id.is_empty() {
            self.sync_all().await?;
            return Ok(());
        }
        self.handle.activate_session(session_id).await?;
        self.sync_all().await?;
        Ok(())
    }

    async fn switch_mode(&mut self, mode: AgentMode) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if session_id.is_empty() {
            return Ok(());
        }
        let command = match mode {
            AgentMode::Build => "/build",
            AgentMode::Plan => "/plan",
            AgentMode::Explore => "/explore",
            AgentMode::Cluster => "/cluster",
        };
        self.handle
            .execute_command(Some(session_id), command)
            .await?;
        self.current.mode = mode;
        self.current.status = format!("模式已切换为 {}", mode.as_str().to_ascii_uppercase());
        self.sync_all().await?;
        Ok(())
    }

    async fn resolve_approval(&mut self, choice: ApprovalChoice) -> Result<()> {
        let Some(approval) = self.approval.clone() else {
            return Ok(());
        };
        let (accept, allow_session) = match choice {
            ApprovalChoice::Approve => (true, false),
            ApprovalChoice::Reject => (false, false),
            ApprovalChoice::AlwaysSession => (true, true),
        };
        self.approval = None;
        self.current.pending_approval = None;
        self.force_full_redraw = true;
        self.handle
            .approve(&approval.approval_id, accept, allow_session)
            .await?;
        Ok(())
    }

    async fn apply_provider_choice(&mut self, preset: ProviderPreset) -> Result<()> {
        if preset == self.active_preset() {
            return Ok(());
        }
        self.cancel_model_refresh();
        // Prefer the target preset's saved model; otherwise use its template
        // default. Carrying the old provider's model across would pair an
        // unrelated model id with the new provider.
        let model = self
            .provider_settings
            .as_ref()
            .and_then(|settings| {
                settings
                    .saved
                    .iter()
                    .find(|profile| profile.preset == preset.key_id())
                    .map(|profile| profile.model.clone())
            })
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| preset.defaults().model);
        if let Err(error) = self.handle.set_provider(preset.key_id(), &model).await {
            self.current.status = secrets::redact(&error.message);
            return Ok(());
        }
        // The active preset changed: drop the previous provider's dynamic model
        // cache before reading the new one.
        self.provider_models = ProviderModelsState::default();
        self.sync_all().await?;
        let _ = refresh_provider_settings(self).await;
        let _ = load_provider_models(self, false).await;
        Ok(())
    }

    async fn apply_model_choice(&mut self, model: String) -> Result<()> {
        let model = model.trim().to_owned();
        if model.is_empty() {
            return Ok(());
        }
        self.cancel_model_refresh();
        let preset = self.active_preset();
        if let Err(error) = self.handle.set_provider(preset.key_id(), &model).await {
            self.current.status = secrets::redact(&error.message);
            return Ok(());
        }
        self.sync_all().await?;
        let _ = refresh_provider_settings(self).await;
        let _ = load_provider_models(self, false).await;
        Ok(())
    }
}

/// Applies one live envelope with cursor dedup: an envelope at or below the
/// facade's last-processed cursor is skipped (replay/live overlap must
/// deduplicate by cursor), a fresh one is handled and the cursor advances.
/// Returns `Some(redraw)` when the envelope was accepted, `None` when its
/// cursor was stale or duplicate.
fn accept_envelope(app: &mut App, envelope: &Envelope) -> Option<bool> {
    if envelope.cursor <= app.event_cursor {
        return None;
    }
    let redraw = handle_envelope(app, envelope);
    app.event_cursor = envelope.cursor;
    Some(redraw)
}

fn handle_envelope(app: &mut App, envelope: &Envelope) -> bool {
    let approval_redraw = match &envelope.event {
        ProtocolEvent::Approval {
            approval_id,
            call,
            reason,
            source_title,
            ..
        } => {
            // The modal reads the facade's global oldest approval, while the
            // projection only owns active-session display state. Populate the
            // global slot directly from the live event so an agent cannot wait
            // invisibly until the server-side approval timeout. A newer event
            // must not replace an older approval already being shown.
            if app.approval.is_none() {
                app.approval = Some(ApprovalDisplay {
                    approval_id: approval_id.clone(),
                    call: call.clone(),
                    reason: reason.clone(),
                    source_session_id: Some(envelope.session_id.clone()),
                    source_title: source_title.clone(),
                    created_at: std::time::Instant::now(),
                });
            }
            true
        }
        ProtocolEvent::ApprovalResolved { approval_id, .. } => {
            if app
                .approval
                .as_ref()
                .is_some_and(|approval| approval.approval_id == *approval_id)
            {
                app.approval = None;
            }
            // The resolved approval may expose another pending approval behind
            // it. Refresh the authoritative global-oldest slot without moving
            // the live event cursor (sync_snapshot preserves it).
            app.sync_pending = true;
            true
        }
        _ => false,
    };
    match &envelope.event {
        ProtocolEvent::SessionsChanged | ProtocolEvent::ResyncRequired => {
            app.sync_pending = true;
            true
        }
        ProtocolEvent::ChildSessionProgress {
            child_session_id,
            status,
            turn,
            max_turns,
            tool,
        } => {
            app.child_status.insert(
                child_session_id.clone(),
                ChildSessionProgress {
                    status: child_status_from_wire(status),
                    turn: *turn,
                    max_turns: *max_turns,
                    tool: tool.clone(),
                    updated_at: std::time::Instant::now(),
                },
            );
            true
        }
        _ => {
            if envelope.session_id == app.current.session_id && !app.current.session_id.is_empty() {
                let outcome = app.current.handle_event(&envelope.event);
                if outcome.sessions_dirty || outcome.transcript_dirty {
                    app.sync_pending = true;
                }
                approval_redraw
                    || outcome.force_redraw
                    || outcome.transcript_dirty
                    || outcome.sessions_dirty
            } else {
                // Background session events only affect the session list.
                app.sync_pending = true;
                true
            }
        }
    }
}

impl App {
    /// Merges snapshot state into the facade. `advance_cursor` is only true
    /// when the caller is establishing a new event-stream baseline; a normal
    /// refresh while the live stream is active must not skip envelopes that
    /// have reached the bridge but have not yet been rendered.
    fn merge_snapshot(&mut self, snapshot: &AppSnapshotV2, advance_cursor: bool) {
        if advance_cursor {
            self.event_cursor = snapshot.event_cursor;
        }
        if let Some(session_id) = &snapshot.active_session {
            if !self.current.session_id.is_empty() && self.current.session_id != *session_id {
                // The active session changed: rebuild the projection shell.
                self.current = TuiSessionProjection::new(
                    session_id.clone(),
                    AgentMode::parse(&snapshot.mode).unwrap_or_default(),
                    snapshot
                        .context
                        .as_ref()
                        .and_then(|context| context.context_window_tokens),
                );
            }
            self.active_session = session_id.clone();
            if self.current.session_id.is_empty() {
                self.current.session_id = session_id.clone();
            }
        }
        if let Some(session) = snapshot
            .sessions
            .iter()
            .find(|session| session.id == self.current.session_id)
        {
            self.current.title = session.title.clone();
            self.current.parent_id = session.parent_id.clone();
            self.current.status = session.status.clone();
            self.current.busy = session.busy;
            if let Ok(phase) = parse_phase(&session.phase) {
                self.current.agent_phase = phase;
            }
        }
        self.current.mode = AgentMode::parse(&snapshot.mode).unwrap_or(self.current.mode);
        // The snapshot budget is authoritative; clear stale local context
        // state when the core reports no budget for this session.
        self.current.context_budget = snapshot.context.clone();
        self.current.context_limit_tokens = snapshot
            .context
            .as_ref()
            .and_then(|context| context.context_window_tokens);
        self.current.context_used_tokens = snapshot
            .context
            .as_ref()
            .map_or(0, |context| context.used_tokens);
        self.current.context_overlay_tokens = 0;
        // Render a persisted incomplete answer (surviving a restart) once, and
        // drop it once the core clears the partial (normal completion).
        self.current
            .sync_partial(snapshot.assistant_partial.as_ref());
        self.sessions = snapshot.sessions.iter().map(session_summary).collect();
        self.approval = snapshot.approval.as_ref().map(approval_display);
        self.current.todos = snapshot
            .todos
            .iter()
            .map(|todo| TodoTask {
                id: todo.id.clone(),
                title: todo.title.clone(),
                status: todo_status_from_wire(&todo.status),
                created_at: todo.created_at.clone(),
                updated_at: todo.updated_at.clone(),
            })
            .collect();
        self.current.todo_collapsed = !self.current.todos.is_empty()
            && self
                .current
                .todos
                .iter()
                .all(|task| task.status == TodoStatus::Done);
        self.config.provider.model = snapshot.model.clone();
        if let Some(preset) = ProviderPreset::ALL
            .iter()
            .copied()
            .find(|preset| preset.label() == snapshot.provider)
        {
            self.config.provider.preset = preset;
        }
        if let Some(provider) = self
            .config
            .providers
            .iter_mut()
            .find(|provider| provider.preset.label() == snapshot.provider)
        {
            provider.model = snapshot.model.clone();
            self.config.provider = provider.clone();
        }
        if let Some(settings) = &mut self.provider_settings {
            settings.active.model = snapshot.model.clone();
            if let Some(preset) = ProviderPreset::ALL
                .iter()
                .copied()
                .find(|preset| preset.label() == snapshot.provider)
            {
                settings.active.preset = preset.key_id().to_owned();
            }
        }
    }

    pub(crate) fn sync_from_snapshot(&mut self, snapshot: &AppSnapshotV2) {
        self.merge_snapshot(snapshot, true);
    }

    /// Refetches the snapshot and merges it into the facade. Never touches
    /// the live history; call [`App::load_history`] separately when the
    /// transcript changed.
    pub(crate) async fn sync_snapshot(&mut self) -> Result<()> {
        let snapshot = self.handle.snapshot().await?;
        self.merge_snapshot(&snapshot, false);
        Ok(())
    }

    /// Refetches state after the event cursor has been evicted or a broadcast
    /// receiver has lagged. The snapshot becomes the new replay baseline.
    pub(crate) async fn resync_all(&mut self) -> Result<()> {
        let snapshot = self.handle.snapshot().await?;
        self.merge_snapshot(&snapshot, true);
        self.load_history().await
    }

    /// Refetches the newest message page for the active session and replaces
    /// the projection history from the database. The page's `has_more` /
    /// `next_before` cursors drive subsequent [`Self::load_older_history`]
    /// calls, so history is no longer capped at a fixed "last 100" snapshot.
    pub(crate) async fn load_history(&mut self) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if session_id.is_empty() {
            return Ok(());
        }
        let page = self.handle.messages(&session_id, None, None).await?;
        self.current.apply_message_page(&page, false);
        Ok(())
    }

    /// Loads the next older message page and prepends it, anchoring the scroll
    /// so the visible content stays put. No-op when the head is fully loaded.
    pub(crate) async fn load_older_history(&mut self) -> Result<()> {
        let session_id = self.current.session_id.clone();
        if session_id.is_empty() || !self.current.history_has_more {
            return Ok(());
        }
        let Some(before) = self.current.history_next_before else {
            return Ok(());
        };
        let page = self
            .handle
            .messages(&session_id, Some(before), None)
            .await?;
        self.current.apply_message_page(&page, true);
        Ok(())
    }

    /// Snapshot merge plus history refetch. Used after any mutation that may
    /// have changed the active session's transcript.
    pub(crate) async fn sync_all(&mut self) -> Result<()> {
        self.sync_snapshot().await?;
        self.load_history().await?;
        Ok(())
    }

    async fn execute_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Help => {
                self.current.push_entry(DisplayEntry {
                    kind: DisplayKind::System,
                    content: DisplayContent::Markdown(
                        "## 命令\n\n`/new` `/rename` `/fork` `/delete`\n`/undo` `/redo` `/compact` `/uncompact` `/export [路径]` `/todo [add|doing|done|undo|edit|remove|clear]` `/memory [search|add|candidate|confirm|edit|delete]` `/diff`\n`/plan` `/build` `/explore` `/cluster` `/model` `/provider` `/agent`\n\nCtrl+P 或 Ctrl+X 打开命令面板 | @ 文件 | ! Shell\n\n搜索后端（DuckDuckGo/Bing）来自 config 的 [runtime].search_backend，TUI 不另存配置"
                            .into(),
                    ),
                });
                self.current.status = "命令帮助".into();
                Ok(())
            }
            Command::Provider => {
                open_settings(self).await;
                Ok(())
            }
            Command::Clear => {
                self.current.invalidate_output_layout();
                self.current.entries.clear();
                self.current.reset_thinking_state();
                self.current.clear_output_selection();
                self.current.status = "显示已清空，会话历史仍保留".into();
                Ok(())
            }
            Command::Quit => {
                self.should_quit = true;
                Ok(())
            }
            Command::Rename(None) => {
                self.input.set("/rename ");
                self.current.status = "请输入新会话名称：/rename <名称>".into();
                Ok(())
            }
            other => {
                let session_id = self.current.session_id.clone();
                if !session_id.is_empty() {
                    self.handle
                        .execute_command(Some(session_id), &command_to_text(&other))
                        .await?;
                }
                self.sync_all().await?;
                Ok(())
            }
        }
    }
}

fn command_to_text(command: &Command) -> String {
    match command {
        Command::NewSession => "/new".into(),
        Command::Rename(Some(title)) => format!("/rename {title}"),
        Command::Rename(None) => "/rename".into(),
        Command::Delete => "/delete".into(),
        Command::Fork => "/fork".into(),
        Command::Undo => "/undo".into(),
        Command::Redo => "/redo".into(),
        Command::Compact(Some(ms)) => format!("/compact {ms}"),
        Command::Compact(None) => "/compact".into(),
        Command::Uncompact => "/uncompact".into(),
        Command::Export(Some(path)) => format!("/export {path}"),
        Command::Export(None) => "/export".into(),
        Command::Diff => "/diff".into(),
        Command::Model(Some(model)) => format!("/model {model}"),
        Command::Model(None) => "/model".into(),
        Command::Agent(Some(agent)) => format!("/agent {agent}"),
        Command::Agent(None) => "/agent".into(),
        Command::Memory(argument) => argument
            .as_deref()
            .map(|argument| format!("/memory {argument}"))
            .unwrap_or_else(|| "/memory".into()),
        Command::Mode(mode) => format!("/{}", mode.as_str()),
        Command::Todo(todo) => format!("/todo {}", todo_to_text(todo)),
        Command::Help | Command::Provider | Command::Clear | Command::Quit => unreachable!(),
    }
}

fn todo_to_text(todo: &TodoCommand) -> String {
    match todo {
        TodoCommand::Show => String::new(),
        TodoCommand::Add(text) => format!("add {text}"),
        TodoCommand::Doing(index) => format!("doing {index}"),
        TodoCommand::Done(index) => format!("done {index}"),
        TodoCommand::Undo(index) => format!("undo {index}"),
        TodoCommand::Edit(index, text) => format!("edit {index} {text}"),
        TodoCommand::Remove(index) => format!("remove {index}"),
        TodoCommand::Clear => "clear".into(),
    }
}

fn child_status_from_wire(status: &str) -> protium_core::agent::ChildSessionStatus {
    match status {
        "completed" => protium_core::agent::ChildSessionStatus::Completed,
        "failed" => protium_core::agent::ChildSessionStatus::Failed,
        "turn_limit" => protium_core::agent::ChildSessionStatus::TurnLimit,
        "timed_out" => protium_core::agent::ChildSessionStatus::TimedOut,
        "cancelled" => protium_core::agent::ChildSessionStatus::Cancelled,
        _ => protium_core::agent::ChildSessionStatus::Queued,
    }
}

fn approval_display(approval: &ApprovalDto) -> ApprovalDisplay {
    ApprovalDisplay {
        approval_id: approval.approval_id.clone(),
        call: approval.call.clone(),
        reason: approval.reason.clone(),
        source_session_id: Some(approval.session_id.clone()),
        source_title: approval.source_title.clone(),
        created_at: std::time::Instant::now(),
    }
}

fn expand_custom_command(app: &App, input: &str) -> Option<String> {
    let mut parts = input[1..].trim().splitn(2, char::is_whitespace);
    let name = parts.next()?;
    let arguments = parts.next().unwrap_or("").trim();
    let command = app
        .config
        .commands
        .iter()
        .find(|command| command.name == name)?;
    if command.template.trim().is_empty() {
        return None;
    }
    Some(
        command
            .template
            .replace("{args}", arguments)
            .replace("{workspace}", &app.workspace.display().to_string()),
    )
}

fn update_file_suggestions(app: &mut App) {
    app.file_suggestions.clear();
    app.file_selected = 0;
    let token = app.input.as_str().split_whitespace().last().unwrap_or("");
    let Some(query) = token.strip_prefix('@') else {
        return;
    };
    let mut candidates = WalkBuilder::new(app.workspace_security.root())
        .hidden(false)
        .standard_filters(true)
        .max_depth(Some(5))
        .build()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry
                .path()
                .strip_prefix(app.workspace_security.root())
                .ok()?;
            if path.as_os_str().is_empty() || path == std::path::Path::new(".git") {
                return None;
            }
            let value = path.to_string_lossy().replace('\\', "/");
            let score = commands::fuzzy_score(query, &value)?;
            Some((score, value))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(score, value)| (*score, value.len()));
    app.file_suggestions = candidates
        .into_iter()
        .take(10)
        .map(|(_, value)| value)
        .collect();
}

fn apply_file_completion(app: &mut App) {
    let Some(path) = app.file_suggestions.get(app.file_selected).cloned() else {
        return;
    };
    let input = app.input.as_str().to_owned();
    let start = input
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    if !input[start..].starts_with('@') {
        return;
    }
    app.input.set(format!("{}@{} ", &input[..start], path));
    app.file_suggestions.clear();
}

async fn open_settings(app: &mut App) {
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
    // explicitly saved (for example the built-in default provider).
    if !providers
        .iter()
        .any(|provider| provider.preset.key_id() == settings.active.preset)
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
    let mut config = if preset == app.config.provider.preset {
        app.config.provider.clone()
    } else {
        preset.defaults()
    };
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
    // The DTO intentionally omits thinking/retry fields. For the preset the
    // local config still tracks, start from that richer copy so editing the
    // active provider does not silently reset those customizations; the core
    // merge keeps them on apply anyway.
    let mut config = if preset == app.config.provider.preset {
        app.config.provider.clone()
    } else {
        preset.defaults()
    };
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

/// Connected presets from the core settings view. Falls back to the local
/// secret cache only before the first core read (e.g. during tests).
fn available_key_presets(app: &App) -> HashSet<ProviderPreset> {
    if let Some(settings) = &app.provider_settings {
        return settings
            .connected
            .iter()
            .filter_map(|preset| ProviderPreset::parse(preset))
            .collect();
    }
    ProviderPreset::ALL
        .iter()
        .filter_map(|preset| secrets::api_key_cached_only(*preset).ok().map(|_| *preset))
        .collect()
}

fn provider_form(app: &App, provider: crate::config::ProviderConfig) -> SettingsForm {
    let preset = provider.preset;
    let available = available_key_presets(app);
    let existing_key_preset = available.contains(&preset).then_some(preset);
    let mut form = SettingsForm::new(provider, existing_key_preset);
    form.set_available_key_presets(available);
    form
}

/// Builds the settings list with `connected` sourced from the core DTO (the
/// local `SettingsState::list` constructor would re-derive it from the cache).
fn provider_list_state(app: &App) -> SettingsState {
    let providers = provider_configs_from_settings(app);
    let mut state = SettingsState::list(providers, app.active_preset());
    if let SettingsState::List(list) = &mut state
        && let Some(settings) = &app.provider_settings
    {
        list.connected = settings
            .connected
            .iter()
            .filter_map(|preset| ProviderPreset::parse(preset))
            .collect();
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

fn open_template_picker(app: &mut App) {
    if let Some(settings) = &mut app.settings {
        settings.open_templates();
        app.current.status = "选择供应商模板".into();
    }
}

fn open_selected_profile(app: &mut App) {
    if let Some(provider) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_profile)
    {
        open_provider_form(app, provider);
        app.current.status = "编辑供应商".into();
    }
}

fn open_selected_template(app: &mut App) {
    if let Some(preset) = app
        .settings
        .as_ref()
        .and_then(SettingsState::selected_template)
    {
        open_provider_form(app, preset.defaults());
        app.current.status = format!("添加 {}", preset.label());
    }
}

/// Total selectable rows in the settings form: the core `FIELDS` registry plus
/// one TUI-only synthetic context-window override row.
fn settings_row_count() -> usize {
    FIELDS.len() + 1
}

/// TUI row index of the synthetic context-window override. It is rendered
/// immediately after Thinking and before the write-only API key, so it sits at
/// the last core field index rather than at the end of the row list.
fn context_window_row() -> usize {
    FIELDS.len().saturating_sub(1)
}

fn settings_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
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

fn palette_key_handled(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(
        code,
        KeyCode::Esc | KeyCode::Enter | KeyCode::Up | KeyCode::Down | KeyCode::Backspace
    ) || matches!(code, KeyCode::Char(_) if !modifiers.contains(KeyModifiers::CONTROL))
}

fn paste_text_into_settings(app: &mut App, text: &str) -> bool {
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

async fn handle_settings_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
            if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
                let field = form.field();
                form.cycle(field, direction);
            }
        }
        KeyCode::Backspace => {
            if app.settings_field_index == context_window_row() {
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
            if app.settings_field_index == context_window_row() {
                if code == KeyCode::Delete {
                    app.context_window_input.clear();
                }
            } else if let Err(error) = remove_settings_provider(app).await {
                app.current.status = format!("移除失败：{}", secrets::redact(&error.to_string()));
            }
        }
        KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
            if app.settings_field_index == context_window_row() {
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

/// Mirrors the TUI row index onto the core form's `FIELDS` selection. The
/// synthetic context-window row has no core `SettingsField`, so the core
/// selection clamps to the last real field while it is highlighted.
fn sync_form_selection(app: &mut App) {
    // TUI rows: [Preset, Protocol, Model, BaseUrl, Thinking, ContextWindow,
    // ApiKey]. The synthetic row maps onto the last core field so the form's
    // own `field()` never indexes out of bounds; edits for it are intercepted
    // before this mapping is consulted.
    let index = app.settings_field_index.min(FIELDS.len().saturating_sub(1));
    if let Some(form) = app.settings.as_mut().and_then(SettingsState::form_mut) {
        form.selected = index;
    }
}

async fn apply_settings(app: &mut App) -> Result<()> {
    let (preset, model, base_url, kind, entered_key, context_window, form_provider) = {
        let form = app
            .settings
            .as_ref()
            .and_then(SettingsState::form)
            .context("provider editor is not open")?;
        let provider = form.prepare()?;
        let context_window = parse_context_window_override(&app.context_window_input)?;
        (
            provider.preset,
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
    let key_warning = if entered_key.is_empty() {
        None
    } else {
        secrets::store_api_key_cached(preset, &entered_key)
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
    let active = app.config.provider.preset == preset;
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
            preset,
            &model,
            (!base_url.trim().is_empty()).then_some(base_url.as_str()),
            Some(kind),
            context_window,
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
    app.current.status = format!(
        "就绪 | {} | {}{}",
        preset.label(),
        model,
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

fn open_palette(app: &mut App) {
    app.palette = Some(CommandPaletteState {
        query: String::new(),
        selected: 0,
    });
    app.current.status = "命令面板 | 输入筛选 | ↑/↓ 选择 | Enter 执行 | Esc 关闭".into();
}

async fn handle_palette_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let Some(palette) = &mut app.palette else {
        return;
    };
    let results = commands::matches(&palette.query, 10);
    match code {
        KeyCode::Esc => {
            app.palette = None;
            app.current.status = "就绪".into();
        }
        KeyCode::Enter => {
            let selected = results.get(palette.selected).copied();
            let action = selected.map(|item| commands::PALETTE_ITEMS[item.index].action);
            app.palette = None;
            if let Some(action) = action {
                if let Err(error) = execute_palette_action(app, action).await {
                    app.current.status = format!("命令失败：{error}");
                }
            }
        }
        KeyCode::Up => {
            palette.selected = palette.selected.saturating_sub(1);
        }
        KeyCode::Down => {
            palette.selected = (palette.selected + 1).min(results.len().saturating_sub(1));
        }
        KeyCode::Backspace => {
            palette.query.pop();
            palette.selected = 0;
        }
        KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
            palette.query.push(character);
            palette.selected = 0;
        }
        _ => {}
    }
}

async fn execute_palette_action(app: &mut App, action: commands::PaletteAction) -> Result<()> {
    match action {
        commands::PaletteAction::Command(input) => {
            let command = commands::parse(input).context("invalid palette command")?;
            app.execute_command(command).await
        }
        commands::PaletteAction::CycleMode => app.switch_mode(next_mode(app.current.mode)).await,
    }
}

fn next_mode(mode: AgentMode) -> AgentMode {
    match mode {
        AgentMode::Build => AgentMode::Plan,
        AgentMode::Plan => AgentMode::Explore,
        AgentMode::Explore => AgentMode::Cluster,
        AgentMode::Cluster => AgentMode::Build,
    }
}

fn session_switch_direction(key: &crossterm::event::KeyEvent) -> Option<i32> {
    let has_switch_modifier = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
    if !has_switch_modifier {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(-1),
        KeyCode::Down => Some(1),
        _ => None,
    }
}

fn todo_status_from_wire(status: &str) -> TodoStatus {
    match status {
        "in_progress" => TodoStatus::InProgress,
        "done" => TodoStatus::Done,
        _ => TodoStatus::Pending,
    }
}

pub(crate) fn braille_spinner_supported() -> bool {
    const BRAILLE_BLANK: char = '⠀';
    const BRAILLE_SET: [char; 6] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴'];
    let mut width = 0;
    for character in [BRAILLE_BLANK].into_iter().chain(BRAILLE_SET) {
        if let Some(value) = unicode_width::UnicodeWidthChar::width(character) {
            width += value;
        }
    }
    width == BRAILLE_SET.len() + 1
}

pub(crate) fn thinking_animation_glyph(frame: usize, braille: bool) -> &'static str {
    if braille {
        const BRAILLE: [&str; 6] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];
        BRAILLE[frame % BRAILLE.len()]
    } else {
        const ASCII: [&str; 4] = ["-", "\\", "|", "/"];
        ASCII[frame % ASCII.len()]
    }
}

async fn remove_settings_provider(app: &mut App) -> Result<()> {
    let preset = app
        .settings
        .as_ref()
        .and_then(SettingsState::form)
        .map(|form| form.provider.preset)
        .context("provider editor is not open")?;
    app.cancel_model_refresh();
    app.handle.remove_provider(preset).await?;
    // Converge from the core view; never reload config as the authority.
    let _ = refresh_provider_settings(app).await;
    app.provider_models = ProviderModelsState::default();
    let _ = load_provider_models(app, false).await;
    app.sync_all().await?;
    reopen_provider_list(app);
    app.current.status = "供应商已移除；API Key 已保留在系统钥匙串".into();
    Ok(())
}

#[cfg(test)]
mod tests;
