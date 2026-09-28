use super::*;
use protium_core::config::Config;
use protium_core::provider::ToolCall;
use ratatui::backend::TestBackend;
use tempfile::tempdir;
use unicode_width::UnicodeWidthStr;

#[test]
fn child_progress_phase_and_old_running_events_are_distinguished_from_queued() {
    use protium_core::agent::ChildSessionStatus as Status;

    assert_eq!(
        child_status_from_wire("running", Some("queued")),
        Status::Queued
    );
    assert_eq!(
        child_status_from_wire("running", Some("waiting_approval")),
        Status::WaitingApproval
    );
    assert_eq!(child_status_from_wire("running", None), Status::Running);
}

async fn test_app() -> (App, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create ws");
    let workspace = workspace.canonicalize().expect("canonicalize");
    let mut config = Config::default();
    config.data_dir = temp.path().join("data");
    let core = CoreConfig {
        workspace: workspace.clone(),
        config: config.clone(),
        data_dir: config.data_dir.clone(),
        event_capacity: 64,
        event_max_bytes: 1024 * 1024,
        approval_timeout: Duration::from_secs(60),
        message_page_size: 20,
    };
    let handle = AppService::start(core).await.expect("start");
    handle.execute_command(None, "/new").await.expect("new");
    let snapshot = handle.snapshot().await.expect("snapshot");
    let app = build_app(handle, snapshot, workspace, config)
        .await
        .expect("build_app");
    (app, temp)
}

#[tokio::test]
async fn model_refresh_channel_is_bounded_to_one_result() {
    let (app, _temp) = test_app().await;
    let provider_id = app.active_provider_id();
    let result = || ModelRefreshResult {
        generation: 1,
        provider_id: provider_id.clone(),
        result: Err("test".to_owned()),
    };

    assert!(app.model_refresh_tx.try_send(result()).is_ok());
    assert!(app.model_refresh_tx.try_send(result()).is_err());
}

#[tokio::test]
async fn stale_model_refresh_does_not_clear_newer_loading_state() {
    let (mut app, _temp) = test_app().await;
    app.model_refresh_generation = 2;
    app.provider_models.loading = true;
    let provider_id = app.active_provider_id();

    apply_model_refresh_result(
        &mut app,
        ModelRefreshResult {
            generation: 1,
            provider_id,
            result: Err("stale".to_owned()),
        },
    );

    assert!(app.provider_models.loading);
}

#[tokio::test]
async fn facade_renders_a_frame_with_test_backend() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let content = terminal.backend().buffer().content().to_vec();
    assert!(!content.is_empty());
    assert!(content.iter().any(|cell| cell.symbol() != " "));
}

/// Replays the core's shared conformance corpus through the facade's
/// cursor-deduped accept path: every envelope must be accepted exactly
/// once, the cursor must land on the last envelope, and replaying the
/// same batch (all stale cursors) must be a no-op.
#[tokio::test]
async fn conformance_corpus_replays_and_dedups_by_cursor() {
    for scenario in protium_core::conformance::scenarios() {
        let (mut app, _temp) = test_app().await;
        app.event_cursor = 0;
        app.current.session_id = protium_core::conformance::SESSION_ID.to_owned();
        for envelope in &scenario.envelopes {
            assert!(
                accept_envelope(&mut app, envelope).is_some(),
                "scenario {}: fresh envelope at cursor {} was skipped",
                scenario.name,
                envelope.cursor
            );
        }
        let last = scenario
            .envelopes
            .last()
            .map(|envelope| envelope.cursor)
            .unwrap_or(0);
        assert_eq!(
            app.event_cursor, last,
            "scenario {}: cursor must land on the last envelope",
            scenario.name
        );
        let status_before = app.current.status.clone();
        for envelope in &scenario.envelopes {
            assert!(
                accept_envelope(&mut app, envelope).is_none(),
                "scenario {}: stale envelope at cursor {} was accepted",
                scenario.name,
                envelope.cursor
            );
        }
        assert_eq!(
            app.event_cursor, last,
            "scenario {}: stale replay must not move the cursor",
            scenario.name
        );
        assert_eq!(
            app.current.status, status_before,
            "scenario {}: stale replay must not touch projection state",
            scenario.name
        );
    }
}

/// The live `Approval` event must populate the facade's global modal slot
/// immediately (an agent must never wait invisibly for the server-side
/// approval timeout).
#[tokio::test]
async fn conformance_approval_pending_populates_the_global_modal() {
    let scenario = protium_core::conformance::scenarios()
        .into_iter()
        .find(|scenario| {
            scenario.expectation == protium_core::conformance::Expectation::ApprovalPending
        })
        .expect("corpus must keep an approval-pending scenario");
    let (mut app, _temp) = test_app().await;
    app.event_cursor = 0;
    app.current.session_id = protium_core::conformance::SESSION_ID.to_owned();
    for envelope in &scenario.envelopes {
        accept_envelope(&mut app, envelope);
    }
    assert!(
        app.approval.is_some(),
        "scenario {}: a live Approval must populate the global modal slot",
        scenario.name
    );
}

#[tokio::test]
async fn live_thinking_streams_reasoning_onto_the_screen() {
    let (mut app, _temp) = test_app().await;
    let session = app.current.session_id.clone();
    let mut cursor = app.event_cursor;
    let mut push = |app: &mut App, event: ProtocolEvent| {
        cursor += 1;
        handle_envelope(
            app,
            &Envelope {
                cursor,
                session_id: session.clone(),
                event,
            },
        );
    };
    push(&mut app, ProtocolEvent::ModelStreaming);
    push(
        &mut app,
        ProtocolEvent::ReasoningDelta {
            delta: "正在检查项目结构".into(),
        },
    );
    push(
        &mut app,
        ProtocolEvent::ReasoningDelta {
            delta: "并确认上下文预算".into(),
        },
    );
    assert!(app.current.thinking_active);
    assert_eq!(
        app.current.thinking_buffer,
        "正在检查项目结构并确认上下文预算"
    );

    let backend = TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let content = terminal.backend().buffer().content().to_vec();
    let text: String = content.iter().map(|cell| cell.symbol()).collect();
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        compact.contains("正在检查项目结构"),
        "live thinking reasoning should be visible on screen; got: {text:?}"
    );
}

#[tokio::test]
async fn tool_call_streaming_is_visible_on_the_screen() {
    let (mut app, _temp) = test_app().await;
    let session = app.current.session_id.clone();
    let mut cursor = app.event_cursor;
    let mut push = |app: &mut App, event: ProtocolEvent| {
        cursor += 1;
        handle_envelope(
            app,
            &Envelope {
                cursor,
                session_id: session.clone(),
                event,
            },
        );
    };
    push(&mut app, ProtocolEvent::ModelStreaming);
    push(
        &mut app,
        ProtocolEvent::ReasoningDelta {
            delta: "推演过程".into(),
        },
    );
    push(&mut app, ProtocolEvent::ReasoningCompleted);
    push(
        &mut app,
        ProtocolEvent::TextDelta {
            delta: "正文".into(),
        },
    );
    push(
        &mut app,
        ProtocolEvent::ToolCallStreaming {
            name: Some("file_write".into()),
            received_bytes: 9216,
        },
    );
    assert_eq!(app.current.agent_phase, AgentPhase::StreamingToolCall);
    assert!(
        app.current.status.contains("文件修改"),
        "status must name the tool; got: {}",
        app.current.status
    );

    let backend = TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let content = terminal.backend().buffer().content().to_vec();
    let text: String = content.iter().map(|cell| cell.symbol()).collect();
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(
        compact.contains("生成工具调用"),
        "the generating-tool row must be visible; got: {text:?}"
    );
    assert!(
        compact.contains("文件修改") && compact.contains("9.0KB"),
        "the row must show the tool name and human-readable bytes; got: {text:?}"
    );
}

#[tokio::test]
async fn live_refresh_does_not_advance_unprocessed_event_cursor() {
    let (mut app, _temp) = test_app().await;
    let processed = app.event_cursor;
    let mut snapshot = app.handle.snapshot().await.expect("snapshot");
    snapshot.event_cursor = processed.saturating_add(100);

    // A refresh issued immediately after submit may race with events that
    // are already in the bridge. Those envelopes still need to be consumed
    // by the live subscription, so an ordinary refresh must preserve the
    // facade's processed cursor.
    app.merge_snapshot(&snapshot, false);
    assert_eq!(app.event_cursor, processed);

    // Initial connection/resync explicitly establishes a new baseline.
    app.merge_snapshot(&snapshot, true);
    assert_eq!(app.event_cursor, processed + 100);
}

#[tokio::test]
async fn live_tool_approval_is_visible_and_resolved_without_waiting_for_snapshot() {
    let (mut app, _temp) = test_app().await;
    let session_id = app.current.session_id.clone();
    let read = ToolCall {
        id: "read-1".into(),
        name: "file_list".into(),
        arguments: serde_json::json!({"path": "."}),
    };
    let shell = ToolCall {
        id: "shell-1".into(),
        name: "terminal_exec".into(),
        arguments: serde_json::json!({"program": "pwd"}),
    };
    let mut cursor = app.event_cursor;
    let mut push = |app: &mut App, event: ProtocolEvent| {
        cursor += 1;
        handle_envelope(
            app,
            &Envelope {
                cursor,
                session_id: session_id.clone(),
                event,
            },
        )
    };

    // Reproduce one model round that first completes an allowed read tool,
    // then blocks on a command requiring approval.
    push(&mut app, ProtocolEvent::ToolStarted { call: read.clone() });
    push(
        &mut app,
        ProtocolEvent::ToolFinished {
            call: read,
            result: "[]".into(),
        },
    );
    assert!(push(
        &mut app,
        ProtocolEvent::Approval {
            approval_id: "approval-1".into(),
            call: shell,
            reason: "command execution requires approval".into(),
            source_session_id: None,
            source_title: None,
        },
    ));
    assert!(app.has_pending_approval());
    assert_eq!(
        app.pending_approval()
            .map(|approval| approval.approval_id.as_str()),
        Some("approval-1")
    );
    assert!(app.current.pending_approval.is_some());

    assert!(push(
        &mut app,
        ProtocolEvent::ApprovalResolved {
            approval_id: "approval-1".into(),
            approved: false,
        },
    ));
    assert!(!app.has_pending_approval());
    assert!(app.current.pending_approval.is_none());
    assert!(
        app.sync_pending,
        "resolution must refresh the next global-oldest approval"
    );
}

#[test]
fn should_coalesce_stream_redraw_is_a_phase_barrier() {
    let session = "s1".to_owned();
    let envelope = |event: ProtocolEvent| Envelope {
        cursor: 1,
        session_id: session.clone(),
        event,
    };
    // Body and reasoning deltas stay coalescable into the 16ms window.
    assert!(should_coalesce_stream_redraw(
        &session,
        &envelope(ProtocolEvent::TextDelta { delta: "d".into() }),
    ));
    assert!(should_coalesce_stream_redraw(
        &session,
        &envelope(ProtocolEvent::ReasoningDelta { delta: "r".into() }),
    ));
    // The completion barrier is a phase boundary and must never be batched:
    // it is painted immediately so the finished thinking view shows alone.
    assert!(!should_coalesce_stream_redraw(
        &session,
        &envelope(ProtocolEvent::ReasoningCompleted),
    ));
    // Tool-call streaming progress is low-frequency and phase-forming: it is
    // never coalesced so the animated "generating tool call" row appears on
    // its own frame instead of waiting for the 16ms window.
    assert!(!should_coalesce_stream_redraw(
        &session,
        &envelope(ProtocolEvent::ToolCallStreaming {
            name: Some("file_write".into()),
            received_bytes: 9216,
        }),
    ));
    // Events for a background session never force an immediate repaint of
    // the active view through this path.
    assert!(!should_coalesce_stream_redraw(
        "other",
        &envelope(ProtocolEvent::TextDelta { delta: "d".into() }),
    ));
}

#[tokio::test]
async fn reasoning_completed_draws_summary_before_body_frame() {
    let (mut app, _temp) = test_app().await;
    let session = app.current.session_id.clone();
    let mut cursor = app.event_cursor;
    let mut push = |app: &mut App, event: ProtocolEvent| {
        cursor += 1;
        handle_envelope(
            app,
            &Envelope {
                cursor,
                session_id: session.clone(),
                event,
            },
        );
    };
    push(&mut app, ProtocolEvent::ModelStreaming);
    push(
        &mut app,
        ProtocolEvent::ReasoningDelta {
            delta: "思考摘要文本".into(),
        },
    );
    push(&mut app, ProtocolEvent::ReasoningCompleted);

    let compact_of = |app: &mut App| -> String {
        let backend = TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, app)).expect("draw");
        let content = terminal.backend().buffer().content().to_vec();
        let text: String = content.iter().map(|cell| cell.symbol()).collect();
        text.chars().filter(|ch| !ch.is_whitespace()).collect()
    };

    // The frame right after the barrier contains the finished summary and no
    // Agent body yet.
    let frame = compact_of(&mut app);
    assert!(
        frame.contains("思考摘要文本"),
        "the summary must be visible on the barrier frame; got: {frame:?}"
    );
    assert!(
        !frame.contains("正文"),
        "no Agent body may appear before the first TextDelta; got: {frame:?}"
    );

    // The next frame after the first body delta shows summary and body.
    push(
        &mut app,
        ProtocolEvent::TextDelta {
            delta: "正文".into(),
        },
    );
    let frame = compact_of(&mut app);
    assert!(frame.contains("思考摘要文本"), "summary must persist");
    assert!(frame.contains("正文"), "body must stream below the summary");
}

#[tokio::test]
async fn facade_submit_and_load_history_syncs_transcript() {
    let (mut app, _temp) = test_app().await;
    app.input.set("你好");
    app.submit_current().await.expect("submit");
    assert!(!app.current.entries.is_empty());
    assert!(!app.current.session_id.is_empty());
}

#[test]
fn command_text_round_trips() {
    let commands = [
        Command::NewSession,
        Command::Rename(Some("名称".into())),
        Command::Delete,
        Command::Undo,
        Command::Model(Some("gpt-5-mini".into())),
    ];
    for command in commands {
        let text = command_to_text(&command);
        assert!(!text.is_empty());
    }
    assert_eq!(command_to_text(&Command::NewSession), "/new");
    assert_eq!(todo_to_text(&TodoCommand::Clear), "clear");
}

#[test]
fn session_switch_direction_only_accepts_alt_or_ctrl_arrows() {
    let plain = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Up);
    assert_eq!(session_switch_direction(&plain), None);
    let alt_up = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Up,
        crossterm::event::KeyModifiers::ALT,
    );
    assert_eq!(session_switch_direction(&alt_up), Some(-1));
    let ctrl_down = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Down,
        crossterm::event::KeyModifiers::CONTROL,
    );
    assert_eq!(session_switch_direction(&ctrl_down), Some(1));
}

// ---------------------------------------------------------------------------
// Footer pickers: provider, model and thinking level.
//
// Every picker is painted from a row window (`PickerGeometry`) and the mouse
// hit-test resolves the same window, so these tests compare what the frame
// actually shows with what a click on that row applies.
// ---------------------------------------------------------------------------

fn picker_click(column: u16, row: u16) -> Event {
    Event::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn picker_key(code: KeyCode) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE))
}

fn picker_key_with(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
}

fn picker_row(terminal: &Terminal<TestBackend>, row: u16, from: u16, to: u16) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    let mut column = from;
    while column < to {
        let symbol = buffer[(column, row)].symbol();
        let width = UnicodeWidthStr::width(symbol);
        text.push_str(symbol);
        // A double-width glyph owns the cell after it, and that half-cell keeps
        // whatever was painted under the popup, so it is skipped rather than
        // read back as row content.
        column += 1.max(width as u16);
    }
    text
}

fn saved_provider_settings(presets: &[&str]) -> ProviderSettingsDto {
    let profiles = presets
        .iter()
        .map(|preset| ProviderProfileDto {
            id: (*preset).to_owned(),
            preset: (*preset).to_owned(),
            name: String::new(),
            kind: "chat_completions".into(),
            model: format!("{preset}-model"),
            base_url: "https://example.invalid/v1".into(),
            enabled_models: Vec::new(),
        })
        .collect::<Vec<_>>();
    ProviderSettingsDto {
        active: profiles[0].clone(),
        saved: profiles.clone(),
        connected: vec![profiles[0].preset.clone()],
    }
}

fn listed_models(count: usize) -> ProviderModelsState {
    ProviderModelsState {
        provider_id: Some("openai".to_owned()),
        models: (0..count)
            .map(|index| ProviderModelDto {
                id: format!("model-{index:02}"),
                context_window_tokens: None,
                max_output_tokens: None,
            })
            .collect(),
        fetched_at: None,
        loading: false,
        last_error: None,
    }
}

// ---------------------------------------------------------------------------
// The provider panel: one pass paints the rows, and the same pass records the
// rectangles those rows answer to.
// ---------------------------------------------------------------------------

/// Opens the panel after saving one profile **through the core**, so `saved`
/// and the active view come from the authority rather than a hand-faked DTO,
/// and paints it once so the caller can click the cells the frame really drew.
async fn painted_panel(width: u16) -> (App, Terminal<TestBackend>, tempfile::TempDir) {
    let (mut app, temp) = test_app().await;
    app.handle
        .set_provider_profile(
            "openai",
            ProviderPreset::OpenAi,
            None,
            "openai-model",
            None,
            None,
            None,
            None,
        )
        .await
        .expect("save an openai profile");
    // The key state depends on the host's keyring and environment, so it is
    // pinned here: a panel test must not flip with the machine it runs on.
    let backend = TestBackend::new(width, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    super::provider_editor::open_settings(&mut app).await;
    app.provider_models = listed_models(6);
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    (app, terminal, temp)
}

async fn send(app: &mut App, event: Event) {
    handle_terminal_event(app, event).await.expect("event");
}

use super::provider_editor::ProviderRow;

/// Reaches into the panel's own state, which is exactly what the draw pass
/// does: the tests below arm a background answer without going to the network.
fn with_editor<R>(app: &mut App, body: impl FnOnce(&mut ProviderEditor) -> R) -> Option<R> {
    super::provider_editor::with_editor(app, |_, editor| body(editor))
}

/// Reads one painted row the way the terminal lays it out: a wide symbol owns
/// the cell that follows it. `TestBackend` leaves that follower cell holding
/// whatever the previous frame put there, so a column-by-column read interleaves
/// stale characters into every CJK row.
fn panel_cells(
    buffer: &ratatui::buffer::Buffer,
    rect: ratatui::layout::Rect,
    row: u16,
) -> Vec<(u16, &str)> {
    let mut cells = Vec::new();
    let mut column = rect.x;
    while column < rect.x + rect.width {
        let symbol = buffer[(column, row)].symbol();
        cells.push((column, symbol));
        column += UnicodeWidthStr::width(symbol).max(1) as u16;
    }
    cells
}

#[tokio::test]
async fn provider_panel_opens_on_the_active_provider_with_its_draft_loaded() {
    let (app, _terminal, _temp) = painted_panel(120).await;
    let editor = app.provider_editor.as_ref().expect("panel open");
    assert_eq!(editor.pane, EditorPane::Providers);
    assert_eq!(
        editor.selected_provider().map(|row| row.id.as_str()),
        Some("openai"),
        "the panel opens on the provider that is actually active"
    );
    assert_eq!(editor.field(), EditorField::Name);
    // The draft is the core's own profile, not a stale copy of the local config.
    assert_eq!(editor.form.provider.model, "openai-model");
    assert_eq!(
        editor.rows.len(),
        4,
        "the four built-in families are listed even when unsaved"
    );
    assert_eq!(
        editor.row_rects.len(),
        editor.rows.len() + 1,
        "the create command is one more address than the profiles"
    );
    assert!(
        editor
            .row_rects
            .iter()
            .chain(editor.field_rects.iter())
            .all(|rect| rect.width > 0 && rect.height == 1),
        "every painted row answers a click"
    );
    assert_eq!(editor.field_rects.len(), EDITOR_FIELDS.len());
    assert_eq!(
        editor.action_rects.len(),
        3,
        "apply, delete and cancel each own a rectangle"
    );
    assert!(!editor.delete_confirm, "deleting never starts armed");
}

#[tokio::test]
async fn provider_panel_rows_and_fields_answer_the_cells_they_paint() {
    let (mut app, mut terminal, _temp) = painted_panel(120).await;
    let rows = app
        .provider_editor
        .as_ref()
        .expect("panel open")
        .row_rects
        .clone();
    // Each painted profile row loads the draft it names.
    for (index, rect) in rows.iter().enumerate().take(4) {
        send(&mut app, picker_click(rect.x + 2, rect.y)).await;
        let editor = app.provider_editor.as_ref().expect("panel open");
        assert_eq!(
            editor.selected_row, index,
            "row {index} resolves its own cell"
        );
        assert_eq!(editor.pane, EditorPane::Fields);
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .expect("draw");
    }
    // The trailing address starts a create draft instead of selecting a profile.
    let add = rows[4];
    send(&mut app, picker_click(add.x + 2, add.y)).await;
    let editor = app.provider_editor.as_ref().expect("panel open");
    assert!(editor.creating(), "an empty id is the create address");
    assert!(editor.form.provider.name.trim().is_empty());
    assert_eq!(editor.form.provider.preset, ProviderPreset::Custom);
    assert_eq!(
        editor.field(),
        EditorField::Name,
        "a create starts on the required field"
    );

    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let fields = app
        .provider_editor
        .as_ref()
        .expect("panel open")
        .field_rects
        .clone();
    for (index, rect) in fields.iter().enumerate() {
        send(&mut app, picker_click(rect.x + 1, rect.y)).await;
        assert_eq!(
            app.provider_editor
                .as_ref()
                .expect("panel open")
                .field_index,
            index,
            "field row {index} resolves its own cell"
        );
    }

    // A click on the panel frame itself is swallowed rather than leaking to the
    // control the panel covers.
    let popup = app.provider_editor.as_ref().expect("panel open").rect;
    send(&mut app, picker_click(popup.x, popup.y)).await;
    assert!(
        app.provider_editor.is_some(),
        "the frame is part of the panel"
    );
}

#[tokio::test]
async fn provider_panel_escape_steps_back_from_fields_to_rows_to_closed() {
    let (mut app, _terminal, _temp) = painted_panel(120).await;
    send(&mut app, picker_key(KeyCode::Tab)).await;
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").pane,
        EditorPane::Fields
    );
    send(&mut app, picker_key(KeyCode::Up)).await;
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field_index,
        EDITOR_FIELDS.len() - 1,
        "the field cursor wraps"
    );
    send(&mut app, picker_key(KeyCode::Down)).await;
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field_index,
        0,
        "and wraps forward again"
    );
    // Esc walks back one level at a time: fields, then the panel itself.
    send(&mut app, picker_key(KeyCode::Esc)).await;
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").pane,
        EditorPane::Providers,
        "leaving the fields keeps the draft"
    );
    send(&mut app, picker_key(KeyCode::Esc)).await;
    assert!(
        app.provider_editor.is_none(),
        "the second Esc closes the panel"
    );
}

#[tokio::test]
async fn provider_panel_refuses_an_incomplete_draft_before_touching_the_core() {
    let (mut app, _terminal, _temp) = painted_panel(120).await;
    // Walk to the create command and enter it.
    for _ in 0..4 {
        send(&mut app, picker_key(KeyCode::Down)).await;
    }
    send(&mut app, picker_key(KeyCode::Enter)).await;
    assert!(app.provider_editor.as_ref().expect("panel").creating());

    send(
        &mut app,
        picker_key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
    )
    .await;
    let editor = app.provider_editor.as_ref().expect("panel stays open");
    assert_eq!(
        editor.error.as_deref(),
        Some("自定义供应商名称不能为空"),
        "the reason is reported inside the panel, not behind it"
    );
    assert!(
        editor.blocked_reason().is_some(),
        "and the same reason is what disables apply"
    );

    // A name is not enough: a provider without a model cannot be applied.
    for character in "MyProvider".chars() {
        send(&mut app, picker_key(KeyCode::Char(character))).await;
    }
    send(
        &mut app,
        picker_key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
    )
    .await;
    assert_eq!(
        app.provider_editor
            .as_ref()
            .expect("panel")
            .error
            .as_deref(),
        Some("模型不能为空")
    );

    // With both, the apply reaches the core, which mints the id.
    for _ in 0..3 {
        send(&mut app, picker_key(KeyCode::Down)).await;
    }
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field(),
        EditorField::Model
    );
    for character in "test-model".chars() {
        send(&mut app, picker_key(KeyCode::Char(character))).await;
    }
    send(
        &mut app,
        picker_key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
    )
    .await;
    let editor = app.provider_editor.as_ref().expect("the panel stays open");
    assert_eq!(editor.error, None, "a complete draft applies");
    assert!(
        editor
            .selected_provider()
            .is_some_and(|row| row.id.starts_with("custom-")),
        "the core minted a fresh custom id and the panel reselected it: {:?}",
        editor.selected_provider().map(|row| row.id.clone())
    );
    assert!(
        app.current.status.starts_with("已保存"),
        "status: {}",
        app.current.status
    );
    assert!(
        app.provider_settings
            .as_ref()
            .expect("settings")
            .saved
            .iter()
            .any(|profile| profile.name == "MyProvider"),
        "the new profile is visible in the core's own view: {:?}",
        app.provider_settings
            .as_ref()
            .expect("settings")
            .saved
            .iter()
            .map(|profile| (&profile.id, &profile.preset, &profile.name))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn provider_panel_arms_deletion_and_only_deletes_on_confirmation() {
    let (mut app, _terminal, _temp) = painted_panel(120).await;
    // An unsaved built-in template has nothing to remove.
    send(&mut app, picker_key(KeyCode::Down)).await;
    send(
        &mut app,
        picker_key_with(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .await;
    let editor = app.provider_editor.as_ref().expect("panel");
    assert!(!editor.delete_confirm, "there is nothing to confirm");
    assert_eq!(editor.error.as_deref(), Some("内置模板尚未保存，无需删除"));

    // The saved active profile does arm the confirmation — and stays armed.
    send(&mut app, picker_key(KeyCode::Up)).await;
    send(
        &mut app,
        picker_key_with(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(app.provider_editor.as_ref().expect("panel").delete_confirm);
    send(&mut app, picker_key(KeyCode::Char('n'))).await;
    assert!(
        !app.provider_editor.as_ref().expect("panel").delete_confirm,
        "n cancels without deleting"
    );
    assert!(
        app.provider_settings
            .as_ref()
            .expect("settings")
            .saved
            .iter()
            .any(|profile| profile.id == "openai"),
        "nothing was removed"
    );
    // While the confirmation is armed, only its own answers act: a stray
    // keystroke must not be read as "yes".
    send(
        &mut app,
        picker_key_with(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .await;
    assert!(app.provider_editor.as_ref().expect("panel").delete_confirm);
    send(&mut app, picker_key(KeyCode::Char('x'))).await;
    assert!(
        app.provider_editor.as_ref().expect("panel").delete_confirm,
        "an unrelated key leaves the question open"
    );
    assert!(
        app.provider_settings
            .as_ref()
            .expect("settings")
            .saved
            .iter()
            .any(|profile| profile.id == "openai"),
        "and still removes nothing"
    );
}

#[tokio::test]
async fn provider_panel_model_picker_fills_the_draft_without_applying_it() {
    let (mut app, mut terminal, _temp) = painted_panel(120).await;
    send(&mut app, picker_key(KeyCode::Tab)).await;
    for _ in 0..3 {
        send(&mut app, picker_key(KeyCode::Down)).await;
    }
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field(),
        EditorField::Model
    );
    let before = app
        .provider_editor
        .as_ref()
        .expect("panel")
        .form
        .provider
        .model
        .clone();
    send(&mut app, picker_key(KeyCode::Enter)).await;
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let editor = app.provider_editor.as_ref().expect("panel");
    assert!(editor.model_picker.open);
    assert!(editor.model_picker_rect.is_some(), "the picker was painted");
    let choices = editor.model_choices(&app);
    assert!(choices.len() >= 2, "the picker offers more than one row");
    assert_eq!(
        editor.model_picker.selected,
        choices
            .iter()
            .position(|choice| choice.id == before)
            .expect("the draft's own model is one of the offers"),
        "the draft's own model is highlighted first"
    );
    let selected = editor.model_picker.selected;

    // Esc abandons the picker without touching the draft.
    send(&mut app, picker_key(KeyCode::Esc)).await;
    assert!(
        !app.provider_editor
            .as_ref()
            .expect("panel")
            .model_picker
            .open
    );
    assert_eq!(
        app.provider_editor
            .as_ref()
            .expect("panel")
            .form
            .provider
            .model,
        before
    );

    // Down then Enter takes exactly the row after the highlighted one — or, on
    // the trailing "type it myself" row, keeps the value and only returns focus.
    let next = selected + 1;
    let expected = if next < choices.len() {
        choices[next].id.clone()
    } else {
        before.clone()
    };
    send(&mut app, picker_key(KeyCode::Enter)).await;
    send(&mut app, picker_key(KeyCode::Down)).await;
    send(&mut app, picker_key(KeyCode::Enter)).await;
    let editor = app.provider_editor.as_ref().expect("panel");
    assert!(!editor.model_picker.open, "choosing closes the picker");
    assert_eq!(editor.form.provider.model, expected);
    assert_eq!(
        app.provider_settings
            .as_ref()
            .expect("settings")
            .active
            .model,
        "openai-model",
        "the draft is not the profile until it is applied"
    );
}

#[tokio::test]
async fn provider_panel_keeps_the_stacked_pane_clickable_on_a_narrow_terminal() {
    let (mut app, mut terminal, _temp) = painted_panel(68).await;
    send(&mut app, picker_key(KeyCode::Tab)).await;
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let editor = app.provider_editor.as_ref().expect("panel");
    assert_eq!(editor.pane, EditorPane::Fields);
    assert!(
        editor
            .field_rects
            .iter()
            .all(|rect| rect.width > 0 && rect.right() <= editor.rect.right()),
        "the stacked pane keeps a real width instead of a zero-width hit box"
    );
    let model = editor.field_rects[EDITOR_FIELDS
        .iter()
        .position(|field| *field == EditorField::Model)
        .expect("model row")];
    send(&mut app, picker_click(model.x + 1, model.y)).await;
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field(),
        EditorField::Model
    );
}

#[tokio::test]
async fn provider_panel_window_fetch_fills_the_buffer_from_the_reported_models() {
    let (mut app, _terminal, _temp) = painted_panel(120).await;
    send(&mut app, picker_key(KeyCode::Tab)).await;
    for _ in 0..5 {
        send(&mut app, picker_key(KeyCode::Down)).await;
    }
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field(),
        EditorField::ContextWindow
    );
    let editor = app.provider_editor.as_ref().expect("panel");
    assert!(
        editor.window_fetch_available(&app),
        "the draft is the active profile, so its endpoint is the one to ask"
    );
    assert!(
        editor.window_hint(&app).is_some(),
        "an unresolved window explains the override range"
    );

    // Arm the fetch the way its accelerator does, then land the answer the way the
    // event loop does — no network, and no dependence on the host's keyring. The
    // accelerator's own routing is covered by the not-active test below, which
    // reaches the same handler without starting a background task.
    let armed = with_editor(&mut app, |editor| {
        editor.window_fetch_pending = true;
        editor.window_fetch_pending
    });
    assert_eq!(armed, Some(true));

    let generation = app.model_refresh_generation;
    let provider_id = app.active_provider_id();
    let model = app
        .provider_editor
        .as_ref()
        .expect("panel")
        .form
        .provider
        .model
        .clone();
    let report = |models: Vec<ProviderModelDto>| ModelRefreshResult {
        generation,
        provider_id: provider_id.clone(),
        result: Ok(ProviderModelsDto {
            models,
            fetched_at: None,
        }),
    };
    apply_model_refresh_result(
        &mut app,
        report(vec![ProviderModelDto {
            id: model.clone(),
            context_window_tokens: Some(200_000),
            max_output_tokens: Some(8_192),
        }]),
    );
    let editor = app.provider_editor.as_ref().expect("panel");
    assert_eq!(
        editor.window_input, "200000",
        "the reported window becomes the explicit value"
    );
    assert_eq!(
        editor.window_note.as_deref(),
        Some("已获取；保留则作为显式值优先生效，清空则交给自动解析。")
    );
    assert_eq!(
        editor.window_value(&app),
        "200000",
        "the buffer wins while set"
    );

    // A report that carries no window for the model says so instead of guessing.
    let _ = with_editor(&mut app, |editor| editor.window_fetch_pending = true);
    apply_model_refresh_result(
        &mut app,
        report(vec![ProviderModelDto {
            id: model.clone(),
            context_window_tokens: None,
            max_output_tokens: None,
        }]),
    );
    assert_eq!(
        app.provider_editor
            .as_ref()
            .expect("panel")
            .window_note
            .as_deref(),
        Some("接口与内置注册表均未报告该模型窗口，请手填。")
    );

    // A failed refresh is reported as a failure, not as "unknown".
    let _ = with_editor(&mut app, |editor| editor.window_fetch_pending = true);
    apply_model_refresh_result(
        &mut app,
        ModelRefreshResult {
            generation,
            provider_id,
            result: Err("gateway unreachable".to_owned()),
        },
    );
    assert_eq!(
        app.provider_editor
            .as_ref()
            .expect("panel")
            .window_note
            .as_deref(),
        Some("刷新失败：网关不可达或密钥未配置。")
    );
}

/// Every painted body row must put the pane divider in the same column, no
/// matter which status that row carries or how long its name is. The panes were
/// once composed into a single line and kept aligned only by padding the left
/// pane to a fixed width, so a status one cell wider than the reservation
/// (`当前 已配置 ●` is 13 cells, `需要 API Key` is 12) pushed that row's divider
/// sideways and every row looked misaligned against it.
#[tokio::test]
async fn provider_panel_paints_one_divider_column_for_every_row_shape() {
    let (mut app, mut terminal, _temp) = painted_panel(120).await;
    let rows = vec![
        ProviderRow {
            id: "openai".to_owned(),
            preset: ProviderPreset::OpenAi,
            label: "OpenAI".to_owned(),
            saved: true,
            active: true,
            connected: true,
        },
        ProviderRow {
            id: "custom-1".to_owned(),
            preset: ProviderPreset::Custom,
            label: "某公司内部网关供应商一号".to_owned(),
            saved: true,
            active: false,
            connected: true,
        },
        ProviderRow {
            id: "deepseek".to_owned(),
            preset: ProviderPreset::DeepSeek,
            label: "DeepSeek".to_owned(),
            saved: false,
            active: false,
            connected: false,
        },
        ProviderRow {
            id: "custom-2".to_owned(),
            preset: ProviderPreset::Custom,
            label: "a-very-long-ascii-provider-name-without-any-spaces".to_owned(),
            saved: true,
            active: false,
            connected: false,
        },
    ];
    let armed = with_editor(&mut app, |editor| {
        editor.rows = rows;
        editor.selected_row = 0;
        editor.pane = EditorPane::Fields;
        editor.field_index = 4;
        editor.form.provider.base_url =
            "https://gateway.internal.example.com/openai-compatible/v1".to_owned();
    });
    assert!(armed.is_some(), "the panel is open");

    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let rect = app.provider_editor.as_ref().expect("panel").rect;
    // The body is what sits above the separator and the action row: those two
    // footer rows carry the frame's side borders but no pane divider.
    let body_rows = rect.height - 4;
    let dividers: Vec<Vec<u16>> = {
        let buffer = terminal.backend().buffer();
        (rect.y + 1..rect.y + 1 + body_rows)
            .map(|row| {
                panel_cells(buffer, rect, row)
                    .into_iter()
                    .filter(|(_, symbol)| *symbol == "│")
                    .map(|(column, _)| column)
                    .collect()
            })
            .collect()
    };
    assert_eq!(
        dividers.len() as u16,
        body_rows,
        "the body painted every row"
    );
    for columns in &dividers {
        assert_eq!(
            columns.len(),
            3,
            "each body row shows the frame and one divider: {columns:?}"
        );
        assert_eq!(
            columns, &dividers[0],
            "every body row shares one divider column"
        );
    }
}

/// The provider name is the row's identity, so a name wider than the label
/// column is truncated rather than allowed to eat the status column: the status
/// is what tells the user the row has no key yet.
#[tokio::test]
async fn provider_panel_truncates_a_long_name_before_the_status() {
    let (mut app, mut terminal, _temp) = painted_panel(120).await;
    let long_name = "某公司内部网关供应商一号与灾备网关二号以及测试网关三号";
    let armed = with_editor(&mut app, |editor| {
        editor.rows = vec![ProviderRow {
            id: "custom-1".to_owned(),
            preset: ProviderPreset::Custom,
            label: long_name.to_owned(),
            saved: true,
            active: false,
            connected: false,
        }];
        editor.selected_row = 0;
    });
    assert!(armed.is_some(), "the panel is open");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let rect = app.provider_editor.as_ref().expect("panel").rect;
    let row = rect.y + 1;
    let text: String = {
        let buffer = terminal.backend().buffer();
        panel_cells(buffer, rect, row)
            .into_iter()
            .map(|(_, symbol)| symbol)
            .collect()
    };
    assert!(
        UnicodeWidthStr::width(long_name) > 40,
        "the name is wider than the pane's label column: {text:?}"
    );
    assert!(
        !text.contains(long_name),
        "the whole name cannot fit: {text:?}"
    );
    assert!(text.contains("..."), "the long name is truncated: {text:?}");
    assert!(
        text.contains("已配置"),
        "and the status survives the truncation: {text:?}"
    );
}

#[tokio::test]
async fn provider_panel_refuses_to_fetch_a_window_for_a_draft_that_is_not_active() {
    let (mut app, _terminal, _temp) = painted_panel(120).await;
    // DeepSeek is not the active provider, so its base URL is not the one the
    // core would query on the panel's behalf.
    send(&mut app, picker_key(KeyCode::Down)).await;
    send(&mut app, picker_key(KeyCode::Enter)).await;
    for _ in 0..5 {
        send(&mut app, picker_key(KeyCode::Down)).await;
    }
    assert_eq!(
        app.provider_editor.as_ref().expect("panel").field(),
        EditorField::ContextWindow
    );
    let editor = app.provider_editor.as_ref().expect("panel");
    assert!(
        !editor.window_fetch_available(&app),
        "a draft that is not the active profile has no endpoint to ask"
    );
    send(
        &mut app,
        picker_key_with(KeyCode::Char('g'), KeyModifiers::CONTROL),
    )
    .await;
    let editor = app.provider_editor.as_ref().expect("panel");
    assert_eq!(
        editor.window_note.as_deref(),
        Some("只能获取当前生效供应商的模型窗口")
    );
    assert!(!editor.window_fetch_pending, "and nothing was requested");
}

#[tokio::test]
async fn thinking_picker_is_operable_from_the_keyboard() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let initial = app.thinking_level();
    let active = app
        .thinking_profile()
        .options
        .iter()
        .position(|level| *level == initial)
        .expect("active level is offered");

    // Alt+T opens the picker without a mouse and parks the cursor on the level
    // that is applied today.
    handle_terminal_event(
        &mut app,
        picker_key_with(KeyCode::Char('t'), KeyModifiers::ALT),
    )
    .await
    .expect("open thinking picker");
    assert!(app.thinking_menu_open, "Alt+T must open the picker");
    assert_eq!(app.thinking_menu_cursor.row, active);

    // Down navigates instead of dismissing, Enter applies.
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("down");
    assert!(app.thinking_menu_open, "navigation keeps the picker open");
    assert_eq!(app.thinking_menu_cursor.row, active + 1);
    handle_terminal_event(&mut app, picker_key(KeyCode::Enter))
        .await
        .expect("enter");
    assert!(!app.thinking_menu_open);
    assert_eq!(
        app.thinking_level(),
        ThinkingLevel::None,
        "the highlighted row is what got applied"
    );

    // Esc closes without applying anything.
    let applied = app.thinking_level();
    handle_terminal_event(
        &mut app,
        picker_key_with(KeyCode::Char('t'), KeyModifiers::ALT),
    )
    .await
    .expect("reopen");
    handle_terminal_event(&mut app, picker_key(KeyCode::Up))
        .await
        .expect("up");
    handle_terminal_event(&mut app, picker_key(KeyCode::Esc))
        .await
        .expect("esc");
    assert!(!app.thinking_menu_open);
    assert_eq!(
        app.thinking_level(),
        applied,
        "Esc must not apply the cursor"
    );

    // A key the picker does not use dismisses it, so the composer stays usable.
    handle_terminal_event(
        &mut app,
        picker_key_with(KeyCode::Char('t'), KeyModifiers::ALT),
    )
    .await
    .expect("reopen");
    // The OpenAI profile offers a single column: ←/→ must not underflow the
    // cursor out of it, and must not dismiss the picker either.
    let before = app.thinking_menu_cursor;
    handle_terminal_event(&mut app, picker_key(KeyCode::Left))
        .await
        .expect("left");
    handle_terminal_event(&mut app, picker_key(KeyCode::Right))
        .await
        .expect("right");
    assert!(app.thinking_menu_open, "column keys keep the picker open");
    assert_eq!(app.thinking_menu_cursor.row, before.row);
    assert_eq!(
        app.thinking_menu_cursor.column, 0,
        "there is no second column to move to"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Char('z')))
        .await
        .expect("dismiss");
    assert!(!app.thinking_menu_open);
}

#[tokio::test]
async fn thinking_picker_clicks_the_cell_it_renders() {
    let (mut app, _temp) = test_app().await;
    app.config.provider.preset = ProviderPreset::Qwen;
    app.config.provider.model = "qwen37-max".into();
    app.config.provider.thinking_level = ThinkingLevel::Enabled;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app.thinking_control_rect.expect("thinking control");
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let picker = app.thinking_menu_geometry.expect("thinking geometry");
    let inner = picker.inner();
    let columns = crate::ui_view_model::thinking_menu_columns(&app);
    assert_eq!(
        columns.len(),
        2,
        "the Qwen3.7 profile paints a level and a budget column"
    );
    assert!(
        picker.visible >= 6,
        "the picker paints every budget row: {picker:?}"
    );
    let level_width = crate::ui_view_model::THINKING_LEVEL_COLUMN_WIDTH;
    for row in inner.y..inner.bottom() {
        let index = picker.scroll + usize::from(row - inner.y);
        let text = picker_row(&terminal, row, inner.x, inner.right());
        for (column, anchor) in [(0usize, inner.x), (1usize, inner.x + level_width)] {
            let painted = columns[column].cells.get(index);
            let resolved = crate::app::provider::thinking_menu_selection(&app, picker, anchor, row);
            match (painted, resolved) {
                (Some(cell), Some(hit)) => {
                    assert_eq!(
                        hit.label, cell.label,
                        "row {row} paints {text:?} but the click resolves another cell"
                    );
                    assert!(
                        text.contains(&cell.label),
                        "row {row} column {column} resolves {} but is not painted there",
                        cell.label
                    );
                }
                (None, None) => {}
                (painted, resolved) => panic!(
                    "row {row} column {column} paints {:?} and resolves {:?}",
                    painted.map(|cell| &cell.label),
                    resolved.map(|cell| cell.label)
                ),
            }
        }
    }

    // Left of the level column and right of the budget column resolve nothing,
    // so a click beside the table can not apply a thinking setting.
    assert!(
        crate::app::provider::thinking_menu_selection(&app, picker, inner.x - 1, inner.y).is_none()
    );

    // ←/→ switch columns; Enter applies the row the cursor is highlighted on.
    handle_terminal_event(&mut app, picker_key(KeyCode::Right))
        .await
        .expect("right");
    assert_eq!(app.thinking_menu_cursor.column, 1);
    assert!(
        app.thinking_menu_open,
        "switching columns keeps the picker open"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("down");
    let row = app.thinking_menu_cursor.row;
    let expected = columns[1].cells[row].budget;
    assert!(
        expected.is_some(),
        "the cursor moved onto a real budget row"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Enter))
        .await
        .expect("enter");
    assert!(!app.thinking_menu_open);
    assert_eq!(
        app.thinking_budget_tokens(),
        expected,
        "Enter applies the highlighted budget"
    );
    assert_eq!(app.thinking_level(), ThinkingLevel::Enabled);
}

#[tokio::test]
async fn provider_picker_rows_map_to_the_rendered_provider() {
    let (mut app, _temp) = test_app().await;
    let fake = saved_provider_settings(&["openai", "deepseek", "qwen", "volcano", "custom"]);
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app.provider_control_rect.expect("provider control");
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    // Opening re-reads the core view, so the multi-provider list is seeded
    // afterwards: the picker must paint and resolve the same rows.
    app.provider_settings = Some(fake);
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let picker = app.provider_menu_geometry.expect("provider geometry");
    let inner = picker.inner();
    assert_eq!(
        app.provider_menu_selected, 0,
        "the cursor starts on the active provider, not on a stale index"
    );
    // Only the item window resolves providers: the pinned action row below it
    // is a separate control and must never be mistaken for a provider.
    for row in inner.y..inner.y.saturating_add(picker.visible as u16) {
        let text = picker_row(&terminal, row, inner.x, inner.right());
        let choice = crate::app::provider::provider_menu_selection(&app, picker, inner.x + 2, row)
            .unwrap_or_else(|| panic!("row {row} paints {text:?} but resolves no provider"));
        assert!(
            text.contains(choice.label.as_str()),
            "row {row} paints {text:?} but the click resolves {}",
            choice.label
        );
    }
    let action = picker.action.expect("the picker pins a settings entry");
    assert!(
        inner.y.saturating_add(picker.visible as u16) == action.y
            && action.bottom() == inner.bottom(),
        "the pinned entry owns the last inner row: {action:?} in {inner:?}"
    );
    let action_text = picker_row(&terminal, action.y, inner.x, inner.right());
    assert!(
        action_text.contains("供应商设置") && action_text.contains("Ctrl+S"),
        "the pinned row advertises the settings entry: {action_text:?}"
    );
    assert_eq!(
        crate::app::provider::provider_menu_selection(&app, picker, action.x + 1, action.y),
        None,
        "the pinned row must not resolve a provider"
    );

    // Keyboard: Down moves the visible cursor, Esc dismisses without applying.
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("down");
    assert_eq!(app.provider_menu_selected, 1);
    handle_terminal_event(&mut app, picker_key(KeyCode::Esc))
        .await
        .expect("esc");
    assert!(!app.provider_menu_open);
    assert_eq!(app.active_preset(), ProviderPreset::OpenAi);

    // A click outside the painted frame only dismisses.
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("reopen");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let picker = app.provider_menu_geometry.expect("provider geometry");
    handle_terminal_event(&mut app, picker_click(picker.area.right(), picker.area.y))
        .await
        .expect("click outside");
    assert!(!app.provider_menu_open);
    assert_eq!(app.active_preset(), ProviderPreset::OpenAi);
}

#[tokio::test]
async fn model_picker_click_hits_the_rendered_row_when_scrolled() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(100, 16);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app.model_control_rect.expect("model control");
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    // A 20-model list only exists after the cache-only read, so it is seeded
    // here: the window has to scroll, which is where the old hit-test drifted.
    app.provider_models = listed_models(20);
    app.model_menu_selected = 17;
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let picker = app.model_menu_geometry.expect("scrolled geometry");
    let inner = picker.inner();
    assert!(
        picker.visible < 20 && picker.scroll > 0,
        "the window must actually scroll for this to be a regression test: {picker:?}"
    );
    assert!(
        picker.area.bottom() <= inner.y + inner.height + 2 && picker.visible <= 20,
        "the popup keeps its rows inside the screen: {picker:?}"
    );
    for row in inner.y..inner.bottom() {
        let text = picker_row(&terminal, row, inner.x, inner.right());
        let model = crate::app::provider::model_menu_selection(&app, picker, inner.x + 1, row)
            .unwrap_or_else(|| panic!("row {row} paints {text:?} but resolves no model"));
        assert!(
            text.contains(&model),
            "row {row} paints {text:?} but the click resolves {model}"
        );
    }

    // Keyboard navigation wraps inside the list and reels a stale cursor back
    // onto it, so Enter never addresses a row that is not painted (an aborted
    // process is what an out-of-range index costs in the release profile).
    let count = crate::app::model_choices(&app).len();
    assert!(count > 3, "the seeded list is long enough to wrap: {count}");
    app.model_menu_selected = count + 80;
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("down with a stale cursor");
    assert_eq!(
        app.model_menu_selected,
        count - 1,
        "the stale cursor lands on the last painted row"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Up))
        .await
        .expect("up");
    assert_eq!(app.model_menu_selected, count - 2);
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("down again");
    assert_eq!(
        app.model_menu_selected,
        count - 1,
        "an in-range cursor moves normally"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Down))
        .await
        .expect("wrap");
    assert_eq!(
        app.model_menu_selected, 0,
        "navigation wraps inside the list"
    );
    handle_terminal_event(&mut app, picker_key(KeyCode::Esc))
        .await
        .expect("esc");
    assert!(!app.model_menu_open);
}

#[tokio::test]
async fn provider_picker_enter_clamps_a_stale_cursor() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app.provider_control_rect.expect("provider control");
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    app.provider_settings = Some(saved_provider_settings(&["openai", "deepseek"]));
    app.provider_menu_selected = 99;
    handle_terminal_event(&mut app, picker_key(KeyCode::Enter))
        .await
        .expect("enter with a stale cursor");
    assert!(!app.provider_menu_open, "Enter closes the picker");
    assert_eq!(
        app.active_preset(),
        ProviderPreset::OpenAi,
        "no key for the target provider means no switch"
    );
    assert!(
        app.current.status.contains("DeepSeek"),
        "the highlighted row is what was requested: {}",
        app.current.status
    );
    assert!(
        app.provider_editor.is_some(),
        "a provider without a key is redirected into the panel instead of switched"
    );
    let editor_row = app
        .provider_editor
        .as_ref()
        .and_then(|editor| editor.selected_provider().map(|row| row.id.clone()));
    assert_eq!(
        editor_row.as_deref(),
        Some("deepseek"),
        "the panel opens focused on the provider the switcher could not switch to"
    );
}

// ---------------------------------------------------------------------------
// Provider discoverability: the settings panel must be reachable from what the
// frame actually paints, not only from a shortcut the user has to know.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_picker_pins_a_settings_entry_that_opens_on_click_and_ctrl_s() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app.provider_control_rect.expect("provider control");

    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let action = app
        .provider_menu_geometry
        .expect("geometry")
        .action
        .expect("pinned settings entry");
    assert!(
        app.provider_menu_open,
        "the picker is open on the pinned row"
    );
    handle_terminal_event(&mut app, picker_click(action.x + 1, action.y))
        .await
        .expect("click the pinned entry");
    assert!(
        !app.provider_menu_open,
        "the picker closes behind the panel"
    );
    assert!(
        app.provider_editor.is_some(),
        "clicking the pinned row opens the provider settings panel"
    );

    // Ctrl+S is the same entry from the keyboard, and it must be claimed by the
    // picker before the shared "unhandled key dismisses" rule can swallow it.
    app.provider_editor = None;
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("reopen");
    assert!(app.provider_menu_open);
    handle_terminal_event(
        &mut app,
        picker_key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
    )
    .await
    .expect("ctrl+s");
    assert!(!app.provider_menu_open);
    assert!(
        app.provider_editor.is_some(),
        "Ctrl+S inside the picker opens the same panel"
    );
}

#[tokio::test]
async fn footer_advertises_the_provider_entry_and_drops_the_repeated_mode() {
    let (mut app, _temp) = test_app().await;
    // Pin the key state: the ambient machine may or may not have a real key in
    // its keyring, and this test is about the layout, not about key resolution.
    app.provider_settings = Some(saved_provider_settings(&["openai"]));
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");

    let footer = crate::ui_layout::compute_layout(
        Rect::new(0, 0, 120, 40),
        crate::ui_layout::Density::Wide,
        crate::ui_layout::HeightClass::Normal,
    )
    .footer;
    let hints = picker_row(&terminal, footer.y, 0, 120);
    assert!(
        hints.contains("Ctrl+S") && hints.contains("供应商设置"),
        "the idle footer advertises the provider entry: {hints:?}"
    );

    // The provider pill is a control, so its hit rectangle covers the text that
    // marks it as one, and the footer no longer repeats the mode that the input
    // block's title already shows.
    let control = app.provider_control_rect.expect("provider control");
    let pill = picker_row(&terminal, control.y, control.x, control.right());
    assert!(
        pill.starts_with("⚙ ") && pill.ends_with(" ▾"),
        "the pill marks itself as a control: {pill:?}"
    );
    let secondary = picker_row(&terminal, footer.y.saturating_add(1), 0, 120);
    assert!(
        !secondary.contains("构建"),
        "the mode belongs to the input title only: {secondary:?}"
    );
    assert!(
        secondary.contains(&app.provider_label()) && secondary.contains(app.model_name()),
        "the secondary line still carries the provider and model: {secondary:?}"
    );
}

#[tokio::test]
async fn narrow_footer_keeps_the_provider_control_reachable() {
    let (mut app, _temp) = test_app().await;
    app.provider_settings = Some(saved_provider_settings(&["openai"]));
    // 69 columns is the compact density: the footer drops the model name but
    // must keep the provider control, which used to disappear entirely.
    let backend = TestBackend::new(69, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app
        .provider_control_rect
        .expect("the provider control survives the compact density");
    let pill = picker_row(&terminal, control.y, control.x, control.right());
    assert!(
        pill.contains(&app.provider_label()),
        "the compact pill paints the provider it opens: {pill:?}"
    );
    handle_terminal_event(&mut app, picker_click(control.x, control.y))
        .await
        .expect("open");
    assert!(
        app.provider_menu_open,
        "a compact terminal can still open the provider picker"
    );

    // The unresolved-key warning must not cost the narrow terminal its entry:
    // the compact pill drops the suffix rather than overflowing the budget.
    let (mut app, _temp) = test_app().await;
    app.provider_settings = Some(ProviderSettingsDto {
        active: saved_provider_settings(&["openai"]).active,
        saved: Vec::new(),
        connected: Vec::new(),
    });
    let backend = TestBackend::new(69, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    let control = app
        .provider_control_rect
        .expect("an unresolved key still leaves a clickable provider pill");
    let pill = picker_row(&terminal, control.y, control.x, control.right());
    assert!(
        pill.contains(&app.provider_label()) && pill.starts_with("⚙ ") && pill.ends_with(" ▾"),
        "the compact pill stays whole while the activity line carries the warning: {pill:?}"
    );
}

#[tokio::test]
async fn an_unresolved_key_is_surfaced_once_and_retracted_when_it_resolves() {
    let (mut app, _temp) = test_app().await;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // `build_app` already seeded the core view; make the active provider
    // unresolved and keep the transcript empty, which is the first screen.
    app.provider_settings = Some(ProviderSettingsDto {
        active: saved_provider_settings(&["openai"]).active,
        saved: Vec::new(),
        connected: Vec::new(),
    });
    app.current.entries.clear();
    app.provider_hint_shown = false;
    assert!(app.provider_needs_key());

    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw");
    crate::app::provider::sync_provider_hint(&mut app);
    assert!(app.provider_hint_shown);
    assert_eq!(
        app.current.entries.len(),
        1,
        "the first screen carries exactly one onboarding entry"
    );

    // Idempotent: a later sync never stacks a second copy.
    crate::app::provider::sync_provider_hint(&mut app);
    assert_eq!(app.current.entries.len(), 1);

    // The footer says why the panel matters, instead of leaving a bare "ready".
    let footer = crate::ui_layout::compute_layout(
        Rect::new(0, 0, 120, 40),
        crate::ui_layout::Density::Wide,
        crate::ui_layout::HeightClass::Normal,
    )
    .footer;
    let status = picker_row(&terminal, footer.y, 0, 120);
    assert!(
        status.contains("供应商未配置密钥"),
        "the activity line reports the missing key: {status:?}"
    );

    // Once a key resolves the stale advice must go away on its own.
    app.provider_settings = Some(saved_provider_settings(&["openai"]));
    crate::app::provider::sync_provider_hint(&mut app);
    assert!(
        app.current.entries.is_empty(),
        "the entry is retracted after the key resolves"
    );
}
