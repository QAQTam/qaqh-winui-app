//! BridgeCore methods: settings.

use std::sync::atomic::Ordering;

use qaqh_client::{
    ActionRequest, AskAnswer as DomainAskAnswer, CommandOptions, ControlCommand,
    ConversationCommand, QueryRequest, RingingCommand, ToolCommand,
};
use serde_json::Value;

use crate::shell_store::{parse_config_load, parse_tools, parse_workspace_status};

use super::*;

impl super::BridgeCore {
    /// 拉取 `config.load` + `skills.list_tools` → 投影进缓存 → rev++。
    /// 幂等：仅缓存为空或 `force` 时执行（进入设置页首次渲染兜底）。
    pub(crate) fn spawn_config_load(&self, force: bool) {
        if !force
            && self
                .settings
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return;
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("config_load: connect failed: {err}"));
                    return;
                }
            };
            let config = match client.query(QueryRequest::ConfigLoad).await {
                Ok(v) => v,
                Err(err) => {
                    log_diag(&format!("config.load failed: {err}"));
                    return;
                }
            };
            let mut snap = parse_config_load(&config);
            // workspace.status 与 config.load 并行（独立查询，失败不阻塞）。
            if let Ok(status) = client.query(QueryRequest::WorkspaceStatus).await {
                let (cfg, active, endpoint) = parse_workspace_status(&status);
                snap.workspace_configured_mode = cfg;
                snap.workspace_active_mode = active;
                snap.workspace_endpoint = endpoint;
            }
            // 工具列表（subagent 勾选项）；失败不阻塞（页面显示空列表）。
            if let Ok(tools) = client.query(QueryRequest::SkillsListTools).await {
                snap.tools = parse_tools(&tools);
            }
            // 同步投影缓存：settings_view 的权限滑杆/ComboBox 读 settings_proj。
            // Web 时代由 shell.setSettings 填充；原生迁移后该通道被移除 → 投影恒
            // 默认（permission_level=0 → UI 误导为 L1/"加载中"）。此处从权威
            // snapshot 补齐（theme 现亦有 snapshot 来源——2026-08 后端契约新增）。
            {
                let mut proj = core.settings_proj.lock().unwrap_or_else(|e| e.into_inner());
                proj.permission_level = snap.permission_level;
                proj.lang = snap.lang.clone();
                proj.workspace_mode = snap.workspace_active_mode.clone();
                proj.theme = snap.theme.clone();
            }
            core.settings_proj_rev.fetch_add(1, Ordering::Relaxed);
            // 通知开关：后端 config 为单一权威源，落内存 + 本地偏好文件镜像
            // （通知器初始化在启动早期完成，config.load 到达前仍走本地偏好）。
            core.notif_enabled
                .store(snap.notifications_enabled, Ordering::Relaxed);
            write_notif_pref(snap.notifications_enabled);
            if snap.notifications_enabled {
                core.ensure_notifier();
            }
            *core.settings.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap);
            core.settings_rev.fetch_add(1, Ordering::Relaxed);
            log_diag("config_load: settings snapshot cached");
        });
    }

    /// 保存设置：`config.save`（camelCase 全字段，对齐 Web `save()`）。
    pub(crate) fn spawn_config_save(&self, fields: Value) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("config.save: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::ConfigSave { fields }).await {
                Ok(_) => log_diag("config.save: ok"),
                Err(err) => log_diag(&format!("config.save failed: {err}")),
            }
        });
    }

    /// 切换预设：`profile.apply`（daemon 应用后下次 config.load 拿到新值）。
    pub(crate) fn spawn_apply_profile(&self, name: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("profile.apply: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::ProfileApply { name }).await {
                Ok(_) => log_diag("profile.apply: ok"),
                Err(err) => log_diag(&format!("profile.apply failed: {err}")),
            }
        });
    }

    /// 把当前编辑的草稿保存为新预设：`profile.save_current`。
    pub(crate) fn spawn_save_profile(&self, name: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("profile.save_current: connect failed: {err}"));
                    return;
                }
            };
            match client
                .action(ActionRequest::ProfileSaveCurrent { name })
                .await
            {
                Ok(_) => log_diag("profile.save_current: ok"),
                Err(err) => log_diag(&format!("profile.save_current failed: {err}")),
            }
        });
    }

    /// 删除预设：`profile.delete`（default 不可删，daemon 会返回 Err）。
    pub(crate) fn spawn_delete_profile(&self, name: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("profile.delete: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::ProfileDelete { name }).await {
                Ok(_) => log_diag("profile.delete: ok"),
                Err(err) => log_diag(&format!("profile.delete failed: {err}")),
            }
        });
    }

    /// 权限等级：`config.set_permission_level`（对齐 Web changePermissionLevel）。
    pub(crate) fn spawn_set_permission(&self, level: u64) {
        // 写前守卫（Bug#2 修复）：settings 缓存未加载（启动时序 config.load
        // 未到达）或 level 与当前已加载值一致时直接跳过，不向 daemon 发送——
        // 否则 ComboBox/Slider 的初始化同步事件会把权限误写成 L1 并持久化。
        let current = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.permission_level);
        match current {
            Some(cur) if cur == level => {
                log_diag(&format!("set_permission {level}: skipped (unchanged)"));
                return;
            }
            None => {
                log_diag(&format!(
                    "set_permission {level}: skipped (config not loaded)"
                ));
                return;
            }
            _ => {}
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("set_permission: connect failed: {err}"));
                    return;
                }
            };
            match client
                .action(ActionRequest::ConfigSetPermissionLevel {
                    level: level.clone(),
                })
                .await
            {
                Ok(_) => log_diag(&format!("set_permission {level}: ok")),
                Err(err) => log_diag(&format!("set_permission {level}: failed: {err}")),
            }
        });
    }

    // ── 直连动作（WebView 移除：协议请求 Rust 直发，不再经 Web 中转）──

    /// conversation 频道命令直发（cancel/compact/set_mode 等）。
    /// ack 仅表示 accepted；业务结果经事件流（causation_id）返回。
    /// 失败只记日志（对齐 Web：错误 toast 由调用方本地判定，不阻塞 UI）。
    pub(crate) fn spawn_conversation_command(&self, command: ConversationCommand) {
        let core = self.self_arc();
        // Capture synchronously at click time. Reading active_seed inside the
        // spawned future lets a fast tab switch retarget Stop/Undo to another
        // conversation while ensure_client is awaiting.
        let seed = self.active_seed();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("cmd: connect failed: {err}"));
                    return;
                }
            };
            match client
                .send_command(
                    Some(&seed),
                    RingingCommand::Conversation(command),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => log_diag("conversation command accepted"),
                Err(err) => log_diag(&format!("conversation command failed: {err}")),
            }
        });
    }

    /// 发送消息：附件统一上传为 ContentRef（图片也走上传——命令中不允许
    /// base64 或本地路径，对齐 daemon 约束与 Electron main 语义）。
    pub(crate) fn spawn_send_message(
        &self,
        text: String,
        image_paths: Vec<ComposerAttachment>,
        text_files: Vec<ComposerTextFile>,
    ) {
        let core = self.self_arc();
        // Uploads can take long enough for the user to switch tabs. The
        // message and its eventual feedback belong to the seed at submit time.
        let seed = self.active_seed();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("send: connect failed: {err}"));
                    return;
                }
            };
            let mut attachments = Vec::new();
            for att in &image_paths {
                match std::fs::read(&att.path) {
                    Ok(bytes) => match client.upload_content(&seed, &att.mime_type, bytes).await {
                        Ok(content_ref) => attachments.push(content_ref),
                        Err(err) => {
                            log_diag(&format!("send: upload {} failed: {err}", att.file_name))
                        }
                    },
                    Err(err) => log_diag(&format!("send: read {} failed: {err}", att.path)),
                }
            }
            for tf in &text_files {
                match std::fs::read(&tf.path) {
                    Ok(bytes) => match client.upload_content(&seed, "text/plain", bytes).await {
                        Ok(content_ref) => attachments.push(content_ref),
                        Err(err) => {
                            log_diag(&format!("send: upload {} failed: {err}", tf.file_name))
                        }
                    },
                    Err(err) => log_diag(&format!("send: read {} failed: {err}", tf.path)),
                }
            }
            match client
                .send_command(
                    Some(&seed),
                    RingingCommand::Conversation(ConversationCommand::ConversationSendMessage {
                        text,
                        images: vec![],
                        attachments: (!attachments.is_empty()).then_some(attachments),
                        as_system: false,
                    }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    log_diag("send_message accepted");
                    // B 组反馈本地写入：ack 递增（清空信号）+ 清除错误。
                    let mut feedback = core
                        .composer_feedback
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let fb = feedback.entry(seed.clone()).or_default();
                    fb.send_ack = fb.send_ack.wrapping_add(1);
                    fb.submit_error.clear();
                    drop(feedback);
                    core.composer_rev.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => {
                    log_diag(&format!("send_message failed: {err}"));
                    let mut feedback = core
                        .composer_feedback
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let fb = feedback.entry(seed.clone()).or_default();
                    fb.submit_error = err.to_string();
                    drop(feedback);
                    core.composer_rev.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    /// 交互响应直发（permission/ask/plan）：Ringing command envelope
    /// （`POST /commands/{control|tool}`，对齐 composer send_message 模式）。
    ///
    /// 2026-08-08 修复：此前误用 query 通道（`/queries/` 白名单不含
    /// `interaction.*` → daemon 404），弹窗按钮全部无效、回合永久挂起；
    /// 现按 method 映射到 qaqh-domain `ControlCommand`/`ToolCommand`
    /// 的 serde tag（snake_case）与频道。
    pub(crate) fn spawn_interaction_response(&self, method: &str, params: Value) {
        let core = self.self_arc();
        let method = method.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("{method}: connect failed: {err}"));
                    return;
                }
            };
            let seed = params
                .get("seed")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let gs = |k: &str| {
                params
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let gb = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            let command = match method.as_str() {
                "interaction.permission" => {
                    RingingCommand::Tool(ToolCommand::ToolPermissionRespond {
                        tool_call_id: gs("toolCallId"),
                        approved: gb("approved"),
                        trust_folder: gb("trustFolder"),
                    })
                }
                "interaction.ask_response" => {
                    let answers = params
                        .get("answers")
                        .cloned()
                        .map(serde_json::from_value::<Vec<DomainAskAnswer>>)
                        .transpose();
                    let answers = match answers {
                        Ok(Some(answers)) => answers,
                        Ok(None) => Vec::new(),
                        Err(error) => {
                            log_diag(&format!("{method}: invalid typed answers: {error}"));
                            return;
                        }
                    };
                    RingingCommand::Control(ControlCommand::InteractionAskRespond {
                        interaction_id: gs("askId"),
                        answers,
                    })
                }
                "interaction.ask_dismiss" => {
                    RingingCommand::Control(ControlCommand::InteractionAskDismiss {
                        interaction_id: gs("askId"),
                    })
                }
                "interaction.plan_review" => {
                    RingingCommand::Control(ControlCommand::PlanReviewRespond {
                        interaction_id: gs("callId"),
                        approved: gb("approved"),
                        message: params
                            .get("message")
                            .and_then(|v| v.as_str())
                            .filter(|message| !message.is_empty())
                            .map(str::to_string),
                        autonomous: gb("autonomous"),
                    })
                }
                _ => {
                    log_diag(&format!("{method}: unknown interaction method"));
                    return;
                }
            };
            match client
                .send_command(seed.as_deref(), command, CommandOptions::default())
                .await
            {
                Ok(_) => log_diag(&format!("{method}: accepted")),
                Err(err) => log_diag(&format!("{method}: failed: {err}")),
            }
        });
    }

    /// 工作区切换：`workspace.set`（headerAction::Workspace 直发）。
    ///
    /// 反馈闭环（2026-08-14 修复）：此前结果只写日志，标题栏永远显示
    /// 「未选择工作区」——用户选了目录但界面零变化，反复点击 → 反复弹窗
    /// → 误判「选择失败（后端未收到）」。现成功写路径、失败写错误文案，
    /// 均经 `header_rev` 递增让标题栏 500ms 轮询即时反映。
    pub(crate) fn spawn_workspace_set(&self, path: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace.set: connect failed: {err}"));
                    core.apply_workspace_feedback(None, Some(format!("连接失败：{err}")));
                    return;
                }
            };
            let seed = core.active_seed();
            if seed.is_empty() {
                // Home 无活动会话时，会话级 workspace.set 无法执行（需 seed/lease），
                // 但组织级已在 header::on_workspace 中经 spawn_workspace_create
                // 完成选中，左侧筛选与顶部显示已由 refresh_header 派生，无需报错。
                log_diag(
                    "workspace.set: no active session, skipped (org workspace already selected)",
                );
                // 不写 workspace_error，避免顶部闪红；组织路径已在 refresh_header 展示
                return;
            }
            match client
                .action(ActionRequest::WorkspaceSet {
                    seed: seed.clone(),
                    path: path.clone(),
                })
                .await
            {
                Ok(_) => {
                    log_diag("workspace.set: ok");
                    core.apply_workspace_feedback(Some(path.clone()), None);
                    // 双保险：会话级 set 后，后端 manager::set_cwd 已 attach，
                    // 但前端需刷新两侧列表才可见分组变化
                    core.refresh_workspaces_inner().await;
                    core.refresh_sessions_inner().await;
                }
                Err(err) => {
                    log_diag(&format!("workspace.set failed: {err}"));
                    core.apply_workspace_feedback(None, Some(format!("设置失败：{err}")));
                }
            }
        });
    }

    /// 标题栏工作区反馈投影：成功路径写入 `workspace`（并清错误），失败
    /// 路径写入 `workspace_error`。`header_rev` 递增驱动 header 轮询刷新。
    pub(crate) fn apply_workspace_feedback(&self, path: Option<String>, error: Option<String>) {
        let mut h = self.header_state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = path {
            // 远端模式：标题栏显示 //ip/路径 的显示形式；本地模式原样。
            h.workspace = self.display_remote_path(&path);
            h.workspace_error = None;
        }
        if let Some(error) = error {
            h.workspace_error = Some(error);
        }
        drop(h);
        self.header_rev.fetch_add(1, Ordering::Relaxed);
    }
}
