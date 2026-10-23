use std::sync::Arc;

use color_eyre::eyre::Result;
use tokio::sync::{Mutex, mpsc::UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::acp::{AcpConnection, SessionRef, SessionSummary};
use crate::action::Action;
use crate::components::Component;
use crate::components::home::Home;
use crate::components::model_picker::ModelSourceConfig;
use crate::config::{Config, SourceKind};
use crate::event::Event;
use crate::mcp::McpManager;
use crate::model::message::{ContentPart, Message, Role, TokenUsage, ToolCallInfo};
use crate::persona::Persona;
use crate::pricing::PricingCatalog;
use crate::provider::{self, Provider};
use crate::store::Store;
use crate::tools::{PermissionLevel, ToolRegistry};
use crate::tui::Tui;

/// Active chat backend: direct genai provider or ACP agent.
enum ActiveSource {
    Provider(Provider),
    Agent(AcpConnection),
}

/// The top-level application. Owns all components and orchestrates
/// the event -> action -> update -> render cycle.
pub struct App<'a> {
    /// Whether the application should quit on the next iteration.
    should_quit: bool,
    /// The home component (main chat view).
    home: Home<'a>,
    /// Active LLM/agent backend.
    source: ActiveSource,
    /// The persistent store (SQLite).
    store: Store,
    /// The tool registry (built-in tools).
    tool_registry: ToolRegistry,
    /// The MCP manager (external tools from MCP servers).
    mcp_manager: Arc<Mutex<McpManager>>,
    /// Tool permission config.
    config: Config,
    /// Cancellation token — cancelled on Quit to stop all background tasks.
    cancellation_token: CancellationToken,
    /// Clone of the event_tx from Tui, used to send streaming actions back.
    action_tx: Option<UnboundedSender<Event>>,
    /// Whether the current conversation has been saved to the database.
    conversation_saved: bool,
    /// Pending attachments from the most recent SubmitMessage, consumed by SendMessage.
    pending_attachments: Vec<crate::components::input::PendingAttachment>,
    /// Pending tool calls waiting for permission or execution.
    pending_tool_calls: Vec<ToolCallInfo>,
    /// Number of tool results received for the current batch.
    tool_results_received: usize,
    /// Total tool calls expected for the current batch.
    tool_results_expected: usize,
    /// Pricing catalog for cost estimation (loaded from models.dev).
    pricing: Option<PricingCatalog>,
    /// True while replaying an ACP session/load.
    acp_loading: bool,
    /// ACP permission request ids awaiting user response.
    acp_pending_permissions: Vec<String>,
}

impl<'a> App<'a> {
    pub fn new(provider: Provider, store: Store, config: Config, mcp_manager: McpManager) -> Self {
        Self {
            should_quit: false,
            home: Home::new(),
            source: ActiveSource::Provider(provider),
            store,
            tool_registry: ToolRegistry::new(),
            mcp_manager: Arc::new(Mutex::new(mcp_manager)),
            config,
            cancellation_token: CancellationToken::new(),
            action_tx: None,
            conversation_saved: false,
            pending_attachments: Vec::new(),
            pending_tool_calls: Vec::new(),
            tool_results_received: 0,
            tool_results_expected: 0,
            pricing: None,
            acp_loading: false,
            acp_pending_permissions: Vec::new(),
        }
    }

    /// Create an app that will connect to an ACP agent during `run`.
    pub async fn new_for_agent(store: Store, config: Config) -> Self {
        // Placeholder provider is unused; replaced in run() when the agent connects.
        // Build from first configured provider if any, else a minimal stub via config default.
        let provider = if let Some(p) = config.provider.first() {
            let mut cfg = config.clone();
            cfg.default_source = Some(p.name.clone());
            match Provider::from_config(&cfg).await {
                Ok(p) => p,
                // Fall through — connect agent path doesn't need this provider.
                Err(_) => Provider::from_config(&Config::default())
                    .await
                    .expect("default provider"),
            }
        } else {
            Provider::from_config(&Config::default())
                .await
                .expect("default provider")
        };
        Self {
            should_quit: false,
            home: Home::new(),
            source: ActiveSource::Provider(provider),
            store,
            tool_registry: ToolRegistry::new(),
            mcp_manager: Arc::new(Mutex::new(McpManager::new())),
            config,
            cancellation_token: CancellationToken::new(),
            action_tx: None,
            conversation_saved: false,
            pending_attachments: Vec::new(),
            pending_tool_calls: Vec::new(),
            tool_results_received: 0,
            tool_results_expected: 0,
            pricing: None,
            acp_loading: false,
            acp_pending_permissions: Vec::new(),
        }
    }

    fn is_acp(&self) -> bool {
        matches!(self.source, ActiveSource::Agent(_))
    }

    fn source_name(&self) -> &str {
        match &self.source {
            ActiveSource::Provider(p) => p.name(),
            ActiveSource::Agent(a) => a.name(),
        }
    }

    fn model_name(&self) -> &str {
        match &self.source {
            ActiveSource::Provider(p) => p.model(),
            ActiveSource::Agent(_) => "default",
        }
    }

    fn provider_mut(&mut self) -> Option<&mut Provider> {
        match &mut self.source {
            ActiveSource::Provider(p) => Some(p),
            ActiveSource::Agent(_) => None,
        }
    }

    /// Bind the active provider's session-scoped headers to the current conversation.
    /// No-op for ACP sources or when the provider rebuild fails.
    fn sync_provider_conversation_id(&mut self) {
        let id = self.home.conversation.id;
        if let Some(p) = self.provider_mut() {
            if let Err(e) = p.set_conversation_id(id) {
                error!("Failed to set provider conversation ID: {}", e);
            }
        }
    }

    fn provider_ref(&self) -> Option<&Provider> {
        match &self.source {
            ActiveSource::Provider(p) => Some(p),
            ActiveSource::Agent(_) => None,
        }
    }

    fn acp_ref(&self) -> Option<&AcpConnection> {
        match &self.source {
            ActiveSource::Agent(a) => Some(a),
            ActiveSource::Provider(_) => None,
        }
    }

    /// Run the main application loop.
    pub async fn run(&mut self, mut tui: Tui) -> Result<()> {
        tui.enter()?;

        // Stash a clone of the event_tx for sending actions from background tasks
        self.action_tx = Some(tui.event_tx.clone());

        // Connect ACP agent if that is the active source
        if let Some(agent_cfg) = self.config.active_agent().cloned() {
            match AcpConnection::connect(
                &agent_cfg,
                self.config.mcp_servers.clone(),
                self.config.tools.clone(),
                tui.event_tx.clone(),
            )
            .await
            {
                Ok(conn) => {
                    info!("Connected to ACP agent '{}'", conn.name());
                    match conn.new_session().await {
                        Ok(sid) => info!("ACP session created: {}", sid),
                        Err(e) => error!("Failed to create ACP session: {}", e),
                    }
                    self.source = ActiveSource::Agent(conn);
                }
                Err(e) => {
                    error!("Failed to connect ACP agent: {}", e);
                    return Err(e);
                }
            }
        }

        // Bind the initial provider's adapter-specific session headers (e.g.
        // x-opencode-session) to the starting conversation.
        self.sync_provider_conversation_id();

        // Initialize the model picker's provider/agent tree (once) and set the
        // active indicator for the status bar.
        self.home.init_model_sources(self.build_model_sources());
        let source_name = self.source_name().to_string();
        let model_name = self.model_name().to_string();
        self.home.set_active_source(&source_name, &model_name);

        // Set config-level model params for the active provider
        if let Some(provider_config) = self.config.active_provider() {
            self.home.set_config_params(provider_config.model_params());
        }

        // Set default persona from config
        self.home.conversation.persona = self.default_persona();

        // Initialize components
        self.home.init()?;

        // Kick off async pricing fetch from models.dev
        {
            let event_tx = tui.event_tx.clone();
            tokio::spawn(async move {
                match PricingCatalog::fetch_and_cache().await {
                    Ok(catalog) => {
                        let _ = event_tx.send(Event::PricingLoaded(catalog));
                    }
                    Err(e) => {
                        warn!("Failed to load pricing catalog: {}", e);
                    }
                }
            });
        }

        loop {
            // Draw the current frame
            tui.terminal.draw(|frame| {
                let area = frame.area();
                self.home.render_with_store(frame, area, &self.store);
            })?;

            // Wait for the next event
            if let Some(event) = tui.next().await {
                // Let the component handle the event and produce an action
                let action = self.handle_event(&event);
                // Process the action (and any follow-up actions)
                self.handle_action(&action).await;
            }

            if self.should_quit {
                break;
            }
        }

        // Shut down MCP servers / ACP agent gracefully before exiting
        if let Some(acp) = self.acp_ref() {
            acp.shutdown();
        }
        {
            let mut mgr = self.mcp_manager.lock().await;
            mgr.shutdown().await;
        }

        tui.exit()?;
        Ok(())
    }

    /// Dispatch an event to components and return the resulting action.
    fn handle_event(&mut self, event: &Event) -> Action {
        if matches!(event, Event::Render) {
            return Action::Render;
        }
        self.home.handle_event(event)
    }

    /// Process an action and any follow-up actions from components.
    async fn handle_action(&mut self, action: &Action) {
        match action {
            Action::Quit => {
                info!("Quit requested — cancelling background tasks");
                self.cancellation_token.cancel();
                self.should_quit = true;
            }
            Action::CancelOperation => {
                info!("Cancel requested — stopping current operation");
                if let Some(acp) = self.acp_ref() {
                    acp.cancel();
                }
                // Cancel all in-flight tasks (streaming, tool execution, compaction)
                self.cancellation_token.cancel();
                // Replace with a fresh token so future operations work
                self.cancellation_token = CancellationToken::new();
                // Reset tool tracking
                self.pending_tool_calls.clear();
                self.tool_results_received = 0;
                self.tool_results_expected = 0;
                // Save whatever partial message we have
                if !self.is_acp() {
                    self.save_last_message();
                    self.update_conversation_meta();
                }
            }
            Action::SubmitMessage {
                text: _,
                attachments,
            } => {
                self.ensure_conversation_saved();
                // Stash attachments — they'll be applied in SendMessage after
                // Home::update has created the user message.
                self.pending_attachments = attachments.clone();
            }
            Action::SendMessage => {
                // Home::update has now created the user message (second-to-last)
                // and the assistant placeholder (last). Attach images to the user message.
                let attachments = std::mem::take(&mut self.pending_attachments);
                if !attachments.is_empty() {
                    let msg_count = self.home.conversation.messages.len();
                    if msg_count >= 2 {
                        let user_msg = &mut self.home.conversation.messages[msg_count - 2];
                        for att in &attachments {
                            user_msg.add_image(att.id, att.mime_type.clone(), None);
                        }
                    }
                }

                // Save user message to DB first (attachments FK references it).
                if !self.is_acp() {
                    self.save_recent_user_message();

                    // Now persist attachment blobs (message row exists).
                    if !attachments.is_empty() {
                        let conv_id = self.home.conversation.id;
                        let msg_count = self.home.conversation.messages.len();
                        if msg_count >= 2 {
                            let msg_id = self.home.conversation.messages[msg_count - 2].id;
                            for att in attachments {
                                if let Err(e) = self.store.save_attachment(
                                    &att.id,
                                    &conv_id,
                                    &msg_id,
                                    &att.mime_type,
                                    &att.data,
                                ) {
                                    error!("Failed to save attachment: {}", e);
                                }
                            }
                        }
                    }
                }

                if self.is_acp() {
                    self.send_to_acp().await;
                } else {
                    self.send_to_provider();
                }
            }
            Action::AssistantMessageComplete { usage } => {
                // Store usage on the assistant message and accumulate into session
                if let Some(u) = usage {
                    if let Some(msg) = self.home.conversation.messages.last_mut() {
                        msg.usage = Some(u.clone());
                    }
                    self.record_usage(u);
                }
                if !self.is_acp() {
                    self.save_last_message();
                    self.update_conversation_meta();
                    self.check_auto_compact();
                }
            }
            Action::StreamError(_) => {
                if !self.is_acp() {
                    self.save_last_message();
                    self.update_conversation_meta();
                }
            }

            // -- Tool calling --
            Action::ToolCallsReceived { tool_calls, usage } => {
                if !self.is_acp() {
                    info!("Received {} tool call(s) from assistant", tool_calls.len());
                    // Complete the current assistant message and record the tool calls on it
                    self.home.conversation.complete_last();
                    if let Some(msg) = self.home.conversation.messages.last_mut() {
                        msg.tool_calls = tool_calls.clone();
                        if let Some(u) = usage {
                            msg.usage = Some(u.clone());
                        }
                    }
                    if let Some(u) = usage {
                        self.record_usage(u);
                    }
                    self.save_last_message();

                    // Process each tool call for permission
                    self.pending_tool_calls = tool_calls.clone();
                    self.tool_results_received = 0;
                    self.tool_results_expected = tool_calls.len();
                    self.process_next_tool_call().await;
                }
            }
            Action::ToolPermissionRequest { call_id, .. } => {
                if self.is_acp() {
                    self.acp_pending_permissions.push(call_id.clone());
                }
                // Handled by home.update() below — it shows the permission prompt
            }
            Action::ToolPermissionGranted { call_id } => {
                if self.is_acp() {
                    self.acp_pending_permissions.retain(|id| id != call_id);
                    if let Some(acp) = self.acp_ref() {
                        acp.respond_permission(call_id.clone(), true, false);
                    }
                } else {
                    self.execute_tool(call_id);
                }
            }
            Action::ToolPermissionDenied { call_id } => {
                if self.is_acp() {
                    self.acp_pending_permissions.retain(|id| id != call_id);
                    if let Some(acp) = self.acp_ref() {
                        acp.respond_permission(call_id.clone(), false, false);
                    }
                } else {
                    info!("Tool call {} denied by user", call_id);
                    // Add a tool result indicating denial
                    let tool_call = self
                        .pending_tool_calls
                        .iter()
                        .find(|tc| tc.call_id == *call_id);
                    let fn_name = tool_call.map(|tc| tc.fn_name.clone()).unwrap_or_default();

                    self.home.conversation.messages.push(Message::tool_result(
                        call_id.clone(),
                        fn_name,
                        vec![ContentPart::Text {
                            text: "Tool execution denied by user.".to_string(),
                        }],
                        false,
                    ));
                    self.save_last_message();
                    self.tool_results_received += 1;
                    self.check_tool_results_complete().await;
                }
            }
            Action::ToolPermissionGrantAll => {
                if self.is_acp() {
                    let ids = std::mem::take(&mut self.acp_pending_permissions);
                    if let Some(acp) = self.acp_ref() {
                        for id in ids {
                            acp.respond_permission(id, true, true);
                        }
                    }
                } else {
                    // Execute all remaining pending tool calls
                    let pending: Vec<String> = self
                        .pending_tool_calls
                        .iter()
                        .map(|tc| tc.call_id.clone())
                        .collect();
                    for call_id in pending {
                        self.execute_tool(&call_id);
                    }
                }
            }
            Action::ToolResultReady {
                call_id,
                fn_name,
                content,
                success,
                blobs,
            } => {
                let text_len: usize = content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => text.len(),
                        ContentPart::Image { .. } => 0,
                    })
                    .sum();
                info!(
                    "Tool result for {}/{}: success={} ({} bytes, {} blobs)",
                    fn_name,
                    call_id,
                    success,
                    text_len,
                    blobs.len()
                );
                // Add tool result message to conversation
                let msg = Message::tool_result(
                    call_id.clone(),
                    fn_name.clone(),
                    content.clone(),
                    *success,
                );
                let msg_id = msg.id;
                self.home.conversation.messages.push(msg);
                self.save_last_message();

                // Persist any binary blobs (MCP image responses) as attachments
                for (blob_id, mime_type, data) in blobs {
                    if let Err(e) = self.store.save_attachment(
                        blob_id,
                        &self.home.conversation.id,
                        &msg_id,
                        mime_type,
                        data,
                    ) {
                        error!("Failed to save MCP image attachment: {}", e);
                    }
                }

                self.tool_results_received += 1;
                self.check_tool_results_complete().await;
            }
            Action::ToolResultsComplete => {
                if self.is_acp() {
                    // ACP agent continues on its own; no genai continuation.
                } else {
                    info!("All tool results collected, sending continuation to LLM");
                    // Start a new streaming assistant message for the continuation
                    self.home.conversation.start_assistant_message();
                    self.send_to_provider();
                }
            }

            // -- Model switching --
            Action::SwitchModel(model, provider_name) => {
                match self.config.source_by_name(provider_name) {
                    Some(SourceKind::Agent(agent_cfg)) => {
                        let agent_cfg = agent_cfg.clone();
                        if let Some(event_tx) = self.action_tx.clone() {
                            if let Some(acp) = self.acp_ref() {
                                acp.shutdown();
                            }
                            match AcpConnection::connect(
                                &agent_cfg,
                                self.config.mcp_servers.clone(),
                                self.config.tools.clone(),
                                event_tx,
                            )
                            .await
                            {
                                Ok(conn) => {
                                    info!("Switched to ACP agent '{}'", conn.name());
                                    if let Err(e) = conn.new_session().await {
                                        error!("Failed to create ACP session: {}", e);
                                    }
                                    self.source = ActiveSource::Agent(conn);
                                    self.conversation_saved = false;
                                    self.home.new_conversation();
                                    self.home.conversation.persona = self.default_persona();
                                }
                                Err(e) => error!("Failed to connect ACP agent: {}", e),
                            }
                        }
                    }
                    Some(SourceKind::Provider(_)) => {
                        if self.source_name() == provider_name {
                            if let Some(p) = self.provider_mut() {
                                match p.set_model(model) {
                                    Ok(()) => info!("Switched to model: {}", model),
                                    Err(e) => error!("Failed to switch model: {}", e),
                                }
                            }
                        } else {
                            let mut new_config = self.config.clone();
                            new_config.default_source = Some(provider_name.clone());
                            match Provider::from_config_with_model(&new_config, model).await {
                                Ok(new_provider) => {
                                    if let Some(acp) = self.acp_ref() {
                                        acp.shutdown();
                                    }
                                    self.source = ActiveSource::Provider(new_provider);
                                    info!(
                                        "Switched to provider {} / model {}",
                                        provider_name, model
                                    );
                                }
                                Err(e) => error!("Failed to switch provider: {}", e),
                            }
                        }
                    }
                    None => error!("Source '{}' not found in config", provider_name),
                }
                // Switching to a Provider gives us its live model list for free
                // (whether explicit from config or just auto-discovered); push it
                // into the picker's tree so it doesn't need a redundant fetch later.
                if !self.is_acp() {
                    if let Some(p) = self.provider_ref() {
                        let name = p.name().to_string();
                        let models = p.models().to_vec();
                        self.home.set_provider_models(&name, models);
                    }
                    // A new Provider instance may have just been built with no
                    // conversation ID bound; rebind adapter-specific session headers
                    // (e.g. x-opencode-session) to the current conversation.
                    self.sync_provider_conversation_id();
                }
                let source_name = self.source_name().to_string();
                let model_name = self.model_name().to_string();
                self.home.set_active_source(&source_name, &model_name);
                if let Some(pc) = self
                    .config
                    .provider
                    .iter()
                    .find(|p| p.name == *provider_name)
                {
                    self.home.set_config_params(pc.model_params());
                }
                self.home.conversation.provider = self.source_name().to_string();
                self.home.conversation.model = self.model_name().to_string();
                self.update_context_window();
                if !self.is_acp() {
                    self.persist_session_config();
                }
            }
            Action::SwitchPersona(_) => {
                // Handled by home.update() below, then persisted after
            }
            Action::OpenPersonaPicker | Action::ClosePersonaPicker => {
                // Handled by home.update() below
            }
            Action::OpenModelPicker | Action::CloseModelPicker => {
                // Handled by home.update() below
            }
            Action::RequestProviderModels(name) => {
                if let Some(pc) = self.config.provider.iter().find(|p| &p.name == name) {
                    let pc = pc.clone();
                    if let Some(event_tx) = self.action_tx.clone() {
                        tokio::spawn(async move {
                            let models = provider::discover_models(&pc).await;
                            let _ = event_tx.send(Event::ProviderModelsFetched(pc.name, models));
                        });
                    }
                }
            }
            Action::ProviderModelsFetched(_, _) => {
                // Handled by home.update() below
            }
            Action::OpenParamInput(_) | Action::CloseParamInput => {
                // Handled by home.update() below
            }
            Action::SetTemperature(_) | Action::SetTopP(_) | Action::SetMaxTokens(_) => {
                // Handled by home.update() below, then persist
            }

            // -- Session management --
            Action::NewSession => {
                info!("Starting new session");
                self.conversation_saved = false;
                self.pending_tool_calls.clear();
                self.acp_pending_permissions.clear();
                // Apply default persona from config
                self.home.conversation.persona = self.default_persona();
                if self.is_acp() {
                    let result = if let Some(acp) = self.acp_ref() {
                        acp.new_session().await
                    } else {
                        return;
                    };
                    match result {
                        Ok(sid) => {
                            info!("ACP session created: {}", sid);
                            let source_name = self.source_name().to_string();
                            let label = short_session_label(&sid);
                            self.home.conversation.provider = source_name.clone();
                            self.home.conversation.model = label.clone();
                            self.home.set_active_source(&source_name, &label);
                            if let Some(acp) = self.acp_ref() {
                                debug!("ACP active session_id={:?}", acp.session_id().await);
                            }
                        }
                        Err(e) => error!("Failed to create ACP session: {}", e),
                    }
                }
            }
            Action::RefreshSessionList => {
                self.load_session_list().await;
            }
            Action::LoadSession(id) => {
                info!("Loading session: {}", id.as_str());
                // Hide overlay before awaiting load (ACP can take a while).
                let _ = self.home.update(&Action::CloseSessionList);
                self.load_session(id).await;
            }
            Action::DeleteSession(id) => {
                info!("Deleting session: {}", id.as_str());
                match id {
                    SessionRef::Local(uuid) => {
                        if let Err(e) = self.store.delete_conversation(uuid) {
                            error!("Failed to delete conversation: {}", e);
                        }
                        if self.home.conversation.id == *uuid {
                            self.conversation_saved = false;
                        }
                    }
                    SessionRef::Acp(acp_id) => {
                        if let Some(acp) = self.acp_ref() {
                            if let Err(e) = acp.delete_session(acp_id.clone()).await {
                                error!("Failed to delete ACP session: {}", e);
                            }
                        }
                    }
                }
            }
            Action::AcpToolCallDisplay(info) => {
                let note = format!("\n[tool: {} #{}]\n", info.fn_name, info.call_id);
                if let Some(msg) = self.home.conversation.messages.last_mut() {
                    if msg.role == Role::Assistant {
                        msg.append(&note);
                    }
                }
            }
            Action::AcpToolCallUpdateDisplay(update) => {
                let mut parts = vec![format!("#{}", update.call_id)];
                if let Some(ref title) = update.title {
                    parts.push(title.clone());
                }
                if let Some(status) = &update.status {
                    parts.push(status.clone());
                }
                if let Some(text) = &update.content_text {
                    parts.push(text.clone());
                }
                let note = format!("\n[{}]\n", parts.join(" — "));
                if let Some(msg) = self.home.conversation.messages.last_mut() {
                    if msg.role == Role::Assistant {
                        msg.append(&note);
                    }
                }
            }
            Action::AcpUserMessageReplay(text) => {
                if self.acp_loading {
                    self.home.conversation.complete_last();
                    self.home
                        .conversation
                        .messages
                        .push(Message::user(text.clone()));
                }
            }
            Action::AcpUsageUpdate(usage) => {
                self.record_usage(usage);
                if let Some(msg) = self.home.conversation.messages.last_mut() {
                    msg.usage = Some(usage.clone());
                }
            }
            Action::AcpSessionLoadComplete => {
                self.acp_loading = false;
                self.home.conversation.complete_last();
                info!("ACP session load complete");
            }
            // -- Export --
            Action::ExportMarkdown => {
                if self.home.conversation.messages.is_empty() {
                    let _ = self
                        .home
                        .update(&Action::ExportError("Nothing to export".into()));
                    return;
                }
                let content = crate::export::to_markdown(&self.home.conversation);
                let path = crate::export::export_file_path(&self.home.conversation.title, "md");
                let follow = match std::fs::write(&path, &content) {
                    Ok(_) => Action::ExportComplete(format!("Exported to {}", path.display())),
                    Err(e) => Action::ExportError(format!("Export failed: {e}")),
                };
                let _ = self.home.update(&follow);
                return;
            }
            Action::ExportJson => {
                if self.home.conversation.messages.is_empty() {
                    let _ = self
                        .home
                        .update(&Action::ExportError("Nothing to export".into()));
                    return;
                }
                let follow = match crate::export::to_json(&self.home.conversation) {
                    Ok(content) => {
                        let path =
                            crate::export::export_file_path(&self.home.conversation.title, "json");
                        match std::fs::write(&path, &content) {
                            Ok(_) => {
                                Action::ExportComplete(format!("Exported to {}", path.display()))
                            }
                            Err(e) => Action::ExportError(format!("Export failed: {e}")),
                        }
                    }
                    Err(e) => Action::ExportError(format!("Export failed: {e}")),
                };
                let _ = self.home.update(&follow);
                return;
            }
            Action::ExportMarkdownClipboard => {
                if self.home.conversation.messages.is_empty() {
                    let _ = self
                        .home
                        .update(&Action::ExportError("Nothing to export".into()));
                    return;
                }
                let content = crate::export::to_markdown(&self.home.conversation);
                let follow = match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(content))
                {
                    Ok(_) => Action::ExportComplete("Copied Markdown to clipboard".into()),
                    Err(e) => Action::ExportError(format!("Clipboard failed: {e}")),
                };
                let _ = self.home.update(&follow);
                return;
            }
            Action::ExportJsonClipboard => {
                if self.home.conversation.messages.is_empty() {
                    let _ = self
                        .home
                        .update(&Action::ExportError("Nothing to export".into()));
                    return;
                }
                let follow = match crate::export::to_json(&self.home.conversation) {
                    Ok(content) => {
                        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(content)) {
                            Ok(_) => Action::ExportComplete("Copied JSON to clipboard".into()),
                            Err(e) => Action::ExportError(format!("Clipboard failed: {e}")),
                        }
                    }
                    Err(e) => Action::ExportError(format!("Export failed: {e}")),
                };
                let _ = self.home.update(&follow);
                return;
            }
            Action::CopyCodeBlockResult(content) => {
                let line_count = content.lines().count();
                let follow = match arboard::Clipboard::new()
                    .and_then(|mut cb| cb.set_text(content.clone()))
                {
                    Ok(_) => {
                        Action::ExportComplete(format!("Copied {} lines to clipboard", line_count))
                    }
                    Err(e) => Action::ExportError(format!("Clipboard failed: {e}")),
                };
                let _ = self.home.update(&follow);
                return;
            }
            Action::ExportComplete(_) | Action::ExportError(_) => {
                // Handled by home.update() below (status flash message)
            }

            Action::TitleGenerated { title, usage } => {
                info!("Title generated: {}", title);
                if let Some(u) = usage {
                    self.record_usage(u);
                }
                self.home.conversation.title = title.clone();
                self.update_conversation_meta();
            }

            Action::CompactContext => {
                info!("Starting context compaction");
                self.start_compaction();
            }
            Action::CompactionComplete {
                summary,
                compacted_ids,
                usage,
            } => {
                info!(
                    "Compaction complete: {} messages compacted",
                    compacted_ids.len()
                );
                if let Some(u) = usage {
                    self.record_usage(u);
                }
                self.apply_compaction(summary, compacted_ids);
            }
            Action::CompactionError(err) => {
                warn!("Compaction failed: {}", err);
            }
            Action::AppendAssistantChunk(_) => {
                // Handled by home.update() below
            }
            Action::PricingLoaded(catalog) => {
                info!(
                    "Pricing catalog loaded with {} providers",
                    catalog.provider_count()
                );
                self.pricing = Some(catalog.clone());
                self.update_context_window();
            }
            Action::Noop | Action::Tick | Action::Render => {}
            Action::Resize(_, _) => {}
            _ => {}
        }

        // Give the component a chance to react to the action
        if let Some(follow_up) = self.home.update(action) {
            Box::pin(self.handle_action(&follow_up)).await;
        }

        // After a new session the conversation ID has changed; rebind provider headers.
        if matches!(action, Action::NewSession) {
            self.sync_provider_conversation_id();
        }

        // Generate title after home.update() has marked the assistant message as complete
        if matches!(action, Action::AssistantMessageComplete { .. }) {
            self.maybe_generate_title();
        }

        // Persist session config after param/persona changes (home.update sets the values first)
        if matches!(
            action,
            Action::SetTemperature(_)
                | Action::SetTopP(_)
                | Action::SetMaxTokens(_)
                | Action::SwitchPersona(_)
        ) {
            self.persist_session_config();
        }
    }

    /// Record usage from an API call: accumulate into session totals and compute cost.
    fn record_usage(&mut self, usage: &TokenUsage) {
        self.home.conversation.add_usage(usage);
        if let Some(catalog) = &self.pricing {
            let adapter = self
                .config
                .active_provider()
                .and_then(|p| p.adapter.as_deref());
            let provider_id =
                PricingCatalog::map_provider_id(&self.home.conversation.provider, adapter);
            if let Some(cost) =
                catalog.calculate_cost(&provider_id, &self.home.conversation.model, usage)
            {
                self.home.conversation.session_cost += cost;
            }
        }
    }

    /// Update the conversation's context window from the pricing catalog.
    fn update_context_window(&mut self) {
        if let Some(catalog) = &self.pricing {
            let adapter = self
                .config
                .active_provider()
                .and_then(|p| p.adapter.as_deref());
            let provider_id =
                PricingCatalog::map_provider_id(&self.home.conversation.provider, adapter);
            if let Some(ctx) = catalog.context_window(&provider_id, &self.home.conversation.model) {
                self.home.conversation.context_window = ctx;
            }
        }
    }

    // ── Tool execution helpers ─────────────────────────────────────

    /// Process pending tool calls: check permission and either auto-approve or prompt.
    async fn process_next_tool_call(&mut self) {
        if self.pending_tool_calls.is_empty() {
            return;
        }

        // Collect decisions first to avoid borrow conflict
        let mut auto_approve_ids = Vec::new();
        let mut permission_actions = Vec::new();

        for tc in &self.pending_tool_calls {
            let permission_level = if let Some(tool) = self.tool_registry.get(&tc.fn_name) {
                tool.permission_level()
            } else {
                // MCP tools default to Destructive — we don't know their side effects
                PermissionLevel::Destructive
            };

            let should_auto_approve = match permission_level {
                PermissionLevel::ReadOnly => self.config.tools.auto_approve_read,
                PermissionLevel::Destructive => self.config.tools.auto_approve_write,
                PermissionLevel::Dangerous => self.config.tools.auto_approve_execute,
            };

            if should_auto_approve {
                debug!("Auto-approving tool call: {} ({})", tc.fn_name, tc.call_id);
                auto_approve_ids.push(tc.call_id.clone());
            } else {
                permission_actions.push(Action::ToolPermissionRequest {
                    call_id: tc.call_id.clone(),
                    tool_name: tc.fn_name.clone(),
                    arguments: tc.fn_arguments.clone(),
                    permission_level: permission_level.clone(),
                });
            }
        }

        // Execute auto-approved tools
        for call_id in auto_approve_ids {
            self.execute_tool(&call_id);
        }

        // Dispatch permission requests
        for action in permission_actions {
            if let Some(follow_up) = self.home.update(&action) {
                Box::pin(self.handle_action(&follow_up)).await;
            }
        }
    }

    /// Execute a tool call by its call_id.
    fn execute_tool(&self, call_id: &str) {
        let tool_call = match self
            .pending_tool_calls
            .iter()
            .find(|tc| tc.call_id == call_id)
        {
            Some(tc) => tc.clone(),
            None => {
                error!("Tool call not found: {}", call_id);
                return;
            }
        };

        let Some(ref event_tx) = self.action_tx else {
            return;
        };

        let tool_summary = self
            .tool_registry
            .display_summary(&tool_call.fn_name, &tool_call.fn_arguments);
        info!("Executing tool: {}", tool_summary);

        let is_builtin = self.tool_registry.is_builtin(&tool_call.fn_name);
        let is_mcp = if let Ok(mcp) = self.mcp_manager.try_lock() {
            mcp.is_mcp_tool(&tool_call.fn_name)
        } else {
            false
        };

        if !is_builtin && !is_mcp {
            let _ = event_tx.send(Event::ToolExecutionResult {
                call_id: tool_call.call_id.clone(),
                fn_name: tool_call.fn_name.clone(),
                content: vec![ContentPart::Text {
                    text: format!("Unknown tool: {}", tool_call.fn_name),
                }],
                success: false,
                blobs: Vec::new(),
            });
            return;
        }

        let event_tx = event_tx.clone();
        let fn_name = tool_call.fn_name.clone();
        let call_id = tool_call.call_id.clone();
        let args = tool_call.fn_arguments.clone();

        let cancel = self.cancellation_token.clone();

        if is_mcp {
            // MCP tool — execute via McpManager, with cancellation
            let mcp = Arc::clone(&self.mcp_manager);
            tokio::spawn(async move {
                let mcp_result = tokio::select! {
                    res = async {
                        let mgr = mcp.lock().await;
                        mgr.execute_tool(&fn_name, args).await
                    } => res,
                    _ = cancel.cancelled() => {
                        debug!("MCP tool '{}' cancelled on shutdown", fn_name);
                        Err("Tool cancelled (application shutting down)".to_string())
                    }
                };
                match mcp_result {
                    Ok(output) => {
                        let success = !output.is_error;
                        let _ = event_tx.send(Event::ToolExecutionResult {
                            call_id,
                            fn_name,
                            content: output.parts,
                            success,
                            blobs: output.blobs,
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(Event::ToolExecutionResult {
                            call_id,
                            fn_name,
                            content: vec![ContentPart::Text { text: e }],
                            success: false,
                            blobs: Vec::new(),
                        });
                    }
                }
            });
        } else {
            // Built-in tool — execute via stateless reconstruction, with cancellation
            tokio::spawn(async move {
                let result = execute_tool_by_name(&fn_name, &args, cancel).await;
                let success = result.is_ok();
                let _ = event_tx.send(Event::ToolExecutionResult {
                    call_id,
                    fn_name,
                    success,
                    content: result.into_content_parts(),
                    blobs: Vec::new(),
                });
            });
        }
    }

    /// Check if all tool results are in, and if so, trigger continuation.
    async fn check_tool_results_complete(&mut self) {
        if self.tool_results_received >= self.tool_results_expected {
            debug!("All {} tool results collected", self.tool_results_received);
            self.pending_tool_calls.clear();
            // Dispatch ToolResultsComplete through the action system
            Box::pin(self.handle_action(&Action::ToolResultsComplete)).await;
        }
    }

    // ── Model helpers ───────────────────────────────────────────────

    /// Get the default persona from config (falls back to Ozma).
    fn default_persona(&self) -> Persona {
        self.config
            .default_persona
            .as_ref()
            .map(|name| Persona::from_name(name))
            .unwrap_or_default()
    }

    /// Build the model picker's provider/agent sources from config. Providers with
    /// an explicit `models` list carry it directly; others are left unknown (`None`)
    /// until lazily fetched, except the active provider which already has a live
    /// (possibly auto-discovered) model list available for free.
    fn build_model_sources(&self) -> Vec<ModelSourceConfig> {
        let active_name = self.source_name().to_string();
        let mut sources: Vec<ModelSourceConfig> = self
            .config
            .provider
            .iter()
            .map(|p| {
                let models = if p.models.is_some() {
                    p.models.clone()
                } else if p.name == active_name {
                    self.provider_ref().map(|pr| pr.models().to_vec())
                } else {
                    None
                };
                ModelSourceConfig::Provider {
                    name: p.name.clone(),
                    models,
                }
            })
            .collect();
        for agent in &self.config.agent {
            sources.push(ModelSourceConfig::Agent {
                name: agent.name.clone(),
            });
        }
        sources
    }

    // ── Persistence helpers ────────────────────────────────────────

    fn ensure_conversation_saved(&mut self) {
        if self.is_acp() {
            // ACP sessions are not persisted in ozma.db
            return;
        }
        if self.conversation_saved {
            return;
        }
        // Snapshot current provider/model onto the conversation
        self.home.conversation.provider = self.source_name().to_string();
        self.home.conversation.model = self.model_name().to_string();
        self.update_context_window();
        // Snapshot config-level params into overrides for any fields the user hasn't set
        let config_params = self
            .config
            .active_provider()
            .map(|p| p.model_params())
            .unwrap_or_default();
        self.home.conversation.param_overrides =
            config_params.merged_with(&self.home.conversation.param_overrides);

        if let Err(e) = self.store.save_conversation(&self.home.conversation) {
            error!("Failed to save conversation: {}", e);
            return;
        }
        self.conversation_saved = true;
    }

    fn save_recent_user_message(&self) {
        let messages = &self.home.conversation.messages;
        if messages.len() >= 2 {
            let user_msg = &messages[messages.len() - 2];
            if let Err(e) = self
                .store
                .save_message(&self.home.conversation.id, user_msg)
            {
                error!("Failed to save user message: {}", e);
            }
        }
    }

    fn save_last_message(&self) {
        if let Some(msg) = self.home.conversation.messages.last() {
            if let Err(e) = self.store.save_message(&self.home.conversation.id, msg) {
                error!("Failed to save message: {}", e);
            }
        }
    }

    fn update_conversation_meta(&self) {
        if let Err(e) = self.store.update_conversation_meta(&self.home.conversation) {
            error!("Failed to update conversation meta: {}", e);
        }
    }

    /// Switch the active provider/model to match a loaded conversation's session config.
    async fn switch_to_session_provider(
        &mut self,
        conversation: &crate::model::conversation::Conversation,
    ) {
        let current_name = self.source_name().to_string();
        let current_model = self.model_name().to_string();
        let needs_provider_switch = conversation.provider != current_name;
        let needs_model_switch = conversation.model != current_model;

        if needs_provider_switch {
            if self
                .config
                .provider
                .iter()
                .any(|p| p.name == conversation.provider)
            {
                let mut new_config = self.config.clone();
                new_config.default_source = Some(conversation.provider.clone());
                match Provider::from_config_with_model(&new_config, &conversation.model).await {
                    Ok(new_provider) => {
                        self.source = ActiveSource::Provider(new_provider);
                        info!(
                            "Restored session provider {} / model {}",
                            conversation.provider, conversation.model
                        );
                    }
                    Err(e) => {
                        error!("Failed to restore session provider: {}", e);
                    }
                }
            } else {
                error!(
                    "Session provider '{}' not found in config, keeping current",
                    conversation.provider
                );
            }
        } else if needs_model_switch {
            if let Some(p) = self.provider_mut() {
                if let Err(e) = p.set_model(&conversation.model) {
                    error!("Failed to restore session model: {}", e);
                }
            }
        }

        // Update UI. Push the provider's live model list into the picker's tree
        // (whether explicit from config or just auto-discovered) so it doesn't
        // need a redundant fetch later.
        if let Some(p) = self.provider_ref() {
            let name = p.name().to_string();
            let models = p.models().to_vec();
            self.home.set_provider_models(&name, models);
        }
        let source_name = self.source_name().to_string();
        let model_name = self.model_name().to_string();
        self.home.set_active_source(&source_name, &model_name);
        if let Some(pc) = self
            .config
            .provider
            .iter()
            .find(|p| p.name == conversation.provider)
        {
            self.home.set_config_params(pc.model_params());
        }
    }

    /// Persist the session's config to the database (if the session has been saved).
    fn persist_session_config(&self) {
        if !self.conversation_saved || self.is_acp() {
            return;
        }
        if let Err(e) = self
            .store
            .update_conversation_config(&self.home.conversation)
        {
            error!("Failed to persist session config: {}", e);
        }
    }

    async fn load_session_list(&mut self) {
        if self.is_acp() {
            let source_name = self.source_name().to_string();
            let result = if let Some(acp) = self.acp_ref() {
                acp.list_sessions().await
            } else {
                return;
            };
            match result {
                Ok(sessions) => {
                    let summaries: Vec<SessionSummary> = sessions
                        .into_iter()
                        .map(|s| {
                            let updated = s
                                .updated_at
                                .as_deref()
                                .and_then(|ts| {
                                    chrono::DateTime::parse_from_rfc3339(ts)
                                        .ok()
                                        .map(|dt| dt.with_timezone(&chrono::Utc))
                                })
                                .unwrap_or_else(chrono::Utc::now);
                            SessionSummary {
                                id: SessionRef::Acp(s.id),
                                title: s.title,
                                provider: source_name.clone(),
                                model: "default".to_string(),
                                message_count: 0,
                                updated_at: updated,
                                cwd: Some(s.cwd),
                            }
                        })
                        .collect();
                    let _ = self.home.update(&Action::SessionsLoaded(summaries));
                }
                Err(e) => error!("Failed to list ACP sessions: {}", e),
            }
            return;
        }

        match self.store.list_conversations() {
            Ok(sessions) => {
                let summaries: Vec<SessionSummary> = sessions
                    .into_iter()
                    .map(|s| SessionSummary {
                        id: SessionRef::Local(s.id),
                        title: s.title,
                        provider: s.provider,
                        model: s.model,
                        message_count: s.message_count,
                        updated_at: s.updated_at,
                        cwd: None,
                    })
                    .collect();
                let _ = self.home.update(&Action::SessionsLoaded(summaries));
            }
            Err(e) => {
                error!("Failed to list sessions: {}", e);
            }
        }
    }

    async fn load_session(&mut self, id: &SessionRef) {
        match id {
            SessionRef::Local(uuid) => match self.store.load_conversation(uuid) {
                Ok(Some(conversation)) => {
                    self.conversation_saved = true;
                    self.switch_to_session_provider(&conversation).await;
                    let _ = self
                        .home
                        .update(&Action::SessionLoaded(Box::new(conversation)));
                    self.sync_provider_conversation_id();
                    self.update_context_window();
                }
                Ok(None) => {
                    error!("Session {} not found", uuid);
                }
                Err(e) => {
                    error!("Failed to load session {}: {}", uuid, e);
                }
            },
            SessionRef::Acp(acp_id) => {
                self.acp_loading = true;
                self.home.new_conversation();
                let source_name = self.source_name().to_string();
                let label = short_session_label(acp_id);
                self.home.conversation.provider = source_name.clone();
                self.home.conversation.model = label.clone();
                self.home.set_active_source(&source_name, &label);
                self.home.conversation.start_assistant_message();
                let load_result = if let Some(acp) = self.acp_ref() {
                    acp.load_session(acp_id.clone()).await
                } else {
                    return;
                };
                match load_result {
                    Ok(()) => {
                        if let Some(acp) = self.acp_ref() {
                            debug!("ACP loaded session_id={:?}", acp.session_id().await);
                        }
                    }
                    Err(e) => {
                        error!("Failed to load ACP session: {}", e);
                        self.acp_loading = false;
                    }
                }
            }
        }
    }

    fn check_auto_compact(&self) {
        if self.home.conversation.should_suggest_compaction() {
            let usage = self.home.conversation.context_usage();
            debug!(
                "Context usage at {:.0}% — auto-compaction threshold reached",
                usage * 100.0
            );
            info!(
                "Context window {:.0}% full. Press Ctrl+K to compact.",
                usage * 100.0
            );
        }
    }

    /// If the conversation title is still the default, spawn an async LLM call
    /// to generate a short title from the first user–assistant exchange.
    fn maybe_generate_title(&self) {
        if self.is_acp() {
            return;
        }
        if self.home.conversation.title != "New conversation" {
            return;
        }

        let Some(ref event_tx) = self.action_tx else {
            return;
        };

        // Find the first user message and first assistant message
        let user_text = self
            .home
            .conversation
            .messages
            .iter()
            .find(|m| m.role == crate::model::message::Role::User)
            .map(|m| m.text_content());
        let assistant_text = self
            .home
            .conversation
            .messages
            .iter()
            .find(|m| m.role == crate::model::message::Role::Assistant && !m.streaming)
            .map(|m| m.text_content());

        let (Some(user_text), Some(assistant_text)) = (user_text, assistant_text) else {
            return;
        };

        let Some(provider) = self.provider_ref() else {
            return;
        };
        let provider = provider.clone_for_compaction();
        let event_tx = event_tx.clone();

        tokio::spawn(async move {
            let event = match provider.generate_title(&user_text, &assistant_text).await {
                Ok((title, usage)) => Event::TitleGenerated(Ok((title, usage))),
                Err(e) => Event::TitleGenerated(Err(e.to_string())),
            };
            let _ = event_tx.send(event);
        });
    }

    fn start_compaction(&mut self) {
        if self.is_acp() {
            info!("Context compaction is not available for ACP agents");
            return;
        }
        let Some(ref event_tx) = self.action_tx else {
            return;
        };

        let (to_compact, _to_keep) = self.home.conversation.compaction_split(4);

        if to_compact.is_empty() {
            info!("Nothing to compact — too few messages");
            let _ = event_tx.send(Event::CompactionResult(Err(
                "Not enough messages to compact.".to_string(),
            )));
            return;
        }

        let compacted_ids: Vec<uuid::Uuid> = to_compact.iter().map(|m| m.id).collect();
        let messages_to_compact: Vec<Message> = to_compact.into_iter().cloned().collect();

        info!(
            "Compacting {} messages ({} tokens estimated)",
            messages_to_compact.len(),
            messages_to_compact
                .iter()
                .map(|m| m.estimate_tokens())
                .sum::<u32>()
        );

        let Some(provider) = self.provider_ref() else {
            return;
        };
        let provider = provider.clone_for_compaction();
        let event_tx = event_tx.clone();
        let cancel = self.cancellation_token.clone();

        tokio::spawn(async move {
            let event = tokio::select! {
                result = provider.compact_messages(&messages_to_compact) => {
                    match result {
                        Ok((summary, usage)) => Event::CompactionResult(Ok((summary, compacted_ids, usage))),
                        Err(e) => Event::CompactionResult(Err(e.to_string())),
                    }
                }
                _ = cancel.cancelled() => {
                    debug!("Compaction cancelled on shutdown");
                    Event::CompactionResult(Err("Compaction cancelled (application shutting down)".to_string()))
                }
            };
            let _ = event_tx.send(event);
        });
    }

    fn apply_compaction(&mut self, summary: &str, compacted_ids: &[uuid::Uuid]) {
        self.home
            .conversation
            .apply_compaction(compacted_ids, summary.to_string());

        if let Err(e) = self.store.mark_messages_compacted(compacted_ids) {
            error!("Failed to mark messages as compacted in DB: {}", e);
        }

        for msg in &self.home.conversation.messages {
            if msg.role == Role::System && !msg.compacted {
                if msg.text_content().starts_with("[Context compacted") {
                    if let Err(e) = self.store.save_message(&self.home.conversation.id, msg) {
                        error!("Failed to save compaction summary message: {}", e);
                    }
                    break;
                }
            }
        }

        self.update_conversation_meta();
    }

    /// Send the current conversation to the LLM provider (direct genai path only).
    fn send_to_provider(&self) {
        let Some(ref event_tx) = self.action_tx else {
            return;
        };

        let ActiveSource::Provider(provider) = &self.source else {
            return;
        };

        // Send only non-compacted, non-streaming messages as active context
        let messages = &self.home.conversation.messages;
        let send_messages: Vec<_> = messages
            .iter()
            .filter(|m| !m.streaming && !m.compacted)
            .cloned()
            .collect();

        // Combine built-in tools with MCP tools
        let mut tools = self.tool_registry.genai_tools();

        // Add MCP tools (need to block briefly to get the lock — MCP manager
        // is only mutated during startup, so this lock is uncontended during chat)
        if let Ok(mcp) = self.mcp_manager.try_lock() {
            tools.extend(mcp.genai_tools());
        }

        // Once saved, the session owns its params (already snapshotted).
        // Before save, merge config defaults with any in-memory overrides.
        let effective_params = if self.conversation_saved {
            self.home.conversation.param_overrides.clone()
        } else {
            let config_params = self
                .config
                .active_provider()
                .map(|p| p.model_params())
                .unwrap_or_default();
            config_params.merged_with(&self.home.conversation.param_overrides)
        };

        // Pre-resolve attachment data for any image content parts
        let attachment_ids: Vec<uuid::Uuid> = send_messages
            .iter()
            .flat_map(|m| {
                m.content.iter().filter_map(|p| match p {
                    ContentPart::Image { id, .. } => Some(*id),
                    _ => None,
                })
            })
            .collect();
        let attachments = if attachment_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.store
                .load_attachments(&attachment_ids)
                .unwrap_or_default()
        };

        provider.send_streaming_with_tools(
            &send_messages,
            tools,
            &effective_params,
            &attachments,
            &self.home.conversation.persona,
            event_tx.clone(),
            self.cancellation_token.clone(),
        );
    }

    async fn send_to_acp(&self) {
        let Some(acp) = self.acp_ref() else {
            return;
        };
        let Some(sid) = acp.session_id().await else {
            error!("No active ACP session");
            if let Some(tx) = &self.action_tx {
                let _ = tx.send(Event::StreamError("No active ACP session".to_string()));
            }
            return;
        };
        debug!("ACP prompt on session {}", sid);
        let text = self
            .home
            .conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.text_content())
            .unwrap_or_default();
        if let Err(e) = acp.prompt(text).await {
            error!("ACP prompt failed: {}", e);
            if let Some(tx) = &self.action_tx {
                let _ = tx.send(Event::StreamError(e.to_string()));
            }
        }
    }
}

/// Short label for an ACP session id (status bar / session list meta).
fn short_session_label(id: &str) -> String {
    if id.chars().count() <= 12 {
        id.to_string()
    } else {
        let head: String = id.chars().take(8).collect();
        format!("{head}…")
    }
}

/// Execute a tool by name (used in spawned tasks where we can't borrow the registry).
/// All built-in tools are stateless unit structs, so we reconstruct them here.
async fn execute_tool_by_name(
    name: &str,
    args: &serde_json::Value,
    cancel: tokio_util::sync::CancellationToken,
) -> crate::tools::ToolResult {
    use crate::tools::ToolDef;

    match name {
        "read_file" => {
            crate::tools::read_file::ReadFile
                .execute(args, cancel)
                .await
        }
        "write_file" => {
            crate::tools::write_file::WriteFile
                .execute(args, cancel)
                .await
        }
        "list_directory" => {
            crate::tools::list_directory::ListDirectory
                .execute(args, cancel)
                .await
        }
        "run_command" => {
            crate::tools::run_command::RunCommand
                .execute(args, cancel)
                .await
        }
        "web_fetch" => {
            crate::tools::web_fetch::WebFetch
                .execute(args, cancel)
                .await
        }
        "web_search" => {
            crate::tools::web_search::WebSearch
                .execute(args, cancel)
                .await
        }
        _ => crate::tools::ToolResult::Err(format!("Unknown tool: {}", name)),
    }
}
