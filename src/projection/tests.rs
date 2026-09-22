use super::*;
use protium_core::conformance::Expectation;
use protium_core::protocol::MessagePage;

#[test]
fn usage_never_mutates_the_authoritative_context_meter() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, Some(1000));
    projection.handle_event(&Event::ContextUpdated {
        budget: ContextBudgetDto {
            context_window_tokens: Some(1000),
            used_tokens: 120,
            output_reserve_tokens: 100,
            safe_input_tokens: Some(780),
            window_source: "registry".into(),
            estimated: true,
        },
    });
    assert_eq!(projection.context_used_tokens, 120);
    assert_eq!(projection.context_limit_tokens, Some(1000));

    // Usage is display-only: the old code did `input_tokens.max(limit)`,
    // which could render a small window as permanently full.
    projection.handle_event(&Event::Usage {
        input_tokens: 40,
        output_tokens: 10,
        total_tokens: 50,
    });
    assert_eq!(projection.context_used_tokens, 120);
    assert_eq!(projection.usage.input_tokens, 40);

    projection.handle_event(&Event::Usage {
        input_tokens: 9000,
        output_tokens: 10,
        total_tokens: 9010,
    });
    assert_eq!(projection.context_used_tokens, 120);
    assert_eq!(projection.usage.input_tokens, 9000);
}

#[test]
fn context_updated_is_the_authoritative_anchor_and_resets_overlay() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::TextDelta {
        delta: "x".repeat(400),
    });
    assert!(projection.context_overlay_tokens > 0);

    projection.handle_event(&Event::ContextUpdated {
        budget: ContextBudgetDto {
            context_window_tokens: Some(2000),
            used_tokens: 250,
            output_reserve_tokens: 100,
            safe_input_tokens: Some(1650),
            window_source: "provider".into(),
            estimated: true,
        },
    });
    assert_eq!(projection.context_used_tokens, 250);
    assert_eq!(projection.context_limit_tokens, Some(2000));
    assert_eq!(projection.context_overlay_tokens, 0);
    let budget = projection.context_budget.as_ref().expect("budget");
    assert_eq!(budget.window_source, "provider");
    assert!(budget.estimated);

    // Terminal states also clear the overlay.
    projection.handle_event(&Event::TextDelta {
        delta: "y".repeat(400),
    });
    assert!(projection.context_overlay_tokens > 0);
    projection.handle_event(&Event::Completed);
    assert_eq!(projection.context_overlay_tokens, 0);
}

/// Replays the core's shared conformance corpus through the projection:
/// every scenario must replay without panicking and must land the
/// projection in the end-state the scenario declares. When the core adds
/// an `Event` variant and a scenario for it, this test consumes the new
/// scenario automatically - the projection must keep up or this fails.
#[test]
fn conformance_corpus_replays_into_projection() {
    for scenario in protium_core::conformance::scenarios() {
        let mut projection = TuiSessionProjection::new(
            protium_core::conformance::SESSION_ID.to_owned(),
            AgentMode::Build,
            None,
        );
        for envelope in &scenario.envelopes {
            projection.handle_event(&envelope.event);
        }
        match scenario.expectation {
            Expectation::Completed | Expectation::Cancelled => {
                assert_eq!(
                    projection.agent_phase,
                    AgentPhase::Idle,
                    "scenario {}: terminal expectation must return to idle",
                    scenario.name
                );
                assert!(
                    !projection.thinking_active,
                    "scenario {}: terminal expectation must commit the thinking row",
                    scenario.name
                );
            }
            Expectation::Failed => {
                assert_eq!(
                    projection.agent_phase,
                    AgentPhase::Failed,
                    "scenario {}: failed expectation",
                    scenario.name
                );
                assert!(
                    !projection.thinking_active,
                    "scenario {}: failure must commit the thinking row",
                    scenario.name
                );
            }
            Expectation::ApprovalPending => {
                assert_eq!(
                    projection.agent_phase,
                    AgentPhase::WaitingApproval,
                    "scenario {}: approval expectation",
                    scenario.name
                );
                assert!(
                    projection.pending_approval.is_some(),
                    "scenario {}: approval must be visible to the consumer",
                    scenario.name
                );
            }
            Expectation::Active => {
                assert_ne!(
                    projection.agent_phase,
                    AgentPhase::Idle,
                    "scenario {}: active expectation must not be idle",
                    scenario.name
                );
            }
            Expectation::Resync => {
                // Transport-level signal: the projection itself stays
                // untouched; only the facade refetches state.
            }
        }
    }
}

#[test]
fn message_page_rebuilds_entry_history() {
    let messages = vec![
        MessageDto::User {
            id: 1,
            content: "hi".into(),
            created_at: "t".into(),
        },
        MessageDto::Assistant {
            id: 2,
            content: "hello".into(),
            created_at: "t".into(),
        },
        MessageDto::Thinking {
            id: 3,
            content: "reasoning".into(),
            created_at: "t".into(),
        },
    ];
    let entries = TuiSessionProjection::message_dto_to_entries(&messages);
    assert_eq!(entries.len(), 3);
    assert!(matches!(entries[0].kind, DisplayKind::User));
    assert!(matches!(entries[1].kind, DisplayKind::Assistant));
    assert!(matches!(entries[2].kind, DisplayKind::Thinking));
}

fn message_page(ids: &[i64], next_before: Option<i64>, has_more: bool) -> MessagePage {
    MessagePage {
        messages: ids
            .iter()
            .map(|&id| MessageDto::User {
                id,
                content: format!("msg {id}"),
                created_at: "t".into(),
            })
            .collect(),
        next_before,
        has_more,
    }
}

#[test]
fn apply_message_page_tracks_pagination_and_prepends_older() {
    fn user_text(entry: &DisplayEntry) -> &str {
        match &entry.content {
            DisplayContent::Markdown(text) => text,
            other => panic!("expected markdown entry, got {other:?}"),
        }
    }
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    // Fresh head page: newest 2 messages, older exist.
    projection.apply_message_page(&message_page(&[10, 11], Some(9), true), false);
    assert_eq!(projection.entries.len(), 2);
    assert_eq!(user_text(&projection.entries[0]), "msg 10");
    assert!(projection.history_has_more);
    assert_eq!(projection.history_next_before, Some(9));
    // Load the older page: prepended in front, cursors advance.
    projection.apply_message_page(&message_page(&[7, 8, 9], Some(6), true), true);
    assert_eq!(projection.entries.len(), 5);
    assert_eq!(user_text(&projection.entries[0]), "msg 7");
    assert_eq!(user_text(&projection.entries[4]), "msg 11");
    assert!(projection.history_has_more);
    assert_eq!(projection.history_next_before, Some(6));
    // Head exhausted.
    projection.apply_message_page(&message_page(&[1, 2, 3, 4, 5, 6], None, false), true);
    assert_eq!(projection.entries.len(), 11);
    assert!(!projection.history_has_more);
    assert_eq!(projection.history_next_before, None);
}

#[test]
fn apply_message_page_preserves_scroll_anchor_on_prepend() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.apply_message_page(&message_page(&[10, 11], None, false), false);
    // Simulate the user having scrolled up (distance from bottom > 0).
    projection.message_scroll = 3;
    projection.follow_output = false;
    projection.output_scroll_top = Some(2);
    projection.apply_message_page(&message_page(&[7, 8, 9], None, false), true);
    // The anchor is preserved: distance-from-bottom unchanged and the
    // absolute top position recomputed by the renderer, not kept stale.
    assert_eq!(projection.message_scroll, 3);
    assert!(!projection.follow_output);
    assert_eq!(projection.output_scroll_top, None);
}

#[test]
fn at_history_top_requires_more_history_and_top_scroll() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.apply_message_page(&message_page(&[10, 11], Some(9), true), false);
    assert!(!projection.at_history_top(), "no layout yet");
    projection.message_layout = Some(MessageLayout {
        viewport: ratatui::layout::Rect::new(0, 0, 80, 24),
        width: 80,
        scroll: 1,
        text: String::new(),
        lines: Vec::new(),
        visual_lines: Vec::new(),
        live_thinking_before: None,
        live_thinking_rows: 0,
        live_thinking_lines: Vec::new(),
        live_thinking_clickable: true,
    });
    projection.output_scroll_top = Some(0);
    projection.follow_output = false;
    assert!(projection.at_history_top());
    projection.output_scroll_top = Some(5);
    assert!(!projection.at_history_top());
    projection.history_has_more = false;
    projection.output_scroll_top = Some(0);
    assert!(!projection.at_history_top(), "no more history");
}

#[test]
fn text_delta_appends_to_live_assistant_entry() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    projection.handle_event(&Event::TextDelta {
        delta: "hel".into(),
    });
    projection.handle_event(&Event::TextDelta { delta: "lo".into() });
    assert_eq!(projection.entries.len(), 1);
    assert!(matches!(projection.entries[0].kind, DisplayKind::Assistant));
    assert!(matches!(&projection.entries[0].content, DisplayContent::Markdown(t) if t == "hello"));
}

#[test]
fn streaming_deltas_require_a_repaint() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    // Every streaming delta must ask the facade for a (coalesced) repaint so
    // the answer and reasoning are visibly rendered as they arrive instead
    // of only appearing once the response completes.
    assert!(projection.handle_event(&Event::ModelStreaming).force_redraw);
    assert!(
        projection
            .handle_event(&Event::ReasoningDelta {
                delta: "reasoning".into(),
            })
            .force_redraw
    );
    assert!(
        projection
            .handle_event(&Event::TextDelta {
                delta: "answer".into(),
            })
            .force_redraw
    );
}

#[test]
fn tool_and_search_and_shell_events_force_a_repaint() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    let call = ToolCall {
        id: "c1".into(),
        name: "file_read".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    // Tool start and finish must both request a repaint: the card leaves
    // "Running" and shows "正在将工具结果交给模型……" on its own frame,
    // even when no model event has arrived after the tool ended.
    assert!(
        projection
            .handle_event(&Event::ToolStarted { call: call.clone() })
            .force_redraw
    );
    assert!(
        projection
            .handle_event(&Event::ToolFinished {
                call,
                result: "ok".into(),
            })
            .force_redraw
    );
    // Native web search start and completion are visible transitions too.
    assert!(
        projection
            .handle_event(&Event::WebSearchStarted {
                query: "rust async".into(),
            })
            .force_redraw
    );
    assert!(
        projection
            .handle_event(&Event::WebSearchCompleted { count: 3 })
            .force_redraw
    );
    // A finished local shell command immediately releases the busy state
    // and must be drawn (including the /diff early-return branch).
    assert!(
        projection
            .handle_event(&Event::LocalCommandFinished {
                command: "!echo hi".into(),
                result: "hi".into(),
            })
            .force_redraw
    );
    assert!(
        projection
            .handle_event(&Event::LocalCommandFinished {
                command: "/diff".into(),
                result: "--- a\n+++ b".into(),
            })
            .force_redraw
    );
}

#[test]
fn reasoning_completed_persists_summary_and_clears_live_state() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    projection.handle_event(&Event::ReasoningDelta {
        delta: "推演过程".into(),
    });
    assert!(projection.thinking_active);
    let outcome = projection.handle_event(&Event::ReasoningCompleted);
    assert!(
        outcome.force_redraw,
        "the phase barrier must request a repaint"
    );
    assert!(
        !projection.thinking_active,
        "live thinking retires at the barrier"
    );
    assert!(projection.thinking_buffer.is_empty());
    assert_eq!(projection.agent_phase, AgentPhase::StreamingText);
    assert_eq!(
        projection
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, DisplayKind::Thinking))
            .count(),
        1,
        "the reasoning is persisted as a thinking summary"
    );
}

#[test]
fn text_delta_after_reasoning_completed_only_appends_body() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    projection.handle_event(&Event::ReasoningDelta {
        delta: "推演过程".into(),
    });
    projection.handle_event(&Event::ReasoningCompleted);
    projection.handle_event(&Event::TextDelta {
        delta: "正文".into(),
    });
    projection.handle_event(&Event::TextDelta {
        delta: "继续".into(),
    });
    let summaries = projection
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, DisplayKind::Thinking))
        .count();
    assert_eq!(summaries, 1, "body deltas must not re-persist a summary");
    assert!(
        matches!(projection.entries.last().map(|entry| &entry.content),
        Some(DisplayContent::Markdown(text)) if text == "正文继续")
    );
}

#[test]
fn thinking_autexpands_while_streaming_then_summary_collapses_when_done() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    assert!(
        projection.thinking_expanded,
        "live thinking should be expanded so reasoning is visible while streaming"
    );
    projection.handle_event(&Event::ReasoningDelta {
        delta: "推演过程".into(),
    });
    projection.handle_event(&Event::ReasoningCompleted);
    projection.handle_event(&Event::TextDelta {
        delta: "正文".into(),
    });
    let thinking = projection
        .entries
        .iter()
        .find_map(|entry| match &entry.content {
            DisplayContent::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("a thinking summary should be persisted when thinking ends");
    assert!(
        !projection.expanded_thinking.contains(&thinking.id),
        "the finished summary should be collapsed by default (auto-collapse)"
    );
    // The user can still re-expand it by clicking the summary.
    projection.expanded_thinking.insert(thinking.id.clone());
    assert!(projection.expanded_thinking.contains(&thinking.id));
}

#[test]
fn tool_lifecycle_renders_a_card_and_finishes() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    let call = ToolCall {
        id: "c1".into(),
        name: "file_read".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    projection.handle_event(&Event::ToolStarted { call: call.clone() });
    projection.handle_event(&Event::ToolFinished {
        call,
        result: "ok".into(),
    });
    assert_eq!(projection.entries.len(), 1);
    assert!(matches!(
        &projection.entries[0].content,
        DisplayContent::Tool(tool) if tool.status == ToolDisplayStatus::Completed && tool.result.as_deref() == Some("ok")
    ));
}

#[test]
fn tool_started_is_idempotent_after_history_rebuild() {
    fn tool_cards(projection: &TuiSessionProjection) -> Vec<&ToolDisplay> {
        projection
            .entries
            .iter()
            .filter_map(|entry| match &entry.content {
                DisplayContent::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect()
    }
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    let call = ToolCall {
        id: "c1".into(),
        name: "file_read".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    // A transcript rebuild (approval resolution or a resync replay)
    // materializes the persisted tool call as a Running card.
    projection.apply_message_page(
        &MessagePage {
            messages: vec![MessageDto::ToolCalls {
                id: 1,
                calls: vec![call.clone()],
                created_at: "t".into(),
            }],
            next_before: None,
            has_more: false,
        },
        false,
    );
    assert_eq!(projection.entries.len(), 1);
    // The runner re-emits ToolStarted after the rebuild: it must reuse the
    // existing card, not insert a duplicate.
    projection.handle_event(&Event::ToolStarted { call: call.clone() });
    let cards = tool_cards(&projection);
    assert_eq!(
        cards.len(),
        1,
        "replayed ToolStarted must not duplicate the card"
    );
    assert_eq!(cards[0].call_id, "c1");
    assert_eq!(cards[0].status, ToolDisplayStatus::Running);
    // ToolFinished still lands on that single card.
    projection.handle_event(&Event::ToolFinished {
        call,
        result: "ok".into(),
    });
    let cards = tool_cards(&projection);
    assert_eq!(cards.len(), 1, "ToolFinished must not add a card either");
    assert_eq!(cards[0].status, ToolDisplayStatus::Completed);
    assert_eq!(cards[0].result.as_deref(), Some("ok"));
}

#[test]
fn approval_event_is_display_only() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    let call = ToolCall {
        id: "c1".into(),
        name: "file_write".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    projection.handle_event(&Event::Approval {
        approval_id: "ap1".into(),
        call: call.clone(),
        reason: "need".into(),
        source_session_id: None,
        source_title: None,
    });
    assert!(projection.pending_approval.is_some());
    projection.handle_event(&Event::ApprovalResolved {
        approval_id: "ap1".into(),
        approved: true,
    });
    assert!(projection.pending_approval.is_none());
}

#[test]
fn reasoning_resumes_after_history_replace_without_losing_the_buffer() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    projection.handle_event(&Event::ReasoningDelta {
        delta: "前半".into(),
    });
    assert!(projection.thinking_active);
    assert_eq!(projection.thinking_anchor, Some(0));
    // A transcript sync rebuilds history mid-reasoning: the live anchor is
    // dropped but the buffered reasoning survives.
    projection.replace_history(vec![]);
    assert!(projection.thinking_anchor.is_none());
    assert_eq!(projection.thinking_buffer, "前半");
    // The next reasoning delta must restore the anchor/active state while
    // keeping the already-buffered reasoning.
    projection.handle_event(&Event::ReasoningDelta {
        delta: "后半".into(),
    });
    assert!(projection.thinking_active);
    assert_eq!(projection.thinking_anchor, Some(0));
    assert_eq!(projection.thinking_buffer, "前半后半");
    assert!(
        projection
            .handle_event(&Event::ReasoningDelta {
                delta: "结尾".into(),
            })
            .force_redraw
    );
}

#[test]
fn tool_call_streaming_collapses_thinking_and_clears_on_tool_start() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ModelStreaming);
    projection.handle_event(&Event::ReasoningDelta {
        delta: "推演过程".into(),
    });
    // The reasoning is still live when the tool call starts streaming; it
    // must collapse into exactly one summary and hand the live slot to the
    // "generating tool call" row.
    projection.handle_event(&Event::ToolCallStreaming {
        name: Some("file_write".into()),
        received_bytes: 9216,
    });
    assert_eq!(projection.agent_phase, AgentPhase::StreamingToolCall);
    assert_eq!(projection.model_phase, ModelPhase::Streaming);
    assert!(projection.thinking_active);
    assert!(!projection.thinking_expanded);
    assert_eq!(
        projection.generating_tool,
        Some(("文件修改".to_owned(), 9216))
    );
    assert!(projection.status.contains("文件修改"));
    assert!(projection.status.contains("9.0 KB"));
    assert_eq!(
        projection
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, DisplayKind::Thinking))
            .count(),
        1,
        "the live reasoning collapses into exactly one summary"
    );
    // A later ToolStarted clears the generating row (the next phase ends it).
    projection.handle_event(&Event::ToolStarted {
        call: ToolCall {
            id: "c1".into(),
            name: "file_write".into(),
            arguments: serde_json::json!({"path": "a.txt"}),
        },
    });
    assert!(projection.generating_tool.is_none());
    assert!(!projection.thinking_active);
    assert_eq!(projection.agent_phase, AgentPhase::ToolRunning);
}

#[test]
fn tool_call_streaming_with_unknown_name_uses_a_generic_label() {
    let mut projection = TuiSessionProjection::new("s1".into(), AgentMode::Build, None);
    projection.handle_event(&Event::ToolCallStreaming {
        name: None,
        received_bytes: 512,
    });
    assert_eq!(
        projection.generating_tool,
        Some(("工具调用".to_owned(), 512))
    );
    assert!(projection.status.contains("512 B"));
    assert!(projection.thinking_anchor.is_some());
}
