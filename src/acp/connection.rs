//! Long-lived ACP agent connection driven by a command channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, CreateTerminalRequest,
    CreateTerminalResponse, DeleteSessionRequest, EnvVariable, FileSystemCapabilities,
    Implementation, InitializeRequest, KillTerminalRequest, KillTerminalResponse,
    ListSessionsRequest, LoadSessionRequest, NewSessionRequest, PermissionOptionKind,
    PromptRequest, ReadTextFileRequest, ReadTextFileResponse, ReleaseTerminalRequest,
    ReleaseTerminalResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    TerminalExitStatus, TerminalId, TerminalOutputRequest, TerminalOutputResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo};
use color_eyre::eyre::{Result, eyre};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::acp::map::session_update_to_events;
use crate::acp::mcp::mcp_servers_from_config;
use crate::config::{AgentConfig, McpServerConfig, ToolsConfig};
use crate::event::Event;
use crate::tools::PermissionLevel;

/// Summary row for ACP session/list.
#[derive(Clone, Debug)]
pub struct AcpSessionSummary {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub updated_at: Option<String>,
}

enum AcpCommand {
    NewSession {
        reply: oneshot::Sender<Result<String>>,
    },
    Prompt {
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Cancel,
    ListSessions {
        reply: oneshot::Sender<Result<Vec<AcpSessionSummary>>>,
    },
    LoadSession {
        id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    DeleteSession {
        id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    RespondPermission {
        request_id: String,
        approve: bool,
        approve_always: bool,
    },
    Shutdown,
}

struct PendingPermission {
    options: Vec<(String, PermissionOptionKind)>,
    reply: oneshot::Sender<RequestPermissionOutcome>,
}

struct TerminalState {
    child: Child,
    output: Arc<Mutex<String>>,
    byte_limit: usize,
    exit_status: Arc<Mutex<Option<TerminalExitStatus>>>,
}

/// Handle to a running ACP agent process + JSON-RPC connection.
pub struct AcpConnection {
    name: String,
    cmd_tx: mpsc::UnboundedSender<AcpCommand>,
    session_id: Arc<Mutex<Option<String>>>,
    /// Shared with the connection task for permission replies.
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
}

impl AcpConnection {
    /// Spawn the agent subprocess and initialize the ACP connection.
    pub async fn connect(
        agent: &AgentConfig,
        mcp_servers: Vec<McpServerConfig>,
        tools: ToolsConfig,
        event_tx: UnboundedSender<Event>,
    ) -> Result<Self> {
        let name = agent.name.clone();
        let cwd = agent
            .cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(cwd)
        };

        let acp_agent = build_acp_agent(agent)?;

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AcpCommand>();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();
        let session_id = Arc::new(Mutex::new(None));
        let pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let terminals: Arc<Mutex<HashMap<String, TerminalState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let session_id_task = session_id.clone();
        let pending_task = pending_permissions.clone();
        let event_tx_notif = event_tx.clone();
        let event_tx_perm = event_tx.clone();
        let pending_perm = pending_permissions.clone();
        let terminals_fs = terminals.clone();
        let tools_cfg = tools;

        tokio::spawn(async move {
            let pending_for_perm = pending_perm.clone();
            let event_for_perm = event_tx_perm.clone();
            let tools_for_perm = tools_cfg;
            let terminals_create = terminals_fs.clone();
            let terminals_output = terminals_fs.clone();
            let terminals_wait = terminals_fs.clone();
            let terminals_kill = terminals_fs.clone();
            let terminals_release = terminals_fs.clone();

            let result = Client
                .builder()
                .name("ozma")
                .on_receive_notification(
                    {
                        let event_tx = event_tx_notif.clone();
                        async move |notification: SessionNotification, _cx| {
                            for ev in session_update_to_events(notification.update) {
                                let _ = event_tx.send(ev);
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    {
                        let pending = pending_for_perm;
                        let event_tx = event_for_perm;
                        let tools = tools_for_perm;
                        async move |request: RequestPermissionRequest, responder, _cx| {
                            let request_id = uuid::Uuid::new_v4().to_string();
                            let tool_name = request
                                .tool_call
                                .fields
                                .title
                                .clone()
                                .unwrap_or_else(|| "tool".to_string());
                            let call_id = request.tool_call.tool_call_id.0.to_string();
                            let kind = request
                                .tool_call
                                .fields
                                .kind
                                .as_ref()
                                .map(|k| format!("{k:?}"))
                                .unwrap_or_default();
                            let permission_level = map_tool_kind_to_permission(&kind);

                            let options: Vec<(String, PermissionOptionKind)> = request
                                .options
                                .iter()
                                .map(|o| (o.option_id.0.to_string(), o.kind.clone()))
                                .collect();

                            if should_auto_approve(&permission_level, &tools) {
                                let outcome = resolve_permission_outcome(&options, true, false);
                                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                                return Ok(());
                            }

                            let (reply_tx, reply_rx) = oneshot::channel();
                            pending.lock().await.insert(
                                request_id.clone(),
                                PendingPermission {
                                    options,
                                    reply: reply_tx,
                                },
                            );

                            let _ = event_tx.send(Event::AcpPermissionRequest {
                                request_id: request_id.clone(),
                                call_id,
                                tool_name,
                                permission_level,
                            });

                            tokio::spawn(async move {
                                let outcome = reply_rx
                                    .await
                                    .unwrap_or(RequestPermissionOutcome::Cancelled);
                                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                            });
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        async move |request: ReadTextFileRequest, responder, _cx| {
                            match std::fs::read_to_string(&request.path) {
                                Ok(mut content) => {
                                    if let Some(limit) = request.limit {
                                        let lines: Vec<&str> = content.lines().collect();
                                        let start =
                                            request.line.unwrap_or(1).saturating_sub(1) as usize;
                                        let end = (start + limit as usize).min(lines.len());
                                        content = lines.get(start..end).unwrap_or(&[]).join("\n");
                                    }
                                    let _ = responder.respond(ReadTextFileResponse::new(content));
                                }
                                Err(e) => {
                                    let _ = responder.respond(ReadTextFileResponse::new(format!(
                                        "Error reading file: {e}"
                                    )));
                                }
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        async move |request: WriteTextFileRequest, responder, _cx| {
                            match std::fs::write(&request.path, &request.content) {
                                Ok(()) => {
                                    let _ = responder.respond(WriteTextFileResponse::new());
                                }
                                Err(e) => {
                                    warn!("fs/write_text_file failed: {e}");
                                    let _ = responder.respond(WriteTextFileResponse::new());
                                }
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        let terminals = terminals_create;
                        async move |request: CreateTerminalRequest, responder, _cx| {
                            let id = uuid::Uuid::new_v4().to_string();
                            let mut cmd = Command::new(&request.command);
                            cmd.args(&request.args);
                            for env in &request.env {
                                cmd.env(&env.name, &env.value);
                            }
                            if let Some(ref cwd) = request.cwd {
                                cmd.current_dir(cwd);
                            }
                            cmd.stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .stdin(std::process::Stdio::null())
                                .kill_on_drop(true);

                            match cmd.spawn() {
                                Ok(mut child) => {
                                    let output = Arc::new(Mutex::new(String::new()));
                                    let exit_status = Arc::new(Mutex::new(None));
                                    let byte_limit =
                                        request.output_byte_limit.unwrap_or(1024 * 1024) as usize;
                                    let out_clone = output.clone();
                                    let exit_clone = exit_status.clone();
                                    if let Some(mut stdout) = child.stdout.take() {
                                        let out = out_clone.clone();
                                        let limit = byte_limit;
                                        tokio::spawn(async move {
                                            let mut buf = [0u8; 4096];
                                            loop {
                                                match stdout.read(&mut buf).await {
                                                    Ok(0) => break,
                                                    Ok(n) => {
                                                        let mut o = out.lock().await;
                                                        o.push_str(&String::from_utf8_lossy(
                                                            &buf[..n],
                                                        ));
                                                        if o.len() > limit {
                                                            let drain = o.len() - limit;
                                                            o.drain(..drain);
                                                        }
                                                    }
                                                    Err(_) => break,
                                                }
                                            }
                                        });
                                    }
                                    if let Some(mut stderr) = child.stderr.take() {
                                        let out = out_clone;
                                        let limit = byte_limit;
                                        tokio::spawn(async move {
                                            let mut buf = [0u8; 4096];
                                            loop {
                                                match stderr.read(&mut buf).await {
                                                    Ok(0) => break,
                                                    Ok(n) => {
                                                        let mut o = out.lock().await;
                                                        o.push_str(&String::from_utf8_lossy(
                                                            &buf[..n],
                                                        ));
                                                        if o.len() > limit {
                                                            let drain = o.len() - limit;
                                                            o.drain(..drain);
                                                        }
                                                    }
                                                    Err(_) => break,
                                                }
                                            }
                                        });
                                    }
                                    // Keep child in the map; poll via try_wait in output/wait handlers.
                                    terminals.lock().await.insert(
                                        id.clone(),
                                        TerminalState {
                                            child,
                                            output,
                                            byte_limit,
                                            exit_status,
                                        },
                                    );
                                    let _ = exit_clone; // kept in TerminalState
                                    let _ = responder.respond(CreateTerminalResponse::new(
                                        TerminalId::new(id.clone()),
                                    ));
                                }
                                Err(e) => {
                                    error!("terminal/create failed: {e}");
                                    let _ = responder
                                        .respond(CreateTerminalResponse::new(TerminalId::new(id)));
                                }
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        let terminals = terminals_output;
                        async move |request: TerminalOutputRequest, responder, _cx| {
                            let id = request.terminal_id.0.to_string();
                            let (output, exit) = {
                                let mut map = terminals.lock().await;
                                if let Some(term) = map.get_mut(&id) {
                                    // Poll exit
                                    if term.exit_status.lock().await.is_none() {
                                        if let Ok(Some(status)) = term.child.try_wait() {
                                            *term.exit_status.lock().await = Some(
                                                TerminalExitStatus::new()
                                                    .exit_code(status.code().map(|c| c as u32)),
                                            );
                                        }
                                    }
                                    (
                                        {
                                            let mut output = term.output.lock().await.clone();
                                            if output.len() > term.byte_limit {
                                                let drain = output.len() - term.byte_limit;
                                                output.drain(..drain);
                                            }
                                            output
                                        },
                                        term.exit_status.lock().await.clone(),
                                    )
                                } else {
                                    (String::new(), None)
                                }
                            };
                            let _ = responder.respond(
                                TerminalOutputResponse::new(output, false).exit_status(exit),
                            );
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        let terminals = terminals_wait;
                        async move |request: WaitForTerminalExitRequest, responder, _cx| {
                            let id = request.terminal_id.0.to_string();
                            let status = {
                                let mut map = terminals.lock().await;
                                if let Some(term) = map.get_mut(&id) {
                                    match term.child.wait().await {
                                        Ok(status) => TerminalExitStatus::new()
                                            .exit_code(status.code().map(|c| c as u32)),
                                        Err(_) => TerminalExitStatus::new(),
                                    }
                                } else {
                                    TerminalExitStatus::new()
                                }
                            };
                            let _ = responder.respond(WaitForTerminalExitResponse::new(status));
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        let terminals = terminals_kill;
                        async move |request: KillTerminalRequest, responder, _cx| {
                            let id = request.terminal_id.0.to_string();
                            let mut map = terminals.lock().await;
                            if let Some(term) = map.get_mut(&id) {
                                let _ = term.child.kill().await;
                            }
                            let _ = responder.respond(KillTerminalResponse::new());
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    {
                        let terminals = terminals_release;
                        async move |request: ReleaseTerminalRequest, responder, _cx| {
                            let id = request.terminal_id.0.to_string();
                            terminals.lock().await.remove(&id);
                            let _ = responder.respond(ReleaseTerminalResponse::new());
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(acp_agent, {
                    let session_id = session_id_task;
                    let pending = pending_task;
                    let event_tx = event_tx;
                    let mcp_configs = mcp_servers;
                    let cwd = cwd;
                    async move |cx: ConnectionTo<Agent>| {
                        let caps = ClientCapabilities::new()
                            .fs(FileSystemCapabilities::new()
                                .read_text_file(true)
                                .write_text_file(true))
                            .terminal(true);

                        let init = cx
                            .send_request(
                                InitializeRequest::new(ProtocolVersion::V1)
                                    .client_capabilities(caps)
                                    .client_info(Implementation::new(
                                        "ozma",
                                        env!("CARGO_PKG_VERSION"),
                                    )),
                            )
                            .block_task()
                            .await;

                        match init {
                            Ok(resp) => {
                                info!(
                                    "ACP agent initialized: {:?}",
                                    resp.agent_info.as_ref().map(|i| &i.name)
                                );
                                let include_http = resp.agent_capabilities.mcp_capabilities.http;
                                let can_list =
                                    resp.agent_capabilities.session_capabilities.list.is_some();
                                let can_delete = resp
                                    .agent_capabilities
                                    .session_capabilities
                                    .delete
                                    .is_some();
                                let can_load = resp.agent_capabilities.load_session;

                                let _ = ready_tx.send(Ok(()));

                                while let Some(cmd) = cmd_rx.recv().await {
                                    match cmd {
                                        AcpCommand::NewSession { reply } => {
                                            let mcp =
                                                mcp_servers_from_config(&mcp_configs, include_http);
                                            let result = cx
                                                .send_request(
                                                    NewSessionRequest::new(cwd.clone())
                                                        .mcp_servers(mcp),
                                                )
                                                .block_task()
                                                .await;
                                            match result {
                                                Ok(resp) => {
                                                    let id = resp.session_id.0.to_string();
                                                    *session_id.lock().await = Some(id.clone());
                                                    let _ = reply.send(Ok(id));
                                                }
                                                Err(e) => {
                                                    let _ = reply.send(Err(eyre!("{e}")));
                                                }
                                            }
                                        }
                                        AcpCommand::Prompt { text, reply } => {
                                            let sid = session_id.lock().await.clone();
                                            let Some(sid) = sid else {
                                                let _ =
                                                    reply.send(Err(eyre!("No active ACP session")));
                                                continue;
                                            };
                                            let result = cx
                                                .send_request(PromptRequest::new(
                                                    SessionId::new(sid),
                                                    vec![ContentBlock::from(text)],
                                                ))
                                                .block_task()
                                                .await;
                                            match result {
                                                Ok(resp) => {
                                                    debug!(
                                                        "ACP prompt done: {:?}",
                                                        resp.stop_reason
                                                    );
                                                    let _ = event_tx
                                                        .send(Event::StreamDone { usage: None });
                                                    let _ = reply.send(Ok(()));
                                                }
                                                Err(e) => {
                                                    let _ = event_tx
                                                        .send(Event::StreamError(e.to_string()));
                                                    let _ = reply.send(Err(eyre!("{e}")));
                                                }
                                            }
                                        }
                                        AcpCommand::Cancel => {
                                            if let Some(sid) = session_id.lock().await.clone() {
                                                let _ = cx.send_notification(
                                                    CancelNotification::new(SessionId::new(sid)),
                                                );
                                            }
                                        }
                                        AcpCommand::ListSessions { reply } => {
                                            if !can_list {
                                                let _ = reply.send(Ok(Vec::new()));
                                                continue;
                                            }
                                            let result = cx
                                                .send_request(ListSessionsRequest::new())
                                                .block_task()
                                                .await;
                                            match result {
                                                Ok(resp) => {
                                                    let sessions = resp
                                                        .sessions
                                                        .into_iter()
                                                        .map(|s| AcpSessionSummary {
                                                            id: s.session_id.0.to_string(),
                                                            title: s.title.unwrap_or_else(|| {
                                                                s.session_id.0.to_string()
                                                            }),
                                                            cwd: s.cwd.display().to_string(),
                                                            updated_at: s.updated_at,
                                                        })
                                                        .collect();
                                                    let _ = reply.send(Ok(sessions));
                                                }
                                                Err(e) => {
                                                    let _ = reply.send(Err(eyre!("{e}")));
                                                }
                                            }
                                        }
                                        AcpCommand::LoadSession { id, reply } => {
                                            if !can_load {
                                                let _ = reply.send(Err(eyre!(
                                                    "Agent does not support session/load"
                                                )));
                                                continue;
                                            }
                                            let mcp =
                                                mcp_servers_from_config(&mcp_configs, include_http);
                                            let result = cx
                                                .send_request(
                                                    LoadSessionRequest::new(
                                                        SessionId::new(id.clone()),
                                                        cwd.clone(),
                                                    )
                                                    .mcp_servers(mcp),
                                                )
                                                .block_task()
                                                .await;
                                            match result {
                                                Ok(_) => {
                                                    *session_id.lock().await = Some(id);
                                                    let _ = event_tx
                                                        .send(Event::AcpSessionLoadComplete);
                                                    let _ = reply.send(Ok(()));
                                                }
                                                Err(e) => {
                                                    let _ = reply.send(Err(eyre!("{e}")));
                                                }
                                            }
                                        }
                                        AcpCommand::DeleteSession { id, reply } => {
                                            if !can_delete {
                                                let _ = reply.send(Err(eyre!(
                                                    "Agent does not support session/delete"
                                                )));
                                                continue;
                                            }
                                            let result = cx
                                                .send_request(DeleteSessionRequest::new(
                                                    SessionId::new(id),
                                                ))
                                                .block_task()
                                                .await;
                                            match result {
                                                Ok(_) => {
                                                    let _ = reply.send(Ok(()));
                                                }
                                                Err(e) => {
                                                    let _ = reply.send(Err(eyre!("{e}")));
                                                }
                                            }
                                        }
                                        AcpCommand::RespondPermission {
                                            request_id,
                                            approve,
                                            approve_always,
                                        } => {
                                            if let Some(pending) =
                                                pending.lock().await.remove(&request_id)
                                            {
                                                let outcome = resolve_permission_outcome(
                                                    &pending.options,
                                                    approve,
                                                    approve_always,
                                                );
                                                let _ = pending.reply.send(outcome);
                                            }
                                        }
                                        AcpCommand::Shutdown => break,
                                    }
                                }

                                Ok(())
                            }
                            Err(e) => {
                                let _ = ready_tx.send(Err(eyre!("ACP initialize failed: {e}")));
                                Err(e)
                            }
                        }
                    }
                })
                .await;

            if let Err(e) = result {
                error!("ACP connection ended with error: {e}");
            }
        });

        ready_rx
            .await
            .map_err(|_| eyre!("ACP connection task dropped"))??;

        Ok(Self {
            name,
            cmd_tx,
            session_id,
            pending_permissions,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn session_id(&self) -> Option<String> {
        self.session_id.lock().await.clone()
    }

    pub async fn new_session(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::NewSession { reply: tx })
            .map_err(|_| eyre!("ACP connection closed"))?;
        rx.await.map_err(|_| eyre!("ACP new_session dropped"))?
    }

    pub async fn prompt(&self, text: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Prompt { text, reply: tx })
            .map_err(|_| eyre!("ACP connection closed"))?;
        rx.await.map_err(|_| eyre!("ACP prompt dropped"))?
    }

    pub fn cancel(&self) {
        let _ = self.cmd_tx.send(AcpCommand::Cancel);
    }

    pub async fn list_sessions(&self) -> Result<Vec<AcpSessionSummary>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::ListSessions { reply: tx })
            .map_err(|_| eyre!("ACP connection closed"))?;
        rx.await.map_err(|_| eyre!("ACP list_sessions dropped"))?
    }

    pub async fn load_session(&self, id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::LoadSession { id, reply: tx })
            .map_err(|_| eyre!("ACP connection closed"))?;
        rx.await.map_err(|_| eyre!("ACP load_session dropped"))?
    }

    pub async fn delete_session(&self, id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::DeleteSession { id, reply: tx })
            .map_err(|_| eyre!("ACP connection closed"))?;
        rx.await.map_err(|_| eyre!("ACP delete_session dropped"))?
    }

    pub fn respond_permission(&self, request_id: String, approve: bool, approve_always: bool) {
        let _ = self.cmd_tx.send(AcpCommand::RespondPermission {
            request_id,
            approve,
            approve_always,
        });
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(AcpCommand::Shutdown);
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.shutdown();
        // Unblock any in-flight permission requests so the agent doesn't hang.
        if let Ok(mut map) = self.pending_permissions.try_lock() {
            for (_, pending) in map.drain() {
                let _ = pending.reply.send(RequestPermissionOutcome::Cancelled);
            }
        }
    }
}

fn build_acp_agent(agent: &AgentConfig) -> Result<AcpAgent> {
    let env: Vec<EnvVariable> = agent
        .env
        .iter()
        .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
        .collect();
    let server = agent_client_protocol::schema::v1::McpServer::Stdio(
        agent_client_protocol::schema::v1::McpServerStdio::new(agent.name.clone(), &agent.command)
            .args(agent.args.clone())
            .env(env),
    );
    Ok(AcpAgent::new(server))
}

fn map_tool_kind_to_permission(kind: &str) -> PermissionLevel {
    let lower = kind.to_lowercase();
    if lower.contains("execute") || lower.contains("terminal") {
        PermissionLevel::Dangerous
    } else if lower.contains("edit")
        || lower.contains("write")
        || lower.contains("delete")
        || lower.contains("move")
    {
        PermissionLevel::Destructive
    } else {
        PermissionLevel::ReadOnly
    }
}

fn should_auto_approve(level: &PermissionLevel, tools: &ToolsConfig) -> bool {
    match level {
        PermissionLevel::ReadOnly => tools.auto_approve_read,
        PermissionLevel::Destructive => tools.auto_approve_write,
        PermissionLevel::Dangerous => tools.auto_approve_execute,
    }
}

fn resolve_permission_outcome(
    options: &[(String, PermissionOptionKind)],
    approve: bool,
    approve_always: bool,
) -> RequestPermissionOutcome {
    if !approve {
        if let Some((id, _)) = options.iter().find(|(_, k)| {
            matches!(
                k,
                PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
            )
        }) {
            return RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.clone()));
        }
        return RequestPermissionOutcome::Cancelled;
    }
    if approve_always {
        if let Some((id, _)) = options
            .iter()
            .find(|(_, k)| matches!(k, PermissionOptionKind::AllowAlways))
        {
            return RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.clone()));
        }
    }
    if let Some((id, _)) = options.iter().find(|(_, k)| {
        matches!(
            k,
            PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
        )
    }) {
        return RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.clone()));
    }
    RequestPermissionOutcome::Cancelled
}
