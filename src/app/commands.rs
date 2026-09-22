use super::*;

impl App {
    pub(super) fn cancel_model_refresh(&mut self) {
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

    pub(super) async fn cancel(&mut self) -> Result<()> {
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

    pub(super) async fn submit_current(&mut self) -> Result<()> {
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

    pub(super) async fn request_shell_approval(&mut self, command: String) -> Result<()> {
        if !self.current.session_id.is_empty() {
            let text = format!("!{command}");
            self.handle
                .submit(Some(self.current.session_id.clone()), &text)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn create_session(&mut self) -> Result<()> {
        self.handle.execute_command(None, "/new").await?;
        self.sync_all().await?;
        self.input.clear();
        Ok(())
    }

    pub(super) async fn switch_session_direction(&mut self, direction: isize) -> Result<()> {
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

    pub(super) async fn activate_session(&mut self, session_id: &str) -> Result<()> {
        if session_id == self.current.session_id && !self.current.session_id.is_empty() {
            self.sync_all().await?;
            return Ok(());
        }
        self.handle.activate_session(session_id).await?;
        self.sync_all().await?;
        Ok(())
    }

    pub(super) async fn switch_mode(&mut self, mode: AgentMode) -> Result<()> {
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

    pub(super) async fn resolve_approval(&mut self, choice: ApprovalChoice) -> Result<()> {
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

    pub(super) async fn apply_provider_choice(&mut self, preset: ProviderPreset) -> Result<()> {
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

    pub(super) async fn apply_model_choice(&mut self, model: String) -> Result<()> {
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
impl App {
    pub(super) async fn execute_command(&mut self, command: Command) -> Result<()> {
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

pub(super) fn command_to_text(command: &Command) -> String {
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

pub(super) fn todo_to_text(todo: &TodoCommand) -> String {
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

pub(super) fn expand_custom_command(app: &App, input: &str) -> Option<String> {
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

pub(super) fn update_file_suggestions(app: &mut App) {
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

pub(super) fn apply_file_completion(app: &mut App) {
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

pub(super) fn open_palette(app: &mut App) {
    app.palette = Some(CommandPaletteState {
        query: String::new(),
        selected: 0,
    });
    app.current.status = "命令面板 | 输入筛选 | ↑/↓ 选择 | Enter 执行 | Esc 关闭".into();
}

pub(super) async fn handle_palette_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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

pub(super) fn next_mode(mode: AgentMode) -> AgentMode {
    match mode {
        AgentMode::Build => AgentMode::Plan,
        AgentMode::Plan => AgentMode::Explore,
        AgentMode::Explore => AgentMode::Cluster,
        AgentMode::Cluster => AgentMode::Build,
    }
}

pub(super) fn session_switch_direction(key: &crossterm::event::KeyEvent) -> Option<i32> {
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

pub(super) fn todo_status_from_wire(status: &str) -> TodoStatus {
    match status {
        "in_progress" => TodoStatus::InProgress,
        "done" => TodoStatus::Done,
        _ => TodoStatus::Pending,
    }
}
