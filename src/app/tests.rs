use super::*;
use protium_core::config::Config;
use protium_core::provider::ToolCall;
use ratatui::backend::TestBackend;
use tempfile::tempdir;

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
    let preset = app.active_preset();
    let result = || ModelRefreshResult {
        generation: 1,
        preset,
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
    let preset = app.active_preset();

    apply_model_refresh_result(
        &mut app,
        ModelRefreshResult {
            generation: 1,
            preset,
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
