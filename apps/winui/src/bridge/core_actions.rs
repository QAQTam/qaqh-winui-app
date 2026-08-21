//! BridgeCore methods: actions.

use std::sync::atomic::Ordering;
use std::time::Duration;

use qaqh_client::{
    ActionRequest, CommandOptions, ControlCommand, ConversationCommand, ConversationMode,
    QueryRequest, RingingCommand, RingingCommandState,
};

use crate::shell_store::parse_workspace_status;

use super::*;

impl super::BridgeCore {
    /// 会话工作模式切换：`conversation_set_mode` 命令 + 本地 mode 缓存
    /// （乐观更新——daemon 无 mode 领域事件，对齐 Web 单例 mode 语义）。
    pub(crate) fn spawn_set_mode(&self, mode: &str) {
        *self.composer_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode.to_string();
        self.composer_rev.fetch_add(1, Ordering::Relaxed);
        let mode = match mode {
            "plan" => ConversationMode::Plan,
            "code" => ConversationMode::Code,
            _ => ConversationMode::Code,
        };
        self.spawn_conversation_command(ConversationCommand::ConversationSetMode { mode });
    }

    /// 工具模式切换：乐观更新本地缓存 + daemon `session.set_tool_mode`
    /// action（daemon 侧 persist meta.json + Control 频道下发 worker 应用
    /// set_allowed_tools + tool_defs 刷新）。
    ///
    /// 锁死检查点（CK-UI-SEND）：
    /// - 白名单：非法值直接拒绝，绝不写入乐观缓存/daemon；
    /// - 幂等：与当前缓存完全一致时跳过，避免重复 action；
    /// - 失败回滚：connect/action 失败立即以 meta.json 为权威回填缓存，
    ///   乐观值不会残留成「假极简/假标准」。
    pub(crate) fn spawn_set_tool_mode(&self, tool_mode: &str, custom_tools: Vec<String>) {
        if !qaqh_types::tool_mode::is_known(tool_mode) {
            log_diag(&format!(
                "set_tool_mode: rejected invalid tool_mode '{tool_mode}'"
            ));
            return;
        }
        let seed = self.active_seed();
        if seed.is_empty() {
            log_diag("set_tool_mode: no active session, skipped");
            return;
        }
        {
            let cur = self
                .composer_tool_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cur.mode == tool_mode && cur.custom_tools == custom_tools {
                return;
            }
        }
        *self
            .composer_tool_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = ToolModeState {
            mode: tool_mode.to_string(),
            custom_tools: custom_tools.clone(),
        };
        self.composer_rev.fetch_add(1, Ordering::Relaxed);
        let core = self.self_arc();
        let tool_mode_owned = tool_mode.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("set_tool_mode: connect failed: {err}"));
                    // 权威回填：立即撤销乐观值，而不是等下一次 250ms 轮询。
                    core.sync_tool_mode_from_meta();
                    return;
                }
            };
            match client
                .action(ActionRequest::SessionSetToolMode {
                    seed,
                    tool_mode: tool_mode_owned.clone(),
                    custom_tools,
                })
                .await
            {
                Ok(_) => log_diag(&format!("set_tool_mode {tool_mode_owned}: ok")),
                Err(err) => {
                    log_diag(&format!("set_tool_mode {tool_mode_owned}: failed: {err}"));
                    // 权威回填：action 被 daemon 拒绝/失败时撤销乐观值。
                    core.sync_tool_mode_from_meta();
                }
            }
        });
    }

    /// 按 turn_id 撤销（聊天右键「撤回此消息」）：删除该 turn 及其后全部
    /// 回合（daemon `truncate_before_turn` 语义）。命令 + 状态轮询 +
    /// timeline 重拉；turn_id 来自消息本身，不依赖 last_turn_ids 缓存。
    pub(crate) fn spawn_undo_turn(&self, turn_id: String) {
        let seed = self.active_seed();
        let core = self.self_arc();
        let command_id = self.next_command_id();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("undo {seed}: connect failed: {err}"));
                    return;
                }
            };
            let result = client
                .send_command(
                    Some(&seed),
                    RingingCommand::Conversation(ConversationCommand::ConversationUndoTurn {
                        turn_id,
                    }),
                    CommandOptions {
                        command_id: Some(command_id.clone()),
                        ..Default::default()
                    },
                )
                .await;
            if let Err(err) = result {
                log_diag(&format!("undo {seed}: command failed: {err}"));
                return;
            }

            // ACK only means accepted. Wait for the durable receipt before
            // reloading timeline; otherwise a fast GET can still return the
            // pre-undo snapshot and leave ChatView/Header stale.
            let mut succeeded = false;
            for _ in 0..20 {
                match client.command_status(&command_id).await {
                    Ok(status) if status.state == RingingCommandState::Succeeded => {
                        succeeded = true;
                        break;
                    }
                    Ok(status)
                        if matches!(
                            status.state,
                            RingingCommandState::Failed | RingingCommandState::Rejected
                        ) =>
                    {
                        log_diag(&format!(
                            "undo {seed}: terminal {:?} ({:?})",
                            status.state, status.error_code
                        ));
                        return;
                    }
                    Ok(_) => {}
                    Err(err) => log_diag(&format!("undo {seed}: status pending: {err}")),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if !succeeded {
                log_diag(&format!("undo {seed}: completion timed out"));
            }
            // The client owns one active timeline stream. If the user switched
            // away while Undo was completing, refreshing A here would silently
            // replace B's stream. A will be reloaded when it is selected again.
            if core.active_seed() != seed {
                log_diag(&format!(
                    "undo {seed}: timeline refresh deferred (inactive seed)"
                ));
                return;
            }
            if let Err(err) = client.activate_timeline(&seed).await {
                log_diag(&format!("undo {seed}: timeline refresh failed: {err}"));
            } else {
                log_diag(&format!("undo {seed}: completed and refreshed"));
            }
        });
    }

    /// 工作区运行模式切换：`workspace.set_mode`（backend.restart 未实现，
    /// 保存成功后由 UI 提示“下次启动生效”）。
    pub(crate) fn spawn_workspace_set_mode(&self, mode: &str) {
        let core = self.self_arc();
        let mode = mode.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace.set_mode: connect failed: {err}"));
                    return;
                }
            };
            match client
                .action(ActionRequest::WorkspaceSetMode { mode: mode.clone() })
                .await
            {
                Ok(_) => log_diag(&format!("workspace.set_mode {mode}: ok")),
                Err(err) => log_diag(&format!("workspace.set_mode {mode}: failed: {err}")),
            }
        });
    }

    /// 刷新 workspace.status 并合并进 settings 缓存（rev++）。
    pub(crate) fn spawn_workspace_status(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace.status: connect failed: {err}"));
                    return;
                }
            };
            match client.query(QueryRequest::WorkspaceStatus).await {
                Ok(status) => {
                    let (cfg, active, endpoint) = parse_workspace_status(&status);
                    if let Some(snap) = core
                        .settings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_mut()
                    {
                        snap.workspace_configured_mode = cfg;
                        snap.workspace_active_mode = active;
                        snap.workspace_endpoint = endpoint;
                        core.settings_rev.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(err) => log_diag(&format!("workspace.status failed: {err}")),
            }
        });
    }

    /// WSL 诊断（`workspace.diagnose`，workspace 分类只读展示）。
    pub(crate) fn spawn_workspace_diagnose(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace.diagnose: connect failed: {err}"));
                    return;
                }
            };
            match client.query(QueryRequest::WorkspaceDiagnose).await {
                Ok(v) => log_diag(&format!("workspace.diagnose: {v}")),
                Err(err) => log_diag(&format!("workspace.diagnose failed: {err}")),
            }
        });
    }

    /// WSL 安装（`workspace.install_wsl`）。
    pub(crate) fn spawn_workspace_install_wsl(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace.install_wsl: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::WorkspaceInstallWsl).await {
                Ok(_) => log_diag("workspace.install_wsl: ok"),
                Err(err) => log_diag(&format!("workspace.install_wsl failed: {err}")),
            }
        });
    }

    /// home 视图发送：新建会话 + 首条消息（对齐 Web `startNewSessionAndSend`）。
    ///
    /// session_create（control）→ 轮询发现新 seed（15s 超时）→ attach →
    /// 创建新会话并发送首条消息（`session_create` command + 轮询新 seed +
    /// `conversation_send_message` command → navigate chat）。
    ///
    /// 2026-08-08 修复：首条消息此前误用 `action("session.send_message")`
    /// （action 白名单不含 session.* → daemon 拒绝），改走 command 通道。
    pub(crate) fn spawn_send_new_session(&self, text: &str) {
        let core = self.self_arc();
        let text = text.to_string();
        let cwd = core.current_workspace_path();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("send_new_session: connect failed: {err}"));
                    return;
                }
            };
            core.refresh_sessions_inner().await;
            let before = core.seed_set();
            match client
                .send_command(
                    None,
                    RingingCommand::Control(ControlCommand::SessionCreate {
                        close_current: false,
                        cwd: cwd.clone(),
                        tool_mode: None,
                        custom_tools: Vec::new(),
                    }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    let mut seed = String::new();
                    for _ in 0..30 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        core.refresh_sessions_inner().await;
                        let now = core.seed_set();
                        if let Some(new_seed) = now.iter().find(|s| !before.contains(*s)) {
                            seed = new_seed.clone();
                            break;
                        }
                    }
                    if seed.is_empty() {
                        log_diag("send_new_session: no new seed within 15s");
                        return;
                    }
                    if let Err(err) = client.attach(&seed).await {
                        log_diag(&format!("send_new_session: attach failed: {err}"));
                        return;
                    }
                    core.set_active_seed(&seed);
                    if let Err(err) = client
                        .send_command(
                            Some(&seed),
                            RingingCommand::Conversation(
                                ConversationCommand::ConversationSendMessage {
                                    text,
                                    images: vec![],
                                    attachments: None,
                                    as_system: false,
                                },
                            ),
                            CommandOptions::default(),
                        )
                        .await
                    {
                        log_diag(&format!("send_new_session: send_message failed: {err}"));
                        return;
                    }
                    log_diag(&format!("send_new_session: created {seed}, message sent"));
                    core.navigate("chat", Some(&seed));
                }
                Err(err) => log_diag(&format!("send_new_session: command failed: {err}")),
            }
        });
    }

    /// 通知壳切换视图（XAML 侧栏的导航出口）。
    ///
    /// 同步更新壳侧 `current_view`——XAML 视图族据此接管/让出 skills 视图
    /// （main.rs 内容区同 cell 重叠 + opacity 切换，见 WORKFLOW §8）。
    pub(crate) fn navigate(&self, view: &str, seed: Option<&str>) {
        *self.current_view.lock().unwrap_or_else(|e| e.into_inner()) = view.to_string();
        // WebView 移除：不再 emit shell.navigate（视图切换壳本地持有）。
        if let Some(seed) = seed {
            self.set_active_seed(seed);
        }
        // 标题栏直连：view 变化立即刷新（不再等 Web setHeader 回推）。
        self.refresh_header();
    }

    /// Lazily connect the qaqh-client and register event forwarding.
    // ── 远端 daemon 档案（临时跨端模式）──────────────────────────

    /// 当前远端档案快照（设置页渲染 + 路径显示映射用）。
    pub(crate) fn remote_profile_snapshot(&self) -> Option<RemoteProfile> {
        self.remote_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 保存远端档案并切换：关旧连接、清空会话状态、回首页、重连。
    pub(crate) fn apply_remote_profile(&self, base_url: String, token: String) {
        let url = base_url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            log_diag(&format!("remote: invalid base_url {url}"));
            return;
        }
        let profile = RemoteProfile {
            base_url: url,
            token: token.trim().to_string(),
        };
        if let Err(error) = write_remote_profile(&profile) {
            log_diag(&format!("remote: save profile failed: {error}"));
        }
        *self
            .remote_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(profile);
        self.switch_daemon();
    }
}
