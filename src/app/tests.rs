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
            preset: (*preset).to_owned(),
            kind: "chat_completions".into(),
            model: format!("{preset}-model"),
            base_url: "https://example.invalid/v1".into(),
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
        preset: Some(ProviderPreset::OpenAi),
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
    for row in inner.y..inner.bottom() {
        let text = picker_row(&terminal, row, inner.x, inner.right());
        let preset = crate::app::provider::provider_menu_selection(&app, picker, inner.x + 2, row)
            .unwrap_or_else(|| panic!("row {row} paints {text:?} but resolves no provider"));
        assert!(
            text.contains(preset.label()),
            "row {row} paints {text:?} but the click resolves {}",
            preset.label()
        );
    }

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
}
