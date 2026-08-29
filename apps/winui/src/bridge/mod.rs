//! Bridge: qaqh-client（Ringing 协议）<-> 原生 XAML 视图族。
//!
//! - `BridgeCore`（tokio 侧）：daemon 连接管理 + 三 SSE 频道事件解析
//!   （conversation → ChatView/Composer 直连缓存；control/tool → 交互队列
//!   状态机；control → 侧栏/技能/goalBar 快照）+ 命令/查询直发层。
//! - `Bridge`（UI 线程侧）：`core` 引用 + pump 心跳（失联检测）。
//!
//! WebView 已移除：invoke/emit/outbox 通道整体下线；daemon `/debug/`
//! 浏览器调试入口不经本桥。
//!
//! Threading: `BridgeCore` is `Send + Sync` and lives on the tokio side;
//! `Bridge` 仅持 `Arc<BridgeCore>`，UI 线程调用均无锁跨线程约束。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use qaqh_client::ConversationCommand;
use serde_json::Value;

mod bridge_core;
mod core_actions;
mod core_client;
mod core_interaction;
mod core_lifecycle;
mod core_notifications;
mod core_remote;
mod core_sessions;
mod core_settings;
mod core_skills;
mod core_state;
mod core_timeline;
mod platform;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use bridge_core::*;
pub(crate) use platform::*;
pub(crate) use types::*;

static SHARED_CORE: OnceLock<Arc<BridgeCore>> = OnceLock::new();

/// UI-thread half of the bridge（仅持 tokio 侧 core 引用）。
pub struct Bridge {
    core: Arc<BridgeCore>,
}

static SHARED: OnceLock<Arc<Bridge>> = OnceLock::new();

impl Bridge {
    pub fn shared() -> Arc<Bridge> {
        SHARED
            .get_or_init(|| {
                let core = Arc::new(BridgeCore {
                    client: Mutex::new(None),
                    attached: Mutex::new(HashSet::new()),
                    channel_status: Mutex::new(HashMap::new()),
                    sessions: Mutex::new(Vec::new()),
                    activities: Mutex::new(HashMap::new()),
                    session_rev: AtomicU64::new(0),
                    workspaces: Mutex::new(Vec::new()),
                    workspace_rev: AtomicU64::new(0),
                    current_workspace: Mutex::new(None),
                    active_seed: Mutex::new(String::new()),
                    header_state: Mutex::new(HeaderState::default()),
                    header_rev: AtomicU64::new(0),
                    header_turns: Mutex::new(HashMap::new()),
                    compact_statuses: Mutex::new(HashMap::new()),
                    last_turn_ids: Mutex::new(HashMap::new()),
                    timeline_stall_since: Mutex::new(None),
                    channels_stall_since: Mutex::new(None),
                    rebuilding: AtomicBool::new(false),
                    connecting: AtomicBool::new(false),
                    last_rebuild_at: Mutex::new(Instant::now()),
                    last_auto_reconnect_at: Mutex::new(Instant::now()),
                    rebuild_failures: AtomicU32::new(0),
                    last_timeline_seed: Mutex::new(String::new()),
                    timeline_status: Mutex::new(None),
                    skills: Mutex::new(None),
                    skills_rev: AtomicU64::new(0),
                    current_view: Mutex::new("home".to_string()),
                    remote_profile: Mutex::new(load_remote_profile()),
                    remote_fs_listing: Mutex::new(RemoteFsListing::default()),
                    remote_fs_rev: AtomicU64::new(0),
                    remote_fs_preview: Mutex::new(None),
                    remote_fs_preview_rev: AtomicU64::new(0),
                    settings: Mutex::new(None),
                    settings_rev: AtomicU64::new(0),
                    settings_proj: Mutex::new(SettingsProjection::default()),
                    settings_proj_rev: AtomicU64::new(0),
                    save_status: Mutex::new(None),
                    save_status_rev: AtomicU64::new(0),
                    info: Mutex::new(None),
                    info_rev: AtomicU64::new(0),
                    interaction: Mutex::new(InteractionState::default()),
                    interaction_rev: AtomicU64::new(0),
                    interactions: Mutex::new(HashMap::new()),
                    subagent_tracker: Mutex::new(HashMap::new()),
                    composer_rev: AtomicU64::new(0),
                    composer_activity: Mutex::new(HashMap::new()),
                    composer_mode: Mutex::new("code".to_string()),
                    composer_tool_mode: Mutex::new(ToolModeState::default()),
                    composer_feedback: Mutex::new(HashMap::new()),
                    // Canonical conversation events are queued for the native
                    // ChatView; no shell projection participates here.
                    timeline_events: Mutex::new(TimelineEventQueues::default()),
                    chat_timeline_ready: Mutex::new(None),
                    composer_drafts: Mutex::new(HashMap::new()),
                    timeline_rev: AtomicU64::new(0),
                    send_epoch: AtomicU64::new(0),
                    resume_generation: AtomicU64::new(0),
                    chat_timeline: Mutex::new(None),
                    subagent_timeline: Mutex::new(None),
                    subagent_timeline_fetching: Mutex::new(HashSet::new()),
                    timeline_has_more: Mutex::new(std::collections::HashMap::new()),
                    chat_prepend: Mutex::new(std::collections::VecDeque::new()),
                    timeline_fetching: Mutex::new(std::collections::HashSet::new()),
                    // 初始化为远古时刻：首次 refresh 立即放行。
                    timeline_refresh_at: Mutex::new(Instant::now() - Duration::from_secs(3600)),
                    dashboards: Mutex::new(HashMap::new()),
                    notifier: Mutex::new(None),
                    notif_enabled: AtomicBool::new(true),
                    dashboard_rev: AtomicU64::new(0),
                });
                let _ = SHARED_CORE.set(core.clone());
                let init_core = core.clone();
                let bridge = Arc::new(Bridge { core });
                // 桌面通知初始化**不在 get_or_init 内同步执行**：Bridge::shared()
                // 首次调用发生在 render pass 中（main.rs 各视图组件），WinRT
                // 激活/Register 在此路径执行会破坏 reconciler 状态机（
                // "state-dirty component was not re-rendered"）。改为后台线程
                // 延迟初始化：线程内显式 STA，避开 render；用户开开关时经
                // spawn_set_notif_pref 走 UI 事件回调路径（非 render pass）。
                std::thread::Builder::new()
                    .name("qaqh-notif-init".into())
                    .spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(800));
                        unsafe {
                            let _ = windows::Win32::ro::RoInitialize(
                                windows::Win32::ro::RO_INIT_SINGLETHREADED,
                            );
                        }
                        init_core.init_notifications();
                    })
                    .expect("spawn notif init thread");
                bridge
            })
            .clone()
    }

    /// XAML 侧栏访问 tokio 侧状态（会话列表 / 命令出口）。
    pub fn core(&self) -> Arc<BridgeCore> {
        self.core.clone()
    }

    // ── XAML 侧栏命令透传（sidebar.rs 只依赖 Bridge）─────────────────

    pub fn spawn_refresh_sessions(&self) {
        self.core.spawn_refresh_sessions();
    }

    pub fn spawn_refresh_workspaces(&self) {
        self.core.spawn_refresh_workspaces();
    }

    pub fn set_current_workspace(&self, id: Option<String>) {
        self.core.set_current_workspace(id);
    }

    pub fn spawn_workspace_create(&self, path: String) {
        self.core.spawn_workspace_create(path);
    }

    pub fn spawn_workspace_rename(&self, id: String, title: String) {
        self.core.spawn_workspace_rename(id, title);
    }

    pub fn spawn_workspace_delete(&self, id: String) {
        self.core.spawn_workspace_delete(id);
    }

    pub fn spawn_workspace_move_session(&self, seed: String, workspace_id: String) {
        self.core.spawn_workspace_move_session(seed, workspace_id);
    }

    pub fn spawn_workspace_detach(&self, seed: String) {
        self.core.spawn_workspace_detach(seed);
    }

    pub fn spawn_new_session(&self) {
        self.core.spawn_new_session();
    }

    pub fn spawn_resume(&self, seed: &str) {
        self.core.spawn_resume(seed);
    }

    pub fn spawn_archive(&self, seed: &str) {
        self.core.spawn_archive(seed);
    }

    pub fn spawn_unarchive(&self, seed: &str) {
        self.core.spawn_unarchive(seed);
    }

    pub fn spawn_delete(&self, seed: &str) {
        self.core.spawn_delete(seed);
    }

    pub fn navigate(&self, view: &str, seed: Option<&str>) {
        self.core.navigate(view, seed);
    }

    /// 当前壳视图名（F-N1：Alt+Left 返回守卫用）。
    pub fn current_view_name(&self) -> String {
        self.core.current_view_name()
    }

    // ── XAML 标题栏 STA 能力（header.rs 只依赖 Bridge；①②③ 壳直接处理）──

    /// ①workspace：目录选择对话框（STA COM；用户取消返回 Ok(null)）。
    /// 必须在 UI 线程调用（当前调用方：header/sidebar 的点击处理器）。
    pub fn pick_workspace_directory(&self) -> Result<Value, String> {
        show_open_dialog(true, false, false, None)
    }

    /// 工作区选择/设置失败反馈（picker `Err` 路径：header 静默吞错曾导致
    /// 「用户选了目录但程序无动作、无日志、无提示」——现标题栏展示文案）。
    pub fn report_workspace_error(&self, msg: String) {
        self.core.apply_workspace_feedback(None, Some(msg));
    }

    /// settings：文件选择对话框（tokenizer 路径；用户取消返回 Ok(null)）。
    pub fn pick_file(&self) -> Result<Value, String> {
        show_open_dialog(false, false, false, None)
    }

    /// ②location：系统 shell 打开会话目录（bridge.rs `open_external`）。
    pub fn open_path(&self, target: &str) -> Result<(), String> {
        open_external(target)
    }

    /// 标题栏本地开关翻转（headerDirect：info 壳本地维护）。
    pub fn toggle_header_flag(&self, flag: HeaderFlag) {
        self.core.toggle_header_flag(flag);
    }

    /// 清除压缩终态（F-N3：重发压缩前重置 / 测试用）。
    pub fn clear_compact_result(&self, seed: &str) {
        self.core.clear_compact_result(seed);
    }

    // ── 直连动作转发（WebView 移除：协议请求 Rust 直发）──────────────

    /// conversation 频道命令直发（cancel/compact/set_mode 等）。
    pub fn spawn_conversation_command(&self, command: ConversationCommand) {
        self.core.spawn_conversation_command(command);
    }

    /// 会话工作模式切换（命令 + 本地 mode 缓存）。
    pub fn spawn_set_mode(&self, mode: &str) {
        self.core.spawn_set_mode(mode);
    }

    /// 工具模式切换（standard/minimal/custom，PLAN-TOOL-MODES.md）。
    /// 转发 BridgeCore：乐观更新本地缓存 + daemon `session.set_tool_mode`。
    pub fn spawn_set_tool_mode(&self, tool_mode: &str, custom_tools: Vec<String>) {
        self.core.spawn_set_tool_mode(tool_mode, custom_tools);
    }

    /// 桌面通知开关（转 BridgeCore；写本地偏好 + 惰性初始化通知器）。
    pub fn spawn_set_notif_pref(&self, enabled: bool) {
        self.core.spawn_set_notif_pref(enabled);
    }

    /// 发送消息（附件上传 ContentRef 后直发 send_message）。
    pub fn spawn_send_message(
        &self,
        text: String,
        image_paths: Vec<ComposerAttachment>,
        text_files: Vec<ComposerTextFile>,
    ) {
        self.core.spawn_send_message(text, image_paths, text_files);
    }

    /// 交互响应直发（permission/ask/plan）。
    pub fn spawn_interaction_response(&self, method: &str, params: Value) {
        self.core.spawn_interaction_response(method, params);
    }

    /// 工作区切换直发（workspace.set）。
    pub fn spawn_workspace_set(&self, path: String) {
        self.core.spawn_workspace_set(path);
    }

    /// 按 turn_id 撤回直发（聊天右键「撤回此消息」）。
    pub fn spawn_undo_turn(&self, turn_id: String) {
        self.core.spawn_undo_turn(turn_id);
    }

    /// 附件：图片文件选择对话框（STA COM；用户取消返回 Ok(null)）。
    /// 复用 show_open_dialog 的 image_filter（png/jpg/jpeg/gif/webp/bmp）。
    pub fn pick_image_file(&self) -> Result<Value, String> {
        show_open_dialog(false, false, true, Some("选择图片"))
    }

    /// 附件：文本文件选择对话框（STA COM；用户取消返回 Ok(null)）。
    pub fn pick_text_file(&self) -> Result<Value, String> {
        show_open_dialog(false, false, false, Some("选择文本文件"))
    }

    // ── XAML home / settings 视图透传（home_view.rs / settings_view.rs 只依赖 Bridge）──

    /// home：新建会话 + 首条消息（壳直连，不回传 Web）。
    pub fn spawn_send_new_session(&self, text: &str) {
        self.core.spawn_send_new_session(text);
    }

    /// settings：拉取 config.load + tools（force=true 时忽略缓存）。
    pub fn spawn_config_load(&self, force: bool) {
        self.core.spawn_config_load(force);
    }

    /// settings：保存全字段（camelCase，对齐 Web `save()`）。
    pub fn spawn_config_save(&self, fields: Value) {
        self.core.spawn_config_save(fields);
    }

    /// settings：切换预设（profile.apply；daemon 应用后前端轮询拿到新值）。
    pub fn spawn_apply_profile(&self, name: &str) {
        self.core.spawn_apply_profile(name.to_string());
    }

    /// settings：把当前草稿保存为新预设（profile.save_current）。
    pub fn spawn_save_profile(&self, name: &str) {
        self.core.spawn_save_profile(name.to_string());
    }

    /// settings：删除预设（profile.delete；default 不可删）。
    pub fn spawn_delete_profile(&self, name: &str) {
        self.core.spawn_delete_profile(name.to_string());
    }

    /// settings：权限等级（config.set_permission_level）。
    pub fn spawn_set_permission(&self, level: u64) {
        self.core.spawn_set_permission(level);
    }

    /// settings：工作区运行模式（workspace.set_mode；restart 未实现，提示下次生效）。
    pub fn spawn_workspace_set_mode(&self, mode: &str) {
        self.core.spawn_workspace_set_mode(mode);
    }

    /// settings：刷新 workspace.status 进缓存。
    pub fn spawn_workspace_status(&self) {
        self.core.spawn_workspace_status();
    }

    /// settings：WSL 诊断（日志输出，无 UI 回显）。
    pub fn spawn_workspace_diagnose(&self) {
        self.core.spawn_workspace_diagnose();
    }

    /// settings：WSL 安装（日志输出，无 UI 回显）。
    pub fn spawn_workspace_install_wsl(&self) {
        self.core.spawn_workspace_install_wsl();
    }

    /// 心跳（UI 线程 timer 每 50ms 调用）：daemon 失联检测（轻量内存检查，
    /// 重建在 tokio 侧执行）。WebView 移除后无 outbox 投递。
    pub fn pump(&self) {
        self.core.check_daemon_health();
    }
}

#[cfg(test)]
fn parse_interaction_event(event: &Value) -> Option<InteractionEvent> {
    match event.get("type")?.as_str()? {
        "interaction_requested" => Some(InteractionEvent::AskRequested {
            id: event.get("interaction_id")?.as_str()?.to_string(),
            questions: parse_questions(event.get("questions")?),
        }),
        "interaction_resolved" => Some(InteractionEvent::AskResolved {
            id: event.get("interaction_id")?.as_str()?.to_string(),
        }),
        "plan_review_requested" => Some(InteractionEvent::PlanRequested {
            id: event.get("interaction_id")?.as_str()?.to_string(),
            plan_content: event.get("plan_content")?.as_str()?.to_string(),
            review_type: event
                .get("review_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            todo_items: parse_todo_items(event.get("todo_items")),
        }),
        "plan_review_resolved" => Some(InteractionEvent::PlanResolved {
            id: event.get("interaction_id")?.as_str()?.to_string(),
        }),
        "operation_failed" => match event
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)?
        {
            "ask_rejected" | "interaction_not_found" => Some(InteractionEvent::GhostCleanup),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
fn parse_tool_permission_event(event: &Value) -> Option<ToolPermissionEvent> {
    match event.get("type")?.as_str()? {
        "tool_permission_requested" => Some(ToolPermissionEvent::Requested {
            tool_call_id: event.get("tool_call_id")?.as_str()?.to_string(),
            tool_name: event.get("tool_name")?.as_str()?.to_string(),
            reason: event.get("reason")?.as_str()?.to_string(),
            paths: event
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            category: event.get("category")?.as_str()?.to_string(),
            level: event
                .get("level")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            risk: event.get("risk")?.as_str()?.to_string(),
            consequence: event.get("consequence")?.as_str()?.to_string(),
        }),
        "tool_finished" => Some(ToolPermissionEvent::Resolved {
            tool_call_id: event.get("tool_call_id")?.as_str()?.to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
fn parse_conversation_activity_event(event: &Value) -> Option<ConversationActivityEvent> {
    match event.get("type")?.as_str()? {
        "turn_started" => Some(ConversationActivityEvent::Started),
        "turn_completed" | "turn_failed" | "conversation_cancelled" => {
            Some(ConversationActivityEvent::Ended)
        }
        "usage_updated" => Some(ConversationActivityEvent::Usage {
            prompt_tokens: event
                .get("usage")
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            context_limit: event
                .get("context_limit")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            model: event
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "round_delta" => Some(ConversationActivityEvent::Delta(
            match event.get("kind").and_then(Value::as_str) {
                Some("thinking") => WorkPhase::Thinking,
                Some("answering") => WorkPhase::Answering,
                _ => return None,
            },
        )),
        "block_checkpoint" | "round_completed" | "provider_retrying" | "provider_tool_status" => {
            Some(ConversationActivityEvent::Touched)
        }
        _ => None,
    }
}

/// Minimal file logger (GUI subsystem has no console).
/// `pub(crate)`：header.rs 的 picker `Err` 分支也写入同一日志（此前静默吞错）。
pub(crate) fn log_diag(msg: &str) {
    crate::app_log::write("bridge", msg);
}
