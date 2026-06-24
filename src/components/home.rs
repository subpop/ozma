use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
};

use crate::action::{Action, ParamKind};
use crate::components::Component;
use crate::components::command_palette::CommandPalette;
use crate::components::conversation_view::ConversationView;
use crate::components::input::Input;
use crate::components::model_picker::{ModelPicker, ModelSourceConfig};
use crate::components::param_input::ParamInput;
use crate::components::persona_picker::PersonaPicker;
use crate::components::session_list::SessionList;
use crate::config::ModelParams;
use crate::model::conversation::Conversation;
use crate::persona::Persona;
use crate::store::Store;
use crate::tools::PermissionLevel;
use crate::ui::theme;

/// A pending tool permission request shown to the user.
#[derive(Clone, Debug)]
struct PendingPermission {
    call_id: String,
    tool_name: String,
    arguments: serde_json::Value,
    permission_level: PermissionLevel,
}

/// The home component is the main chat view. It owns:
/// - The active conversation model
/// - The conversation view (message display)
/// - The text input
/// - The session list overlay
/// - A status bar
pub struct Home<'a> {
    /// The active conversation.
    pub conversation: Conversation,
    /// The scrollable message view.
    conversation_view: ConversationView,
    /// The text input component.
    input: Input<'a>,
    /// The session list overlay.
    session_list: SessionList,
    /// The command palette overlay.
    command_palette: CommandPalette,
    /// The model picker overlay.
    model_picker: ModelPicker,
    /// The persona picker overlay.
    persona_picker: PersonaPicker,
    /// The parameter input overlay.
    param_input: ParamInput,
    /// Model params from config (provider-level defaults).
    config_model_params: ModelParams,
    /// Whether a streaming response is in progress.
    streaming: bool,
    /// Whether a compaction is in progress.
    compacting: bool,
    /// Whether tools are being executed.
    executing_tools: bool,
    /// Provider display name (shown in status bar).
    provider_name: String,
    /// Model display name (shown in status bar).
    model_name: String,
    /// Pending tool permission requests.
    pending_permissions: Vec<PendingPermission>,
    /// Index into pending_permissions for the currently displayed prompt.
    permission_index: usize,
    /// Timestamp of the last Esc press (for double-Esc cancel detection).
    last_esc: Option<Instant>,
    /// Transient status message (text, is_error, when it was set).
    status_flash: Option<(String, bool, Instant)>,
}

impl<'a> Home<'a> {
    pub fn new() -> Self {
        Self {
            conversation: Conversation::new(),
            conversation_view: ConversationView::new(),
            input: Input::new(),
            session_list: SessionList::new(),
            command_palette: CommandPalette::new(),
            model_picker: ModelPicker::new(),
            persona_picker: PersonaPicker::new(),
            param_input: ParamInput::new(),
            config_model_params: ModelParams::default(),
            streaming: false,
            compacting: false,
            executing_tools: false,
            provider_name: String::from("none"),
            model_name: String::from("none"),
            pending_permissions: Vec::new(),
            permission_index: 0,
            last_esc: None,
            status_flash: None,
        }
    }

    /// Initialize the model picker's provider/agent tree. Called once at startup.
    pub fn init_model_sources(&mut self, sources: Vec<ModelSourceConfig>) {
        self.model_picker.set_sources(sources);
    }

    /// Update the status bar and the model picker's active indicator.
    /// Cheap — does not rebuild the provider/model tree.
    pub fn set_active_source(&mut self, name: &str, model: &str) {
        self.provider_name = name.to_string();
        self.model_name = model.to_string();
        self.model_picker.set_active(name, model);
    }

    /// Supply/refresh a provider's known model list in the picker (e.g. after a
    /// lazy fetch completes, or opportunistically once a provider has been
    /// instantiated and its live model list is known for free).
    pub fn set_provider_models(&mut self, name: &str, models: Vec<String>) {
        self.model_picker.set_provider_models(name, models);
    }

    /// Set the config-level model params (from the active provider).
    pub fn set_config_params(&mut self, params: ModelParams) {
        self.config_model_params = params;
    }

    /// Replace the current conversation with a loaded one.
    pub fn load_conversation(&mut self, conversation: Conversation) {
        self.conversation = conversation;
        self.conversation_view.reset();
        self.streaming = false;
        self.compacting = false;
        self.executing_tools = false;
        self.pending_permissions.clear();
        self.input.set_focused(true);
    }

    /// Start a brand new conversation.
    pub fn new_conversation(&mut self) {
        self.conversation = Conversation::new();
        self.conversation_view.reset();
        self.streaming = false;
        self.compacting = false;
        self.executing_tools = false;
        self.pending_permissions.clear();
        self.input.set_focused(true);
    }

    /// Whether a permission prompt is showing.
    fn has_permission_prompt(&self) -> bool {
        !self.pending_permissions.is_empty()
    }

    /// Whether the app is currently doing work (streaming, tools, compacting).
    pub fn is_busy(&self) -> bool {
        self.streaming || self.executing_tools || self.compacting
    }
}

impl Home<'_> {
    /// Toggle expand/collapse of `<file>` blocks in the last user message
    /// that contains inlined file content.
    fn toggle_last_file_expansion(&mut self) {
        // Find the last user message whose text contains a <file> tag.
        if let Some(msg) = self.conversation.messages.iter().rev().find(|m| {
            m.role == crate::model::message::Role::User && m.text_content().contains("<file path=")
        }) {
            self.conversation_view.toggle_file_expansion(msg.id);
        }
    }

    /// Render the entire home view, passing the store to the conversation view
    /// so it can load image attachments for rendering.
    pub fn render_with_store(&mut self, frame: &mut Frame, area: Rect, store: &Store) {
        let input_height = self.input.desired_height();
        let layout = Layout::vertical([
            Constraint::Fill(1),              // Conversation view
            Constraint::Length(input_height), // Input area (expands with attachments)
            Constraint::Length(1),            // Status bar
        ])
        .split(area);

        // Render conversation view (with store access for images)
        self.conversation_view.render_with_conversation(
            frame,
            layout[0],
            &self.conversation,
            store,
        );

        // Render input
        self.input.render(frame, layout[1]);

        // Render @-path completion popup (floats above input).
        self.input.render_completion_popup(frame, layout[1]);

        // Render status bar
        self.render_status_bar(frame, layout[2]);

        // Render overlays
        self.session_list.render_overlay(frame, area);
        self.command_palette.render_overlay(frame, area);
        self.model_picker.render_overlay(frame, area);
        self.persona_picker.render_overlay(frame, area);
        self.param_input.render_overlay(frame, area);

        // Render permission prompt overlay on top of everything
        if self.has_permission_prompt() {
            self.render_permission_prompt(frame, area);
        }
    }
}

impl Component for Home<'_> {
    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Action {
        match mouse.kind {
            MouseEventKind::ScrollUp => Action::ScrollUp,
            MouseEventKind::ScrollDown => Action::ScrollDown,
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                Action::ClickAt(mouse.column, mouse.row)
            }
            _ => Action::Noop,
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Action {
        // Double-Esc cancels the current operation when busy.
        // We check this before overlays so it works from any state.
        if key.code == KeyCode::Esc && self.is_busy() {
            let now = Instant::now();
            if let Some(prev) = self.last_esc {
                if now.duration_since(prev).as_millis() < 500 {
                    self.last_esc = None;
                    return Action::CancelOperation;
                }
            }
            self.last_esc = Some(now);
            // Fall through so the first Esc still does its normal thing
            // (clear input, close overlay, etc.)
        } else if key.code != KeyCode::Esc {
            // Any non-Esc key resets the double-Esc timer
            self.last_esc = None;
        }

        // If session list is visible, it handles keys exclusively
        if self.session_list.visible {
            return self.session_list.handle_key_event(key);
        }

        // If model picker is visible, it handles keys exclusively
        if self.model_picker.visible {
            return self.model_picker.handle_key_event(key);
        }

        // If persona picker is visible, it handles keys exclusively
        if self.persona_picker.visible {
            return self.persona_picker.handle_key_event(key);
        }

        // If command palette is visible, it handles keys exclusively
        if self.command_palette.visible {
            return self.command_palette.handle_key_event(key);
        }

        // If param input is visible, it handles keys exclusively
        if self.param_input.visible {
            return self.param_input.handle_key_event(key);
        }

        // If permission prompt is showing, handle permission keys
        if self.has_permission_prompt() {
            return match (key.code, key.modifiers) {
                (KeyCode::Char('y') | KeyCode::Enter, _) => {
                    // Approve current permission
                    if let Some(perm) = self.pending_permissions.get(self.permission_index) {
                        let call_id = perm.call_id.clone();
                        self.pending_permissions.remove(self.permission_index);
                        if self.permission_index > 0
                            && self.permission_index >= self.pending_permissions.len()
                        {
                            self.permission_index =
                                self.pending_permissions.len().saturating_sub(1);
                        }
                        Action::ToolPermissionGranted { call_id }
                    } else {
                        Action::Noop
                    }
                }
                (KeyCode::Char('n') | KeyCode::Esc, _) => {
                    // Deny current permission
                    if let Some(perm) = self.pending_permissions.get(self.permission_index) {
                        let call_id = perm.call_id.clone();
                        self.pending_permissions.remove(self.permission_index);
                        if self.permission_index > 0
                            && self.permission_index >= self.pending_permissions.len()
                        {
                            self.permission_index =
                                self.pending_permissions.len().saturating_sub(1);
                        }
                        Action::ToolPermissionDenied { call_id }
                    } else {
                        Action::Noop
                    }
                }
                (KeyCode::Char('a'), _) => {
                    // Approve all remaining
                    self.pending_permissions.clear();
                    self.permission_index = 0;
                    Action::ToolPermissionGrantAll
                }
                (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,
                _ => Action::Noop,
            };
        }

        // If input is focused, let it handle most keys
        if self.input.is_focused() {
            match (key.code, key.modifiers) {
                (KeyCode::PageUp, _) => return Action::ScrollPageUp,
                (KeyCode::PageDown, _) => return Action::ScrollPageDown,
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::CopySelection;
                }
                (KeyCode::Char('.'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::OpenPersonaPicker;
                }
                (KeyCode::Char('n'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::NewSession;
                }
                (KeyCode::Char('o'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::OpenSessionList;
                }
                (KeyCode::Char('k'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::CompactContext;
                }
                (KeyCode::Char('m'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::OpenModelPicker;
                }
                (KeyCode::Char('p'), m) if m.contains(KeyModifiers::CONTROL) => {
                    return Action::OpenCommandPalette;
                }
                (KeyCode::Char('e'), m) if m.contains(KeyModifiers::CONTROL) => {
                    self.toggle_last_file_expansion();
                    return Action::Noop;
                }
                _ => {}
            }
            return self.input.handle_key_event(key);
        }

        // When input is not focused (e.g. while streaming)
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => Action::Quit,
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => Action::CopySelection,
            (KeyCode::Char('k'), m) if m.contains(KeyModifiers::CONTROL) => Action::CompactContext,
            (KeyCode::Char('m'), m) if m.contains(KeyModifiers::CONTROL) => Action::OpenModelPicker,
            (KeyCode::Char('.'), m) if m.contains(KeyModifiers::CONTROL) => {
                Action::OpenPersonaPicker
            }
            (KeyCode::Char('n'), m) if m.contains(KeyModifiers::CONTROL) => Action::NewSession,
            (KeyCode::Char('o'), m) if m.contains(KeyModifiers::CONTROL) => Action::OpenSessionList,
            (KeyCode::Char('p'), m) if m.contains(KeyModifiers::CONTROL) => {
                Action::OpenCommandPalette
            }
            (KeyCode::PageUp, _) => Action::ScrollPageUp,
            (KeyCode::PageDown, _) => Action::ScrollPageDown,
            (KeyCode::Char('e'), _) => {
                self.toggle_last_file_expansion();
                Action::Noop
            }
            _ => Action::Noop,
        }
    }

    fn update(&mut self, action: &Action) -> Option<Action> {
        match action {
            Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollToBottom => {
                self.conversation_view.update(action);
            }
            Action::CopySelection => {
                return self.conversation_view.update(action);
            }
            Action::ClickAt(_, _) => {
                self.conversation_view.update(action);
            }
            Action::SubmitMessage { text, .. } => {
                self.conversation.add_user_message(text.clone());
                self.conversation.start_assistant_message();
                self.streaming = true;
                self.input.set_focused(false);
                self.conversation_view.update(action);
                return Some(Action::SendMessage);
            }
            Action::AppendAssistantChunk(chunk) => {
                self.conversation.append_to_last(chunk);
                self.conversation_view.update(action);
            }
            Action::AssistantMessageComplete { .. } => {
                self.conversation.complete_last();
                self.streaming = false;
                self.executing_tools = false;
                self.input.set_focused(true);
                self.conversation_view.update(action);
            }
            Action::StreamError(err) => {
                self.conversation
                    .append_to_last(&format!("[Error: {}]", err));
                self.conversation.complete_last();
                self.streaming = false;
                self.executing_tools = false;
                self.input.set_focused(true);
                self.conversation_view
                    .update(&Action::AssistantMessageComplete { usage: None });
            }

            // -- Tool calling --
            Action::ToolCallsReceived { .. } => {
                // The assistant is done streaming text, now executing tools
                self.streaming = false;
                self.executing_tools = true;
                self.conversation_view
                    .update(&Action::AssistantMessageComplete { usage: None });
            }
            Action::ToolPermissionRequest {
                call_id,
                tool_name,
                arguments,
                permission_level,
            } => {
                self.pending_permissions.push(PendingPermission {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: arguments.clone(),
                    permission_level: permission_level.clone(),
                });
                self.input.set_focused(false);
            }
            Action::ToolResultReady { .. } => {
                // A tool finished — update the conversation view
                self.conversation_view.update(&Action::ScrollToBottom);
            }
            Action::ToolResultsComplete => {
                // All tool results in — LLM continuation will start streaming again
                self.streaming = true;
                self.executing_tools = false;
                self.conversation_view.update(&Action::ScrollToBottom);
            }

            // -- Session management --
            Action::FocusInput => {
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::NewSession => {
                self.new_conversation();
                self.session_list.hide();
            }
            Action::OpenSessionList => {
                return Some(Action::RefreshSessionList);
            }
            Action::CloseSessionList => {
                self.session_list.hide();
                self.input.set_focused(true);
            }
            Action::LoadSession(_) => {
                // Hide immediately on selection (local and ACP); local also
                // re-hides via SessionLoaded after the conversation is applied.
                self.session_list.hide();
                self.input.set_focused(true);
            }
            Action::SessionsLoaded(sessions) => {
                self.session_list.show(sessions.clone());
                self.input.set_focused(false);
            }
            Action::SessionLoaded(conversation) => {
                self.load_conversation(*conversation.clone());
                self.session_list.hide();
            }
            Action::OpenCommandPalette => {
                self.command_palette.show();
                self.input.set_focused(false);
            }
            Action::CloseCommandPalette => {
                self.command_palette.hide();
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::OpenPersonaPicker => {
                self.command_palette.hide();
                self.persona_picker.show(&self.conversation.persona);
                self.input.set_focused(false);
            }
            Action::ClosePersonaPicker => {
                self.persona_picker.hide();
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::SwitchPersona(name) => {
                self.conversation.persona = Persona::from_name(name);
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::OpenModelPicker => {
                // Close command palette if open, then show model picker
                self.command_palette.hide();
                let follow_up = self.model_picker.show();
                self.input.set_focused(false);
                return follow_up;
            }
            Action::ProviderModelsFetched(name, models) => {
                self.model_picker.set_provider_models(name, models.clone());
            }
            Action::CloseModelPicker => {
                self.model_picker.hide();
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::CompactContext => {
                if self.streaming || self.compacting || self.executing_tools {
                    return None;
                }
                self.compacting = true;
            }
            Action::CompactionComplete { .. } => {
                self.compacting = false;
                self.conversation_view.update(&Action::ScrollToBottom);
            }
            Action::CancelOperation => {
                // Reset all busy states — the App layer cancels the actual tasks
                self.streaming = false;
                self.compacting = false;
                self.executing_tools = false;
                self.pending_permissions.clear();
                self.permission_index = 0;
                self.input.set_focused(true);
                // If the assistant was mid-stream, mark the message complete
                self.conversation.complete_last();
                self.conversation_view
                    .update(&Action::AssistantMessageComplete { usage: None });
            }
            Action::CompactionError(_err) => {
                self.compacting = false;
            }
            Action::SwitchModel(_, _) => {
                // Handled by App, which calls set_provider_info to update status bar.
                // Restore input focus since the model picker just closed.
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }

            // -- Model parameter overrides --
            Action::OpenParamInput(kind) => {
                self.command_palette.hide();
                let current = match kind {
                    ParamKind::Temperature => self
                        .effective_params()
                        .temperature
                        .map(|v| format!("{}", v)),
                    ParamKind::TopP => self.effective_params().top_p.map(|v| format!("{}", v)),
                    ParamKind::MaxTokens => {
                        self.effective_params().max_tokens.map(|v| format!("{}", v))
                    }
                };
                self.param_input.show(kind.clone(), current);
                self.input.set_focused(false);
            }
            Action::CloseParamInput => {
                self.param_input.hide();
                if !self.streaming && !self.executing_tools {
                    self.input.set_focused(true);
                }
            }
            Action::SetTemperature(val) => {
                self.conversation.param_overrides.temperature = *val;
            }
            Action::SetTopP(val) => {
                self.conversation.param_overrides.top_p = *val;
            }
            Action::SetMaxTokens(val) => {
                self.conversation.param_overrides.max_tokens = *val;
            }

            // -- Export results --
            Action::ExportComplete(msg) => {
                self.status_flash = Some((msg.clone(), false, Instant::now()));
            }
            Action::ExportError(msg) => {
                self.status_flash = Some((msg.clone(), true, Instant::now()));
            }

            Action::PasteText(text) => {
                if self.input.is_focused() {
                    self.input.insert_text(text);
                }
            }

            Action::Tick => {
                // Forward ticks to conversation view to drive the streaming dots animation
                self.conversation_view.update(action);
                // Clear expired status flash messages
                if let Some((_, _, when)) = &self.status_flash {
                    if when.elapsed().as_secs() >= 3 {
                        self.status_flash = None;
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn render(&mut self, _frame: &mut Frame, _area: Rect) {
        // Use render_with_store() instead — called from App::run() directly.
        // This trait method is a no-op.
    }
}

impl Home<'_> {
    /// Compute effective model params: conversation overrides merged onto config defaults.
    fn effective_params(&self) -> ModelParams {
        self.config_model_params
            .merged_with(&self.conversation.param_overrides)
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let title = &self.conversation.title;

        // Activity indicator
        let activity = if self.compacting {
            " compacting..."
        } else if self.executing_tools {
            " executing tools..."
        } else if self.streaming {
            " streaming..."
        } else {
            ""
        };

        // Token usage & cost
        let usage_pct = self.conversation.context_usage();
        let context_style = theme::token_usage_style(usage_pct);
        let su = &self.conversation.session_usage;
        let has_actual_usage = su.prompt_tokens.is_some() || su.completion_tokens.is_some();

        // Build param indicator spans
        let effective = self.effective_params();
        let has_overrides = self.conversation.param_overrides.has_any();
        let param_style = if has_overrides {
            theme::PARAM_OVERRIDE_STYLE
        } else {
            theme::STATUS_BAR_HINT
        };

        let cwd_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "?".to_string());

        let mut spans = vec![
            Span::styled(
                format!(" {} ", self.conversation.persona.name()),
                theme::STATUS_BAR_LABEL,
            ),
            Span::raw(" "),
            Span::styled(cwd_name, theme::STATUS_BAR_HINT),
            Span::raw(" "),
            Span::styled(
                format!("{}/{}", self.provider_name, self.model_name),
                theme::STATUS_BAR_PROVIDER,
            ),
            Span::raw("  "),
            Span::styled(title.as_str(), theme::USER_CONTENT_STYLE),
            Span::raw("  "),
        ];

        // Token usage display: ↑1.2k ↓0.3k 42% $0.0089
        if has_actual_usage {
            let prompt = su.prompt_tokens.unwrap_or(0);
            let completion = su.completion_tokens.unwrap_or(0);
            spans.push(Span::styled(
                format!(
                    "\u{2191}{} \u{2193}{}",
                    theme::format_tokens(prompt),
                    theme::format_tokens(completion)
                ),
                theme::STATUS_BAR_HINT,
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("{:.0}%", usage_pct * 100.0),
            context_style,
        ));
        if self.conversation.session_cost > 0.0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("${:.4}", self.conversation.session_cost),
                theme::STATUS_BAR_HINT,
            ));
        }

        // Show param indicators only if any are set
        if effective.has_any() {
            let mut parts = Vec::new();
            if let Some(t) = effective.temperature {
                parts.push(format!("t:{:.1}", t));
            }
            if let Some(p) = effective.top_p {
                parts.push(format!("p:{:.1}", p));
            }
            if let Some(m) = effective.max_tokens {
                parts.push(format!("m:{}", m));
            }
            spans.push(Span::raw("  "));
            spans.push(Span::styled(parts.join(" "), param_style));
        }

        // Show flash message if present
        if let Some((ref msg, is_error, _)) = self.status_flash {
            let style = if is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(msg.as_str(), style));
        }

        spans.push(Span::styled(activity, theme::ACTIVITY_STYLE));
        spans.push(Span::raw("  "));
        if self.is_busy() {
            spans.push(Span::styled("Esc·Esc cancel  ", theme::STATUS_BAR_HINT));
        }
        spans.push(Span::styled("^P commands", theme::STATUS_BAR_HINT));

        let status = Paragraph::new(Line::from(spans)).alignment(Alignment::Left);
        frame.render_widget(status, area);
    }

    fn render_permission_prompt(&self, frame: &mut Frame, area: Rect) {
        let Some(perm) = self.pending_permissions.get(self.permission_index) else {
            return;
        };

        // Center the overlay
        let width = (area.width * 70 / 100).max(40).min(area.width);
        let height = 12u16.min(area.height);
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let level_str = match perm.permission_level {
            PermissionLevel::ReadOnly => "read-only",
            PermissionLevel::Destructive => "DESTRUCTIVE",
            PermissionLevel::Dangerous => "DANGEROUS",
        };

        let level_color = match perm.permission_level {
            PermissionLevel::ReadOnly => Color::Green,
            PermissionLevel::Destructive => Color::Yellow,
            PermissionLevel::Dangerous => Color::Red,
        };

        let remaining = self.pending_permissions.len();
        let title = if remaining > 1 {
            format!(" Tool Permission ({} pending) ", remaining)
        } else {
            " Tool Permission ".to_string()
        };

        // Format arguments nicely (truncate if too long)
        let args_str = serde_json::to_string_pretty(&perm.arguments).unwrap_or_default();
        let args_display = if args_str.len() > 200 {
            format!("{}...", &args_str[..197])
        } else {
            args_str
        };

        let text = vec![
            Line::from(vec![
                Span::raw("Tool: "),
                Span::styled(&perm.tool_name, Style::new().bold().fg(Color::Cyan)),
                Span::raw("  ["),
                Span::styled(level_str, Style::new().fg(level_color).bold()),
                Span::raw("]"),
            ]),
            Line::default(),
            Line::from(Span::styled("Arguments:", Style::new().dim())),
            Line::from(Span::raw(args_display)),
            Line::default(),
            Line::from(vec![
                Span::styled("y", Style::new().bold().fg(Color::Green)),
                Span::raw("/Enter approve  "),
                Span::styled("n", Style::new().bold().fg(Color::Red)),
                Span::raw("/Esc deny  "),
                Span::styled("a", Style::new().bold().fg(Color::Yellow)),
                Span::raw(" approve all"),
            ]),
        ];

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::new().fg(level_color))
            .padding(Padding::uniform(1));

        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });

        frame.render_widget(paragraph, popup_area);
    }
}
