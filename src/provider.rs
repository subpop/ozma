use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use chrono::Local;
use color_eyre::eyre::{Result, eyre};
use futures::StreamExt;
use genai::WebConfig;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart as GenaiContentPart,
    Tool as GenaiTool, ToolCall, ToolResponse,
};
use genai::resolver::{
    AuthData, Endpoint, ProviderConfig as GenaiProviderConfig, ServiceTargetResolver,
};
use genai::{Client, ModelIden, ServiceTarget};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::config::{ModelParams, ProviderConfig, WireProtocol};
use crate::event::Event;
use crate::model::message::{ContentPart, Message, Role, TokenUsage, ToolCallInfo};
use crate::persona::Persona;

/// Wraps a `genai::Client` and the active model/config. Handles building chat requests
/// from our conversation model and spawning streaming tasks that feed chunks back
/// through the TUI action channel.
pub struct Provider {
    /// The genai client (handles HTTP, auth, SSE parsing internally).
    client: Client,
    /// The currently active model identifier.
    model: String,
    /// All available models for this provider.
    models: Vec<String>,
    /// The provider display name.
    name: String,
    /// Retained so we can rebuild the client when the conversation ID changes.
    provider_config: ProviderConfig,
}

fn effective_adapter_kind(config: &ProviderConfig) -> Result<AdapterKind> {
    let base = AdapterKind::from_lower_str(&config.adapter)
        .ok_or_else(|| eyre!("Unknown adapter: {}", config.adapter))?;

    match config.wire_protocol {
        None | Some(WireProtocol::Auto) => Ok(base),
        Some(WireProtocol::ChatCompletions) => Ok(AdapterKind::OpenAI),
        Some(WireProtocol::Responses) => Ok(AdapterKind::OpenAIResp),
        Some(WireProtocol::Messages) => Ok(AdapterKind::Anthropic),
    }
}

/// Query a provider's API to discover its available model names.
/// Best-effort: returns an empty list (logged) on any failure rather than propagating
/// an error, since this is used for optional UI convenience (model auto-discovery).
pub(crate) async fn discover_models(config: &ProviderConfig) -> Vec<String> {
    let adapter_kind = match effective_adapter_kind(config) {
        Ok(kind) => kind,
        Err(e) => {
            info!("Cannot discover models for '{}': {}", config.name, e);
            return vec![];
        }
    };
    debug!(
        "Discovering models for provider='{}' adapter='{}' wire_protocol={:?} effective_adapter='{}'",
        config.name,
        config.adapter,
        config.wire_protocol,
        adapter_kind.as_lower_str()
    );
    let client = match build_client(config, None) {
        Ok(c) => c,
        Err(e) => {
            info!("Could not build client for '{}': {}", config.name, e);
            return vec![];
        }
    };
    // `all_model_names` does not run the service-target resolver, so resolve
    // auth/endpoint first (runs api_key_cmd, applies base_url) and pass them
    // explicitly.
    let target = match client
        .resolve_service_target(ModelIden::new(adapter_kind, ""))
        .await
    {
        Ok(t) => t,
        Err(e) => {
            info!("Could not resolve auth for '{}': {}", config.name, e);
            return vec![];
        }
    };
    let provider_cfg = GenaiProviderConfig::from_auth(target.auth).with_endpoint(target.endpoint);
    client
        .all_model_names(adapter_kind, provider_cfg)
        .await
        .unwrap_or_else(|e| {
            info!("Could not fetch model list for '{}': {}", config.name, e);
            vec![]
        })
}

impl Provider {
    /// Build a Provider from the application config.
    /// When `models` is not set in config, queries the provider API to discover them.
    pub async fn from_config(config: &crate::config::Config) -> Result<Self> {
        let provider_config = config
            .active_provider()
            .ok_or_else(|| eyre!("No provider configured"))?;

        let client = build_client(provider_config, None)?;

        let models = match &provider_config.models {
            Some(list) => list.clone(),
            None => {
                info!(
                    "No models configured for '{}', querying provider...",
                    provider_config.name
                );
                discover_models(provider_config).await
            }
        };

        let model = provider_config
            .default_model()
            .map(|s| s.to_string())
            .or_else(|| models.first().cloned())
            .ok_or_else(|| {
                eyre!(
                    "No models available for provider '{}'",
                    provider_config.name
                )
            })?;

        info!(
            "Provider initialized: {} (model: {}, {} available)",
            provider_config.name,
            model,
            models.len()
        );

        Ok(Self {
            client,
            model,
            models,
            name: provider_config.name.clone(),
            provider_config: provider_config.clone(),
        })
    }

    /// Build a Provider from config, overriding the active model.
    /// The config's `default_source` is used to select the provider.
    pub async fn from_config_with_model(
        config: &crate::config::Config,
        model: &str,
    ) -> Result<Self> {
        let mut provider = Self::from_config(config).await?;
        // Accept the override regardless of the fetched list — the model may be
        // valid even if it wasn't returned by the listing endpoint (e.g. aliases).
        provider.model = model.to_string();
        Ok(provider)
    }

    /// The display name of the active provider.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The active model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// All known models for this provider.
    pub fn models(&self) -> &[String] {
        &self.models
    }

    /// Switch to a different model. Validated against the known list when non-empty.
    pub fn set_model(&mut self, model: &str) -> Result<()> {
        if !self.models.is_empty() && !self.models.iter().any(|m| m == model) {
            return Err(eyre!(
                "Model '{}' not available for provider '{}'",
                model,
                self.name
            ));
        }
        info!(
            "Switching model: {} -> {} (provider: {})",
            self.model, model, self.name
        );
        self.model = model.to_string();
        Ok(())
    }

    /// Rebuild the genai client with the given conversation ID so that
    /// adapter-specific headers (e.g. `x-opencode-session`) are bound to this
    /// conversation for the rest of its lifetime.
    pub fn set_conversation_id(&mut self, id: Uuid) -> Result<()> {
        self.client = build_client(&self.provider_config, Some(id))?;
        Ok(())
    }

    /// Create a lightweight clone for use in compaction (background task).
    pub fn clone_for_compaction(&self) -> Self {
        Self {
            client: self.client.clone(),
            model: self.model.clone(),
            models: self.models.clone(),
            name: self.name.clone(),
            provider_config: self.provider_config.clone(),
        }
    }

    /// Send a streaming chat request with tool definitions.
    /// Spawns a tokio task that reads the stream and sends events:
    /// - `StreamChunk` for text content
    /// - `StreamDone` when complete with no tool calls
    /// - `StreamDoneWithToolCalls` when the LLM requests tool use
    /// - `StreamError` on errors
    pub fn send_streaming_with_tools(
        &self,
        messages: &[Message],
        tools: Vec<GenaiTool>,
        params: &ModelParams,
        attachments: &HashMap<Uuid, (String, Vec<u8>)>,
        persona: &Persona,
        event_tx: UnboundedSender<Event>,
        cancel: CancellationToken,
    ) {
        let mut chat_req = self.build_chat_request(messages, attachments, persona);

        // Attach tools if any are registered
        if !tools.is_empty() {
            chat_req = chat_req.with_tools(tools);
        }

        match effective_adapter_kind(&self.provider_config) {
            Ok(kind) => debug!(
                "Preparing stream for provider='{}' model='{}' adapter='{}' wire_protocol={:?} effective_adapter='{}'",
                self.name,
                self.model,
                self.provider_config.adapter,
                self.provider_config.wire_protocol,
                kind.as_lower_str()
            ),
            Err(err) => debug!(
                "Preparing stream for provider='{}' model='{}' failed to resolve effective adapter: {}",
                self.name, self.model, err
            ),
        }

        let model = self.model.clone();
        let client = self.client.clone();
        let params = params.clone();

        tokio::spawn(async move {
            let mut options = ChatOptions::default().with_capture_usage(true);
            if let Some(t) = params.temperature {
                options = options.with_temperature(t);
            }
            if let Some(p) = params.top_p {
                options = options.with_top_p(p);
            }
            if let Some(m) = params.max_tokens {
                options = options.with_max_tokens(m);
            }

            debug!("Sending chat request to model: {} (with tools)", model);

            // Use select! so we can abort the HTTP connection if cancelled
            let stream_res = tokio::select! {
                res = client.exec_chat_stream(&model, chat_req, Some(&options)) => {
                    match res {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Failed to start chat stream: {}", e);
                            let _ = event_tx.send(Event::StreamError(format!(
                                "Failed to connect: {}",
                                e
                            )));
                            return;
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    debug!("LLM stream cancelled before connection established");
                    return;
                }
            };

            let mut stream = stream_res.stream;
            let mut tool_calls_by_id: HashMap<String, ToolCallInfo> = HashMap::new();
            let mut tool_call_order: Vec<String> = Vec::new();

            loop {
                tokio::select! {
                    maybe_event = stream.next() => {
                        let Some(event) = maybe_event else {
                            // Stream ended without an explicit End event
                            break;
                        };
                        match event {
                            Ok(ChatStreamEvent::Chunk(chunk)) => {
                                let _ = event_tx.send(Event::StreamChunk(chunk.content));
                            }
                            Ok(ChatStreamEvent::ToolCallChunk(tool_chunk)) => {
                                let tc = tool_chunk.tool_call;
                                debug!(
                                    "Tool call chunk: {} (call_id: {})",
                                    tc.fn_name, tc.call_id
                                );

                                let call_id = tc.call_id;
                                let fn_name = tc.fn_name;
                                let fn_arguments = normalize_tool_arguments(tc.fn_arguments);

                                if let Some(existing) = tool_calls_by_id.get_mut(&call_id) {
                                    if !fn_name.is_empty() {
                                        existing.fn_name = fn_name;
                                    }
                                    existing.fn_arguments = merge_tool_arguments(
                                        existing.fn_arguments.clone(),
                                        fn_arguments,
                                    );
                                } else {
                                    tool_call_order.push(call_id.clone());
                                    tool_calls_by_id.insert(
                                        call_id.clone(),
                                        ToolCallInfo {
                                            call_id,
                                            fn_name,
                                            fn_arguments,
                                        },
                                    );
                                }
                            }
                            Ok(ChatStreamEvent::End(end)) => {
                                let usage = end.captured_usage.map(TokenUsage::from);
                                if let Some(ref u) = usage {
                                    debug!(
                                        "Stream complete. Tokens: prompt={:?}, completion={:?}, total={:?}",
                                        u.prompt_tokens,
                                        u.completion_tokens,
                                        u.total_tokens
                                    );
                                }

                                let accumulated_tool_calls: Vec<ToolCallInfo> = tool_call_order
                                    .iter()
                                    .filter_map(|id| tool_calls_by_id.get(id).cloned())
                                    .collect();

                                if accumulated_tool_calls.is_empty() {
                                    let _ = event_tx.send(Event::StreamDone { usage });
                                } else {
                                    info!(
                                        "Stream completed with {} tool call(s)",
                                        accumulated_tool_calls.len()
                                    );
                                    let _ = event_tx
                                        .send(Event::StreamDoneWithToolCalls { tool_calls: accumulated_tool_calls, usage });
                                }
                                return;
                            }
                            Ok(ChatStreamEvent::Start) => {
                                debug!("Stream started");
                            }
                            Ok(_) => {
                                // ReasoningChunk, ThoughtSignatureChunk, etc.
                            }
                            Err(e) => {
                                error!("Stream error: {}", e);
                                let _ = event_tx.send(Event::StreamError(e.to_string()));
                                return;
                            }
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!("LLM stream cancelled — dropping connection");
                        // Dropping `stream` here closes the HTTP connection
                        return;
                    }
                }
            }
        });
    }

    /// Build a ChatRequest from conversation messages, including tool call/result messages.
    /// `attachments` maps attachment UUIDs to (mime_type, raw_bytes) for image content parts.
    fn build_chat_request(
        &self,
        messages: &[Message],
        attachments: &HashMap<Uuid, (String, Vec<u8>)>,
        persona: &Persona,
    ) -> ChatRequest {
        let mut chat_messages: Vec<ChatMessage> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    chat_messages.push(ChatMessage::system(msg.text_content()));
                }
                Role::User => {
                    if msg.has_images() {
                        // Multi-part: convert each ContentPart to a genai ContentPart
                        let parts: Vec<GenaiContentPart> = msg
                            .content
                            .iter()
                            .filter_map(|part| match part {
                                ContentPart::Text { text } => {
                                    if text.is_empty() {
                                        None
                                    } else {
                                        Some(GenaiContentPart::from_text(text.clone()))
                                    }
                                }
                                ContentPart::Image {
                                    id, mime_type, alt, ..
                                } => {
                                    if let Some((_, data)) = attachments.get(id) {
                                        let b64: Arc<str> =
                                            base64::engine::general_purpose::STANDARD
                                                .encode(data)
                                                .into();
                                        let name = alt.clone();
                                        Some(GenaiContentPart::from_binary_base64(
                                            mime_type.clone(),
                                            b64,
                                            name,
                                        ))
                                    } else {
                                        debug!("Attachment {} not found, skipping", id);
                                        None
                                    }
                                }
                            })
                            .collect();
                        if parts.is_empty() {
                            chat_messages.push(ChatMessage::user(""));
                        } else {
                            chat_messages.push(ChatMessage::user(parts));
                        }
                    } else {
                        chat_messages.push(ChatMessage::user(msg.text_content()));
                    }
                }
                Role::Assistant => {
                    if msg.has_tool_calls() {
                        let tool_calls: Vec<ToolCall> = msg
                            .tool_calls
                            .iter()
                            .map(|tc| ToolCall {
                                call_id: tc.call_id.clone(),
                                fn_name: tc.fn_name.clone(),
                                fn_arguments: tc.fn_arguments.clone(),
                                thought_signatures: None,
                            })
                            .collect();
                        chat_messages.push(ChatMessage::from(tool_calls));
                    } else {
                        chat_messages.push(ChatMessage::assistant(msg.text_content()));
                    }
                }
                Role::Tool => {
                    if let Some(ref result_info) = msg.tool_result {
                        let response = ToolResponse::new(&result_info.call_id, msg.text_content())
                            .with_fn_name(&result_info.fn_name);
                        chat_messages.push(ChatMessage::from(response));
                    }
                }
            }
        }

        let mut req = ChatRequest::from_messages(chat_messages);

        let system = build_system_prompt(persona);
        req = req.with_system(system.as_str());

        req
    }

    /// Send a compaction request: summarize the given messages into a condensed summary.
    /// Returns (summary_text, usage).
    pub async fn compact_messages(
        &self,
        messages: &[Message],
    ) -> Result<(String, Option<TokenUsage>)> {
        let mut chat_messages: Vec<ChatMessage> = Vec::new();

        for msg in messages {
            let text = msg.text_content();
            let chat_msg = match msg.role {
                Role::System => ChatMessage::system(text),
                Role::User => ChatMessage::user(text),
                Role::Assistant => ChatMessage::assistant(text),
                Role::Tool => continue, // Skip tool messages in compaction
            };
            chat_messages.push(chat_msg);
        }

        chat_messages.push(ChatMessage::user(
            "Please provide a comprehensive summary of the conversation above. \
             Capture all key decisions, facts, code snippets, file paths, \
             technical details, and action items discussed. \
             The summary will replace these messages in the conversation context, \
             so it must preserve all important information. \
             Be thorough but concise. Output only the summary, no preamble.",
        ));

        let req = ChatRequest::from_messages(chat_messages);

        let options = ChatOptions::default()
            .with_capture_content(true)
            .with_capture_usage(true);

        debug!("Sending compaction request ({} messages)", messages.len());

        let res = self
            .client
            .exec_chat(&self.model, req, Some(&options))
            .await
            .map_err(|e| eyre!("Compaction request failed: {}", e))?;

        let summary = res
            .first_text()
            .ok_or_else(|| eyre!("Compaction returned no text"))?
            .to_string();

        let usage = TokenUsage::from(res.usage);

        info!("Compaction complete: {} chars summary", summary.len());
        Ok((summary, Some(usage)))
    }

    /// Generate a short title for a conversation from the first user–assistant exchange.
    /// Returns (title, usage).
    pub async fn generate_title(
        &self,
        user_text: &str,
        assistant_text: &str,
    ) -> Result<(String, Option<TokenUsage>)> {
        let chat_messages = vec![
            ChatMessage::user(user_text.to_string()),
            ChatMessage::assistant(assistant_text.to_string()),
            ChatMessage::user(
                "Generate a short title (max 6 words) for this conversation. \
                 Output only the title, no quotes, no punctuation at the end.",
            ),
        ];

        let req = ChatRequest::from_messages(chat_messages);
        let options = ChatOptions::default()
            .with_capture_content(true)
            .with_capture_usage(true);

        debug!("Sending title generation request");

        let res = self
            .client
            .exec_chat(&self.model, req, Some(&options))
            .await
            .map_err(|e| eyre!("Title generation failed: {}", e))?;

        let title = res
            .first_text()
            .ok_or_else(|| eyre!("Title generation returned no text"))?
            .trim()
            .to_string();

        let usage = TokenUsage::from(res.usage);

        info!("Generated title: {}", title);
        Ok((title, Some(usage)))
    }
}

fn build_system_prompt(persona: &Persona) -> String {
    let env = build_environment_context();
    format!("{}\n\n{}", persona.system_prompt(), env)
}

fn build_environment_context() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let platform = std::env::consts::OS;
    let date = Local::now().format("%Y-%m-%d");

    let git_info = std::env::current_dir()
        .ok()
        .map(|p| p.join(".git").join("HEAD"))
        .and_then(|head_path| std::fs::read_to_string(head_path).ok())
        .map(|contents| {
            let contents = contents.trim();
            if let Some(branch) = contents.strip_prefix("ref: refs/heads/") {
                format!("yes (branch: {})", branch)
            } else {
                "yes (detached HEAD)".to_string()
            }
        })
        .unwrap_or_else(|| "no".to_string());

    format!(
        "Working directory: {}\nPlatform: {}\nDate: {}\nGit repo: {}",
        cwd, platform, date, git_info
    )
}

fn normalize_tool_arguments(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::String(s))
        }
        other => other,
    }
}

fn merge_tool_arguments(
    existing: serde_json::Value,
    incoming: serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;

    if !matches!(incoming, Value::String(_)) {
        return incoming;
    }

    match (existing, incoming) {
        // Some providers stream arguments as JSON-string fragments.
        (Value::String(mut prev), Value::String(next)) => {
            prev.push_str(&next);
            serde_json::from_str::<Value>(&prev).unwrap_or(Value::String(prev))
        }

        // Keep parsed structured arguments instead of replacing with a fragment.
        (Value::Object(prev), Value::String(_)) => Value::Object(prev),
        (Value::Array(prev), Value::String(_)) => Value::Array(prev),

        // Fallback: prefer the most recent value.
        (_, other) => other,
    }
}

/// Build the `HeaderMap` for a provider: User-Agent, then adapter-specific
/// headers (e.g. `x-opencode-session` for OpenCode Go), then any
/// user-configured `additional_headers` on top.
fn build_headers(
    config: &ProviderConfig,
    conversation_id: Option<Uuid>,
) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();

    // Set a User-Agent header with the application name and version (e.g. "ozma/0.1.0")
    map.insert(
        reqwest::header::USER_AGENT,
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"))
            .parse()
            .unwrap(),
    );

    // Add any adapter-specific headers.
    match config.adapter.as_str() {
        "opencode_go" => {
            // Inject the conversation UUID for the OpenCode Go proxy so it can optimise
            // routing and prompt-cache affinity. Omitted until the conversation ID is
            // known (i.e. on initial client construction before a session is set).
            if let Some(id) = conversation_id {
                map.insert("x-opencode-session", id.to_string().parse().unwrap());
            }
        }
        _ => {}
    }

    for (key, value) in &config.additional_headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            map.insert(name, val);
        }
    }

    map
}

/// Run a shell command to retrieve an API key, returning its trimmed stdout.
/// Runs via `sh -c` (`cmd /C` on Windows), matching the shell used by the `run_command` tool.
async fn run_api_key_cmd(cmd: &str) -> std::result::Result<String, String> {
    debug!("Running api_key_cmd: {}", cmd);

    let shell = if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };

    let output = tokio::process::Command::new(shell.0)
        .arg(shell.1)
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("Failed to run api_key_cmd '{}': {}", cmd, e))?;

    if !output.status.success() {
        return Err(format!(
            "api_key_cmd '{}' exited with {}: {}",
            cmd,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if key.is_empty() {
        return Err(format!("api_key_cmd '{}' produced empty output", cmd));
    }

    Ok(key)
}

/// Build a `genai::Client` from a provider config.
fn build_client(config: &ProviderConfig, conversation_id: Option<Uuid>) -> Result<Client> {
    let configured_adapter_kind = AdapterKind::from_lower_str(&config.adapter)
        .ok_or_else(|| eyre!("Unknown adapter: {}", config.adapter))?;
    let adapter_kind = effective_adapter_kind(config)?;
    let preserve_adapter_defaults = configured_adapter_kind != adapter_kind;

    debug!(
        "Building client for provider='{}' adapter='{}' wire_protocol={:?} effective_adapter='{}' conversation_id={} preserve_adapter_defaults={}",
        config.name,
        config.adapter,
        config.wire_protocol,
        adapter_kind.as_lower_str(),
        conversation_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        preserve_adapter_defaults
    );

    let mut builder = Client::builder();

    builder = builder
        .with_web_config(
            WebConfig::default().with_default_headers(build_headers(config, conversation_id)),
        )
        // Bind the adapter so genai uses its correct defaults for endpoint and auth
        // when resolving model names that don't carry an explicit namespace.
        .with_adapter_kind(adapter_kind);

    // Install a resolver when there is something to override, or when
    // wire_protocol remaps the effective adapter and we must preserve the
    // configured adapter defaults for endpoint/auth.
    if config.base_url.is_some()
        || config.api_key_env.is_some()
        || config.api_key_cmd.is_some()
        || preserve_adapter_defaults
    {
        let base_url = config.base_url.clone();
        let api_key_env = config.api_key_env.clone();
        let api_key_cmd = config.api_key_cmd.clone();
        let adapter_default_auth_env = configured_adapter_kind
            .default_key_env_name()
            .map(ToString::to_string);
        let adapter_default_base_url = if preserve_adapter_defaults && config.base_url.is_none() {
            match config.adapter.as_str() {
                "opencode_go" => Some("https://opencode.ai/zen/go/v1/".to_string()),
                _ => None,
            }
        } else {
            None
        };

        let resolver = ServiceTargetResolver::from_resolver_async_fn(
            move |st: ServiceTarget| -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = genai::resolver::Result<ServiceTarget>> + Send,
                >,
            > {
                let base_url = base_url.clone();
                let api_key_env = api_key_env.clone();
                let api_key_cmd = api_key_cmd.clone();
                let adapter_default_auth_env = adapter_default_auth_env.clone();
                let adapter_default_base_url = adapter_default_base_url.clone();

                Box::pin(async move {
                    let endpoint = match base_url {
                        Some(url) => Endpoint::from_owned(url),
                        None => match adapter_default_base_url {
                            Some(url) => Endpoint::from_owned(url),
                            None => st.endpoint,
                        },
                    };
                    let auth = if let Some(cmd) = api_key_cmd {
                        let key = run_api_key_cmd(&cmd)
                            .await
                            .map_err(genai::resolver::Error::Custom)?;
                        AuthData::from_single(key)
                    } else {
                        match api_key_env {
                            Some(env_name) => AuthData::from_env(env_name),
                            None => match adapter_default_auth_env {
                                Some(env_name) => AuthData::from_env(env_name),
                                None => st.auth,
                            },
                        }
                    };
                    Ok(ServiceTarget {
                        endpoint,
                        auth,
                        ..st
                    })
                })
            },
        );

        builder = builder.with_service_target_resolver(resolver);
    }

    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_config(adapter: &str, wire_protocol: Option<WireProtocol>) -> ProviderConfig {
        ProviderConfig {
            name: "test".to_string(),
            models: Some(vec!["model".to_string()]),
            api_key_env: None,
            api_key_cmd: None,
            base_url: None,
            adapter: adapter.to_string(),
            wire_protocol,
            temperature: None,
            top_p: None,
            max_tokens: None,
            additional_headers: HashMap::new(),
        }
    }

    #[test]
    fn effective_adapter_kind_defaults_to_adapter() {
        let config = provider_config("opencode_go", None);
        assert_eq!(
            effective_adapter_kind(&config).unwrap(),
            AdapterKind::OpenCodeGo
        );
    }

    #[test]
    fn effective_adapter_kind_supports_opencode_protocol_overrides() {
        let config = provider_config("opencode_go", Some(WireProtocol::ChatCompletions));
        assert_eq!(
            effective_adapter_kind(&config).unwrap(),
            AdapterKind::OpenAI
        );

        let config = provider_config("opencode_go", Some(WireProtocol::Responses));
        assert_eq!(
            effective_adapter_kind(&config).unwrap(),
            AdapterKind::OpenAIResp
        );

        let config = provider_config("opencode_go", Some(WireProtocol::Messages));
        assert_eq!(
            effective_adapter_kind(&config).unwrap(),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn effective_adapter_kind_allows_wire_protocol_for_other_adapters() {
        let config = provider_config("openai", Some(WireProtocol::Responses));
        assert_eq!(
            effective_adapter_kind(&config).unwrap(),
            AdapterKind::OpenAIResp
        );
    }

    #[tokio::test]
    async fn run_api_key_cmd_returns_trimmed_stdout() {
        let key = run_api_key_cmd("echo mysecret").await.unwrap();
        assert_eq!(key, "mysecret");
    }

    #[tokio::test]
    async fn run_api_key_cmd_errors_on_nonzero_exit() {
        let err = run_api_key_cmd("exit 1").await.unwrap_err();
        assert!(err.contains("exited with"));
    }

    #[tokio::test]
    async fn run_api_key_cmd_errors_on_empty_output() {
        let err = run_api_key_cmd("true").await.unwrap_err();
        assert!(err.contains("empty output"));
    }
}
