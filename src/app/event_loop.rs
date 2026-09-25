use super::*;

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

pub(super) fn session_summary(session: &protium_core::protocol::SessionStateDto) -> SessionSummary {
    SessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        parent_id: session.parent_id.clone(),
        child_status: session.child_status.clone(),
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

pub(super) async fn build_app(
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
        thinking_menu_geometry: None,
        thinking_menu_cursor: ThinkingMenuCursor::default(),
        session_panel_rect: None,
        input_mode_rect: None,
        provider_control_rect: None,
        model_control_rect: None,
        provider_menu_open: false,
        provider_menu_geometry: None,
        provider_menu_selected: 0,
        model_menu_open: false,
        model_menu_geometry: None,
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

pub(super) fn parse_phase(phase: &str) -> std::result::Result<AgentPhase, ()> {
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
pub(super) fn should_coalesce_stream_redraw(active_session: &str, envelope: &Envelope) -> bool {
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
