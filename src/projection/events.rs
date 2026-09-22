use super::*;

impl TuiSessionProjection {
    pub fn handle_event(&mut self, event: &Event) -> ProjectionOutcome {
        let mut outcome = ProjectionOutcome::default();
        match event {
            Event::ReasoningDelta { delta } => {
                self.add_context_overlay(delta);
                self.agent_phase = AgentPhase::Thinking;
                self.model_phase = ModelPhase::Streaming;
                // A mid-reasoning sync (replace_history via ApprovalResolved or
                // a lag resync) drops the live anchor but preserves the buffer.
                // Re-anchor so the live row is not lost forever: begin fresh
                // when the buffer is empty, otherwise keep the buffered
                // reasoning and just restore the anchor/active state.
                if self.thinking_anchor.is_none()
                    && (self.thinking_active || !self.thinking_buffer.is_empty())
                {
                    if self.thinking_buffer.is_empty() {
                        self.begin_thinking();
                    } else {
                        self.thinking_active = true;
                        self.thinking_anchor = Some(self.entries.len());
                    }
                }
                self.update_thinking_line(delta);
                outcome.force_redraw = true;
            }
            Event::ReasoningCompleted => {
                // Phase barrier: persist the current reasoning as a summary,
                // retire the live thinking row, and switch to the body phase.
                // The facade draws this frame immediately (not coalesced) so the
                // user sees only the finished thinking before the answer starts.
                self.finish_thinking("思考完成");
                self.agent_phase = AgentPhase::StreamingText;
                self.model_phase = ModelPhase::Streaming;
                self.status = "正在输出正文…… | Esc 取消".into();
                outcome.force_redraw = true;
            }
            Event::ModelStreaming => {
                self.begin_thinking();
                self.agent_phase = AgentPhase::Thinking;
                self.model_phase = ModelPhase::Streaming;
                self.status = "等待模型流式响应".into();
                outcome.force_redraw = true;
            }
            Event::ProviderRetry {
                attempt,
                reason,
                delay_ms,
            } => {
                let delay_seconds = delay_ms.div_ceil(1000);
                self.status =
                    format!("请求失败，{delay_seconds} 秒后第 {attempt} 次重试（{reason}）");
            }
            Event::TodoUpdated { tasks } => {
                self.set_todos(tasks.clone());
            }
            Event::CompactionStarted => {
                self.status = "正在压缩上下文…… | Esc 取消".into();
                self.agent_phase = AgentPhase::Thinking;
                self.model_phase = ModelPhase::Streaming;
            }
            Event::CompactionCompleted { hidden } => {
                self.context_overlay_tokens = 0;
                self.status = format!("上下文已压缩，隐藏 {hidden} 条历史消息");
            }
            Event::CompactionFailed { error } => {
                self.context_overlay_tokens = 0;
                self.status = format!("上下文压缩失败，已使用安全裁剪：{error}");
                self.push_entry(DisplayEntry {
                    kind: DisplayKind::Error,
                    content: DisplayContent::Markdown(self.status.clone()),
                });
            }
            Event::WebSearchStarted { query } => {
                self.finish_thinking("思考完成");
                self.agent_phase = AgentPhase::ToolRunning;
                self.model_phase = ModelPhase::Streaming;
                outcome.force_redraw = true;
                let already_open = self.entries.last().is_some_and(|entry| {
                    matches!(&entry.content, DisplayContent::Tool(tool) if tool.name == "web_search" && tool.status == ToolDisplayStatus::Running)
                });
                if !already_open {
                    let call_id = format!("native-web-search-{}", uuid::Uuid::new_v4());
                    self.push_entry(DisplayEntry {
                        kind: DisplayKind::Tool,
                        content: DisplayContent::Tool(ToolDisplay {
                            call_id,
                            name: "web_search".into(),
                            arguments: serde_json::json!({"query": query}),
                            status: ToolDisplayStatus::Running,
                            result: None,
                        }),
                    });
                }
                self.status = "正在联网搜索".into();
            }
            Event::WebSearchResult {
                title,
                url,
                snippet,
            } => {
                let context = format!("{title}\n{url}\n{snippet}");
                if let Some(tool) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find_map(|entry| match &mut entry.content {
                        DisplayContent::Tool(tool)
                            if tool.name == "web_search"
                                && tool.status == ToolDisplayStatus::Running =>
                        {
                            Some(tool)
                        }
                        _ => None,
                    })
                {
                    let result = tool.result.get_or_insert_with(String::new);
                    if !result.is_empty() {
                        result.push_str("\n\n");
                    }
                    result.push_str(&context);
                }
            }
            Event::WebSearchCompleted { count } => {
                if let Some(tool) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find_map(|entry| match &mut entry.content {
                        DisplayContent::Tool(tool)
                            if tool.name == "web_search"
                                && tool.status == ToolDisplayStatus::Running =>
                        {
                            Some(tool)
                        }
                        _ => None,
                    })
                {
                    tool.status = ToolDisplayStatus::Completed;
                    self.invalidate_output_layout();
                }
                self.agent_phase = AgentPhase::Thinking;
                self.status = if *count == 0 {
                    "联网搜索完成".into()
                } else {
                    format!("联网搜索完成：{count} 条结果")
                };
                // The search card terminal state must be visible even before
                // the next model round arrives.
                outcome.force_redraw = true;
            }
            Event::Cancelled { reason } => {
                self.context_overlay_tokens = 0;
                self.finish_thinking("思考已取消");
                self.mark_partial_if_streaming();
                self.busy = false;
                if self.pending_approval.is_some() {
                    outcome.force_redraw = true;
                }
                self.pending_approval = None;
                self.agent_phase = AgentPhase::Idle;
                self.model_phase = ModelPhase::Idle;
                self.status = if reason.contains("approval") {
                    "审批等待已取消".into()
                } else {
                    "请求已取消".into()
                };
            }
            Event::TextDelta { delta } => {
                self.add_context_overlay(delta);
                self.finish_thinking("思考完成");
                self.agent_phase = AgentPhase::StreamingText;
                self.model_phase = ModelPhase::Streaming;
                self.invalidate_output_layout();
                if let Some(entry) = self.entries.last_mut()
                    && matches!(
                        entry.kind,
                        DisplayKind::Assistant | DisplayKind::AssistantPartial
                    )
                    && let DisplayContent::Markdown(text) = &mut entry.content
                {
                    text.push_str(delta);
                } else {
                    self.push_entry(DisplayEntry {
                        kind: DisplayKind::Assistant,
                        content: DisplayContent::Markdown(delta.clone()),
                    });
                }
                self.status = "正在输出正文…… | Esc 取消".into();
                outcome.force_redraw = true;
            }
            Event::ToolCallStreaming {
                name,
                received_bytes,
            } => {
                // The model is streaming a tool call's arguments (a large
                // file_write payload can take seconds). Collapse any still-live
                // reasoning into its summary exactly once, then show an animated
                // "generating tool call" row so the screen never freezes.
                if self.thinking_active || !self.thinking_buffer.is_empty() {
                    self.finish_thinking("思考完成");
                }
                self.thinking_active = true;
                self.thinking_expanded = false;
                self.thinking_anchor = self.thinking_anchor.or(Some(self.entries.len()));
                self.agent_phase = AgentPhase::StreamingToolCall;
                self.model_phase = ModelPhase::Streaming;
                let display_name = name
                    .as_deref()
                    .map(tool_display_name)
                    .or_else(|| self.generating_tool.as_ref().map(|(name, _)| name.clone()))
                    .unwrap_or_else(|| "工具调用".to_owned());
                self.generating_tool = Some((display_name.clone(), *received_bytes));
                self.status = format!(
                    "正在生成 {display_name} 调用参数（{}）……",
                    format_bytes(*received_bytes)
                );
                // Low-frequency event: draw this frame immediately (the default
                // coalesce branch keeps it out of the 16ms window).
                outcome.force_redraw = true;
            }
            Event::Approval {
                approval_id,
                call,
                reason,
                source_session_id,
                source_title,
            } => {
                self.finish_thinking("思考完成");
                self.agent_phase = AgentPhase::WaitingApproval;
                self.model_phase = ModelPhase::Idle;
                self.status = "需要确认工具权限".into();
                self.pending_approval = Some(ApprovalDisplay {
                    approval_id: approval_id.clone(),
                    call: call.clone(),
                    reason: reason.clone(),
                    source_session_id: source_session_id.clone(),
                    source_title: source_title.clone(),
                    created_at: Instant::now(),
                });
            }
            Event::ApprovalResolved { approval_id, .. } => {
                if self
                    .pending_approval
                    .as_ref()
                    .is_some_and(|approval| approval.approval_id == *approval_id)
                {
                    self.pending_approval = None;
                    outcome.force_redraw = true;
                }
            }
            Event::ToolStarted { call } => {
                self.finish_thinking("思考完成");
                self.agent_phase = AgentPhase::ToolRunning;
                self.model_phase = ModelPhase::Idle;
                self.status = format!("正在执行 {}……", tool_display_name(&call.name));
                // A history rebuild (approval resolution or a resync replay) may
                // already have materialized this call from the persisted
                // transcript, so ToolStarted must be idempotent per call_id:
                // reuse the existing card (refresh name/arguments, back to
                // Running) instead of inserting a duplicate.
                if let Some(tool) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find_map(|entry| match &mut entry.content {
                        DisplayContent::Tool(tool) if tool.call_id == call.id => Some(tool),
                        _ => None,
                    })
                {
                    tool.name = call.name.clone();
                    tool.arguments = call.arguments.clone();
                    tool.status = ToolDisplayStatus::Running;
                    tool.result = None;
                    self.invalidate_output_layout();
                } else {
                    self.push_entry(DisplayEntry {
                        kind: DisplayKind::Tool,
                        content: DisplayContent::Tool(ToolDisplay {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            status: ToolDisplayStatus::Running,
                            result: None,
                        }),
                    });
                }
                outcome.force_redraw = true;
            }
            Event::ToolFinished { call, result } => {
                self.agent_phase = AgentPhase::Thinking;
                let status = tool_result_status(result);
                if let Some(tool) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .find_map(|entry| match &mut entry.content {
                        DisplayContent::Tool(tool) if tool.call_id == call.id => Some(tool),
                        _ => None,
                    })
                {
                    tool.status = status;
                    tool.result = Some(result.clone());
                    self.invalidate_output_layout();
                } else {
                    self.push_entry(DisplayEntry {
                        kind: DisplayKind::Tool,
                        content: DisplayContent::Tool(ToolDisplay {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            status,
                            result: Some(result.clone()),
                        }),
                    });
                }
                self.status = "正在将工具结果交给模型……".into();
                // Terminal tool state must be drawn immediately: the card leaves
                // "Running" even when the next model response has not arrived.
                outcome.force_redraw = true;
            }
            Event::Usage {
                input_tokens,
                output_tokens,
                total_tokens,
            } => {
                // Usage is display-only: the context meter is anchored to the
                // core's ContextUpdated event and must not jump on usage
                // (the old `input_tokens.max(limit)` could even show a small
                // window as permanently full).
                self.usage = Usage {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    total_tokens: *total_tokens,
                };
            }
            Event::Completed => {
                self.context_overlay_tokens = 0;
                self.finish_thinking("思考完成");
                self.busy = false;
                self.agent_phase = AgentPhase::Idle;
                self.model_phase = ModelPhase::Completed;
                self.status = "就绪".into();
                outcome.sessions_dirty = true;
                outcome.transcript_dirty = true;
            }
            Event::SessionsChanged => {
                if !self.busy {
                    self.status = "会话列表已更新".into();
                }
                outcome.sessions_dirty = true;
            }
            Event::ChildSessionProgress { .. } => {}
            Event::Failed { error } => {
                self.context_overlay_tokens = 0;
                self.finish_thinking("思考失败");
                self.mark_partial_if_streaming();
                self.push_entry(DisplayEntry {
                    kind: DisplayKind::Error,
                    content: DisplayContent::Markdown(secrets::redact(error)),
                });
                self.busy = false;
                self.agent_phase = AgentPhase::Failed;
                self.model_phase = ModelPhase::Failed;
                self.status = "请求失败".into();
            }
            Event::LocalCommandFinished { command, result } => {
                outcome.force_redraw = true;
                if command == "/diff" {
                    self.push_entry(DisplayEntry {
                        kind: DisplayKind::Tool,
                        content: DisplayContent::Diff(result.clone()),
                    });
                    self.busy = false;
                    self.agent_phase = AgentPhase::Idle;
                    self.model_phase = ModelPhase::Completed;
                    self.status = "Git diff 已准备好".into();
                    self.trim_entries();
                    return outcome;
                }
                self.push_entry(DisplayEntry {
                    kind: DisplayKind::Tool,
                    content: DisplayContent::Tool(ToolDisplay {
                        call_id: format!("local-shell-{}", uuid::Uuid::new_v4()),
                        name: "terminal_shell".into(),
                        arguments: serde_json::json!({"command": command}),
                        status: tool_result_status(result),
                        result: Some(result.clone()),
                    }),
                });
                self.busy = false;
                self.agent_phase = AgentPhase::Idle;
                self.model_phase = ModelPhase::Completed;
                self.status = "Shell 命令已完成".into();
            }
            Event::TranscriptInvalidated => {
                outcome.transcript_dirty = true;
            }
            Event::ContextUpdated { budget } => {
                // The authoritative anchor: replace the core budget and drop
                // any local streaming overlay.
                self.context_budget = Some(budget.clone());
                self.context_limit_tokens = budget.context_window_tokens;
                self.context_used_tokens = budget.used_tokens;
                self.context_overlay_tokens = 0;
                outcome.force_redraw = true;
            }
            Event::ResyncRequired => {
                outcome.sessions_dirty = true;
                outcome.transcript_dirty = true;
            }
        }
        self.trim_entries();
        outcome
    }

    /// Adds a bounded coarse estimate of streamed bytes to the display-only
    /// context overlay. The core remains the authority; this only keeps the
    /// meter responsive between `ContextUpdated` anchors.
    fn add_context_overlay(&mut self, delta: &str) {
        let estimate = (delta.len() as u64).div_ceil(4).max(1);
        self.context_overlay_tokens = self
            .context_overlay_tokens
            .saturating_add(estimate)
            .min(1_000_000);
    }

    fn begin_thinking(&mut self) {
        self.invalidate_output_layout();
        self.generating_tool = None;
        self.thinking_active = true;
        self.thinking_last_line = "模型正在思考".into();
        self.thinking_buffer.clear();
        self.thinking_buffer_truncated = false;
        self.thinking_buffer_epoch = self.thinking_buffer_epoch.wrapping_add(1);
        self.live_thinking_layout_cache.clear();
        self.thinking_animation_frame = 0;
        self.thinking_anchor = Some(self.entries.len());
        // Auto-expand the live thinking so the reasoning is visible on screen
        // as it streams, instead of hiding behind a one-line spinner and only
        // surfacing as a finished summary once thinking ends.
        self.thinking_expanded = true;
    }

    pub fn finish_thinking(&mut self, line: &str) {
        self.generating_tool = None;
        self.thinking_active = false;
        self.thinking_animation_frame = 0;
        self.persist_thinking_summary();
        match line {
            "思考失败" => self.thinking_result = ThinkingResult::Failed,
            "思考已取消" => self.thinking_result = ThinkingResult::Cancelled,
            _ => self.thinking_result = ThinkingResult::Completed,
        }
    }

    /// Turns the buffered reasoning into a persistent thinking summary entry
    /// and retires the live row.
    fn persist_thinking_summary(&mut self) {
        let truncated = self.thinking_buffer_truncated;
        let reasoning = self.thinking_buffer.trim().to_owned();
        self.thinking_buffer.clear();
        self.thinking_last_line.clear();
        self.thinking_anchor = None;
        self.thinking_buffer_truncated = false;
        self.thinking_buffer_epoch = self.thinking_buffer_epoch.wrapping_add(1);
        self.live_thinking_layout_cache.clear();
        if reasoning.is_empty() {
            return;
        }
        let content = if truncated {
            format!("[较早思考内容已截断]\n\n{reasoning}")
        } else {
            reasoning
        };
        let id = format!("thinking-{}", uuid::Uuid::new_v4());
        self.push_entry(DisplayEntry {
            kind: DisplayKind::Thinking,
            content: DisplayContent::Thinking(ThinkingDisplay {
                id: id.clone(),
                content,
            }),
        });
        // The finished summary starts collapsed (auto-collapse when thinking
        // ends, per the UI contract) so the body answer takes the visual
        // foreground; the user can still click the summary to re-expand it.
    }

    pub fn reset_thinking_state(&mut self) {
        self.generating_tool = None;
        self.thinking_active = false;
        self.thinking_last_line.clear();
        self.thinking_buffer.clear();
        self.thinking_buffer_truncated = false;
        self.thinking_buffer_epoch = self.thinking_buffer_epoch.wrapping_add(1);
        self.live_thinking_layout_cache.clear();
        self.thinking_animation_frame = 0;
        self.thinking_anchor = None;
        self.thinking_result = ThinkingResult::Completed;
    }

    fn update_thinking_line(&mut self, delta: &str) {
        self.generating_tool = None;
        self.thinking_active = true;
        self.thinking_buffer.push_str(delta);
        if self.thinking_buffer.len() > MAX_THINKING_BUFFER_BYTES {
            let minimum = self
                .thinking_buffer
                .len()
                .saturating_sub(MAX_THINKING_BUFFER_BYTES);
            let start = self
                .thinking_buffer
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .find(|offset| *offset >= minimum)
                .unwrap_or(self.thinking_buffer.len());
            self.thinking_buffer.drain(..start);
            self.thinking_buffer_truncated = true;
            self.thinking_buffer_epoch = self.thinking_buffer_epoch.wrapping_add(1);
        }
        let latest = self
            .thinking_buffer
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("思考中");
        self.thinking_last_line = utf8_tail(latest, MAX_THINKING_LINE_BYTES).to_owned();
    }
}
