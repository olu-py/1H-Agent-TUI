use super::*;

impl TuiSessionProjection {
    /// Replaces the display history with one rebuilt from a message page, and
    /// drops transient streaming/thinking/scroll state.
    pub fn replace_history(&mut self, entries: Vec<DisplayEntry>) {
        self.entries = entries;
        self.invalidate_output_layout();
        self.markdown_render_cache.clear();
        self.clear_output_selection();
        self.thinking_anchor = None;
        self.scroll_to_bottom();
    }

    /// Applies a message page to the history. `prepend = false` rebuilds the
    /// history head (session start, reconnect, transcript invalidation) and
    /// resets the pagination cursors; `prepend = true` loads the next older
    /// page above the current entries, keeping the visible content anchored so
    /// the view does not jump.
    pub fn apply_message_page(
        &mut self,
        page: &protium_core::protocol::MessagePage,
        prepend: bool,
    ) {
        let entries = Self::message_dto_to_entries(&page.messages);
        self.history_has_more = page.has_more;
        self.history_next_before = page.next_before;
        if prepend && !entries.is_empty() {
            // Preserve the scroll anchor: distance-from-bottom stays constant
            // when older entries are prepended above, so the same content stays
            // in view. The renderer recomputes scroll from `message_scroll`
            // against the grown layout.
            let anchor = self.message_scroll;
            let old = std::mem::take(&mut self.entries);
            let mut merged = entries;
            merged.extend(old);
            self.entries = merged;
            self.invalidate_output_layout();
            self.markdown_render_cache.clear();
            self.clear_output_selection();
            self.output_scroll_top = None;
            self.follow_output = false;
            self.message_scroll = anchor;
            // When the user is paging toward older history, keep the newly
            // loaded older end and evict from the newer end. This prevents a
            // long paging session from growing the projection forever while
            // preserving the content currently being inspected.
            let removed = trim_entries_from_end(&mut self.entries);
            if removed > 0 {
                self.invalidate_output_layout();
                self.markdown_render_cache.clear();
                self.clear_output_selection();
            }
        } else {
            self.replace_history(entries);
        }
    }

    /// True when the view is scrolled to the very top of the loaded history and
    /// older messages still exist — the signal to load the previous page.
    pub fn at_history_top(&self) -> bool {
        if !self.history_has_more {
            return false;
        }
        match &self.message_layout {
            Some(layout) if !self.follow_output => {
                self.output_scroll_top.unwrap_or(layout.scroll) == 0
            }
            _ => false,
        }
    }

    /// Renders (or drops) a persisted incomplete assistant answer reported by
    /// the snapshot. Re-applies only when the content changed, so repeated
    /// snapshot merges do not duplicate the entry.
    pub fn sync_partial(&mut self, partial: Option<&protium_core::protocol::PartialDto>) {
        let current = self
            .entries
            .iter()
            .rev()
            .find(|entry| matches!(entry.kind, DisplayKind::AssistantPartial))
            .and_then(|entry| match &entry.content {
                DisplayContent::Markdown(text) => Some(text.clone()),
                _ => None,
            });
        let incoming = partial.map(|partial| partial.content.clone());
        if current == incoming {
            return;
        }
        self.entries
            .retain(|entry| !matches!(entry.kind, DisplayKind::AssistantPartial));
        if let Some(partial) = partial
            && !partial.content.trim().is_empty()
        {
            self.push_entry(DisplayEntry {
                kind: DisplayKind::AssistantPartial,
                content: DisplayContent::Markdown(partial.content.clone()),
            });
        }
        self.invalidate_output_layout();
    }

    /// Converts a v2 message page into the display list, mirroring the core's
    /// `display_entries` mapping so history restored from the database matches
    /// the live streaming projection.
    pub fn message_dto_to_entries(messages: &[MessageDto]) -> Vec<DisplayEntry> {
        let mut entries = Vec::new();
        let mut tool_entries = HashMap::<String, usize>::new();
        let mut thinking_index = 0usize;
        for message in messages {
            match message {
                MessageDto::User { content, .. } => entries.push(DisplayEntry {
                    kind: DisplayKind::User,
                    content: DisplayContent::Markdown(content.clone()),
                }),
                MessageDto::Assistant { content, .. } => entries.push(DisplayEntry {
                    kind: DisplayKind::Assistant,
                    content: DisplayContent::Markdown(content.clone()),
                }),
                MessageDto::System { content, .. } => entries.push(DisplayEntry {
                    kind: DisplayKind::System,
                    content: DisplayContent::Markdown(content.clone()),
                }),
                MessageDto::Thinking { content, .. } => {
                    let id = format!("thinking-{thinking_index}");
                    thinking_index += 1;
                    entries.push(DisplayEntry {
                        kind: DisplayKind::Thinking,
                        content: DisplayContent::Thinking(ThinkingDisplay {
                            id,
                            content: content.clone(),
                        }),
                    });
                }
                MessageDto::Context { label, content, .. } => entries.push(DisplayEntry {
                    kind: DisplayKind::System,
                    content: DisplayContent::Markdown(format!("### @{label}\n\n{content}")),
                }),
                MessageDto::CompactionSummary { content, .. } => entries.push(DisplayEntry {
                    kind: DisplayKind::System,
                    content: DisplayContent::Markdown(format!("上下文压缩摘要\n\n{content}")),
                }),
                MessageDto::Tool {
                    call_id,
                    name,
                    arguments,
                    status,
                    result,
                    ..
                } => entries.push(DisplayEntry {
                    kind: DisplayKind::Tool,
                    content: DisplayContent::Tool(ToolDisplay {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                        status: status_from_wire(status),
                        result: result.clone(),
                    }),
                }),
                MessageDto::ToolCalls { calls, .. } => {
                    for call in calls {
                        tool_entries.insert(call.id.clone(), entries.len());
                        entries.push(DisplayEntry {
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
                }
                MessageDto::ToolOutput {
                    call_id, output, ..
                } => {
                    if let Some(tool) = tool_entries
                        .get(call_id)
                        .and_then(|index| entries.get_mut(*index))
                        .and_then(|entry| match &mut entry.content {
                            DisplayContent::Tool(tool) => Some(tool),
                            _ => None,
                        })
                    {
                        tool.status = tool_result_status(output);
                        tool.result = Some(output.clone());
                    } else {
                        entries.push(DisplayEntry {
                            kind: DisplayKind::Tool,
                            content: DisplayContent::Tool(ToolDisplay {
                                call_id: call_id.clone(),
                                name: "tool".into(),
                                arguments: serde_json::Value::Null,
                                status: tool_result_status(output),
                                result: Some(output.clone()),
                            }),
                        });
                    }
                }
            }
        }
        if entries.is_empty() {
            entries.push(DisplayEntry {
                kind: DisplayKind::System,
                content: DisplayContent::Markdown("1H-Agent 已就绪，请输入任务并按 Enter。".into()),
            });
        }
        entries
    }
}
