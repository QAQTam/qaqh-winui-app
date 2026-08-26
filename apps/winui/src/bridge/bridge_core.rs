//! BridgeCore state container, constants and connection helpers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qaqh_app_notifications::Notifier;
use qaqh_client::{Channel, ChannelStatus, Client, TimelineSnapshot, TimelineStatus};
use serde_json::Value;

use crate::shell_store::{
    ActivityState, DashboardSnapshot, SessionDetail, SessionItem, SettingsSnapshot, SkillsSnapshot,
    WorkspaceItem,
};

use super::*;
/// `Send + Sync` half of the bridge: client, lease bookkeeping, outbox sender.
/// Lives on the tokio side.
pub(crate) struct BridgeCore {
    pub(crate) client: Mutex<Option<Client>>,
    pub(crate) attached: Mutex<HashSet<String>>,
    /// Latest native transport state for each Ringing channel.
    pub(crate) channel_status: Mutex<HashMap<Channel, ChannelStatus>>,
    /// XAML 侧栏数据源：会话列表投影（`session.list` + `session.activity`）。
    pub(crate) sessions: Mutex<Vec<SessionItem>>,
    /// XAML 侧栏工作区数据源（`workspace.list` 投影）。
    pub(crate) workspaces: Mutex<Vec<WorkspaceItem>>,
    /// 工作区数据版本：refresh 后递增，UI 侧 timer 比对后刷新（同 session_rev）。
    pub(crate) workspace_rev: AtomicU64,
    /// 当前选中 workspace id（None = 未分组视图）；sidebar/tabs 共享。
    pub(crate) current_workspace: Mutex<Option<String>>,
    /// 实时活动状态（control `session_activity_changed` 事件增量更新）。
    pub(crate) activities: Mutex<HashMap<String, ActivityState>>,
    /// 侧栏数据版本：refresh / activity 事件后递增，UI 侧 timer 比对后刷新。
    pub(crate) session_rev: AtomicU64,
    /// XAML 侧栏当前选中的会话 seed。
    pub(crate) active_seed: Mutex<String>,
    /// XAML 标题栏数据源，由壳导航、会话列表和 conversation 事件组装。
    pub(crate) header_state: Mutex<HeaderState>,
    /// 标题栏状态版本：组装/投影后递增，UI 侧 timer 比对后刷新（同 session_rev）。
    pub(crate) header_rev: AtomicU64,
    /// per-seed turns 计数（undo_disabled 判定源）：timeline 快照提供精确
    /// 数量；实时 TurnStarted 至少把存在性置为 1（标题栏只关心是否为空）。
    pub(crate) header_turns: Mutex<HashMap<String, usize>>,
    /// Per-seed compact projection restored from conversation events/bootstrap.
    /// The header only selects the active seed; background sessions never mutate
    /// another tab's progress state.
    pub(crate) compact_statuses: Mutex<HashMap<String, String>>,
    /// per-seed 最近回合 id（undo 命令用）：turn_started 事件/快照写入点
    /// 更新；无缓存时撤销按钮直发层拒绝发送。
    pub(crate) last_turn_ids: Mutex<HashMap<String, String>>,
    /// daemon 失联检测（A 方案，WORKFLOW §7）：timeline 流非 Open 的起始时刻。
    pub(crate) timeline_stall_since: Mutex<Option<Instant>>,
    /// 三 ringing 通道无一 Open 的起始时刻。
    pub(crate) channels_stall_since: Mutex<Option<Instant>>,
    /// 重建进行中（防 ensure_client 重入）。
    pub(crate) rebuilding: AtomicBool,
    /// 连接进行中（防并发 invoke 各自 connect_async → 各自 spawn daemon）。
    /// 首个调用者置位并真正发起连接，其余调用者轮询等待其结果。
    pub(crate) connecting: AtomicBool,
    /// 最近一次重建时刻（冷却防抖，避免网络抖动时反复重建）。
    pub(crate) last_rebuild_at: Mutex<Instant>,
    /// 最近一次"无 client 自动重连"时刻（独立冷却，见 AUTO_RECONNECT_COOLDOWN）。
    pub(crate) last_auto_reconnect_at: Mutex<Instant>,
    /// 连续 rebuild 失败计数（指数退避冷却用；成功清零）。
    pub(crate) rebuild_failures: AtomicU32,
    /// 最近一次 timeline.activate 的 seed（重建后恢复前端 transcript 流）。
    pub(crate) last_timeline_seed: Mutex<String>,
    /// timeline 连接状态缓存（检测用；ringing 状态走 channel_status）。
    pub(crate) timeline_status: Mutex<Option<TimelineStatus>>,
    /// XAML 技能页数据源：最近 `skills_updated` 事件完整载荷（WORKFLOW §8）。
    pub(crate) skills: Mutex<Option<SkillsSnapshot>>,
    /// 技能数据版本：事件/拉取后递增，UI 侧 timer 比对后刷新（同 session_rev）。
    pub(crate) skills_rev: AtomicU64,
    /// 壳主导的当前视图（`navigate` 同步；XAML 视图族接管 skills 的判定源）。
    pub(crate) current_view: Mutex<String>,
    /// 远端 daemon 直连档案（None = 本地模式；见 [`RemoteProfile`]）。
    pub(crate) remote_profile: Mutex<Option<RemoteProfile>>,
    /// 远端文件选择器：`fs.list` 结果投影。
    pub(crate) remote_fs_listing: Mutex<RemoteFsListing>,
    /// `fs.list` 数据版本（picker 轮询比对）。
    pub(crate) remote_fs_rev: AtomicU64,
    /// 远端文件选择器：`fs.read` 文本预览投影。
    pub(crate) remote_fs_preview: Mutex<Option<RemoteFsPreview>>,
    /// `fs.read` 数据版本。
    pub(crate) remote_fs_preview_rev: AtomicU64,
    /// XAML 设置页数据源：`config.load` + `skills.list_tools` 合并投影。
    pub(crate) settings: Mutex<Option<SettingsSnapshot>>,
    /// 设置数据版本：config.load / tools 拉取后递增，UI 侧 timer 比对后刷新。
    pub(crate) settings_rev: AtomicU64,
    /// XAML-local appearance and workspace preferences.
    pub(crate) settings_proj: Mutex<SettingsProjection>,
    /// Local preference version used by the UI refresh loop.
    pub(crate) settings_proj_rev: AtomicU64,
    /// 最近一次 config.save 结果（None=尚无新结果）。Ok(())=成功、Err(msg)=失败。
    /// 由 spawn_config_save 写入；设置页轮询 save_status_rev 变化后消费，驱动
    /// 「已保存 ✓」/错误提示（2026-08-25 R3：此前失败仅进 log_diag，UI 恒报成功）。
    pub(crate) save_status: Mutex<Option<Result<(), String>>>,
    /// save_status 数据版本：每次写入递增，UI 侧比对后刷新。
    pub(crate) save_status_rev: AtomicU64,
    /// XAML Info 面板数据源：bootstrap `conversation.state` 投影。
    pub(crate) info: Mutex<Option<SessionDetail>>,
    /// Info 数据版本：refresh 后递增，UI 侧 timer 比对后刷新（同 session_rev）。
    pub(crate) info_rev: AtomicU64,
    /// XAML interaction modal view model.
    pub(crate) interaction: Mutex<InteractionState>,
    /// Interaction version used by the UI refresh loop.
    pub(crate) interaction_rev: AtomicU64,
    /// daemon control/tool 事件直接组装的 per-seed 交互状态机。
    pub(crate) interactions: Mutex<HashMap<String, InteractionMachine>>,
    /// Composer version used by the UI refresh loop.
    pub(crate) composer_rev: AtomicU64,
    /// Rust 直连 composer 活动追踪（per seed）：conversation 频道事件
    /// 直连解析 isStreaming（卡死检测）/model/context（usage_updated 缓存）
    /// ——读路径直连，不经 WebView（终局数据源）。`hasPendingGate` 复用
    /// 交互队列状态机（interactions）。
    pub(crate) composer_activity: Mutex<HashMap<String, ComposerActivity>>,
    /// 直连模式的 mode 本地缓存（Web 单例语义：会话共享，默认 "plan"）。
    pub(crate) composer_mode: Mutex<String>,
    /// 直连模式的工具模式本地缓存（standard/minimal/custom + custom_tools）。
    /// 乐观更新 + `sync_tool_mode_from_meta` 从 meta.json 回填（外部变化/
    /// 会话切换 250ms 轮询内可见；首帧即正确初始值）。
    pub(crate) composer_tool_mode: Mutex<ToolModeState>,
    /// 直连模式的发送反馈（submitError / sendAck），按 seed 隔离。
    /// 上传或 ACK 可能在切换会话后才返回，不能污染新会话草稿。
    pub(crate) composer_feedback: Mutex<HashMap<String, ComposerFeedback>>,
    /// 子代理工具调用追踪（per-seed，key=tool_call_id）：工作状态区数据源。
    /// `ToolCallPrepared`/`ToolStarted` 建实例（解析 agent_name），
    /// `[SUBAGENT ...]` 注入 tag 收敛终态，快照时惰性幽灵检测。
    /// 内存态：重连/换 seed 后不恢复（transcript 中注入回合仍可见）。
    pub(crate) subagent_tracker: Mutex<HashMap<String, HashMap<String, SubagentItem>>>,
    /// 原生 ChatView 事件队列：conversation 频道渲染相关事件（turn/round/
    /// delta/checkpoint）直连缓存，UI 线程 timer drain 喂 Transcript。
    /// Queue entries are canonical typed Ringing events; the adapter only maps
    /// domain variants into presentation models.
    ///
    /// **seed 标记（2026-08-08 修复）**：事件按 seed 分队列——
    /// daemon 的 SSE 流按 lease 推送**所有**会话的事件（batch.seed 区分），
    /// 此前入队忽略 seed、drain 全量返回，后台会话增量会污染活动会话的
    /// Transcript（切换瞬间残留事件串台）。现入队带 seed；有界帧泵只
    /// 消费 active_seed 并保留其它 seed，无界恢复入口才统一丢积压。
    ///
    /// **队列上限（保险丝）**：窗口不可见期间 vsync 暂停、事件不消费，
    /// 队列会无限积压（后台 1 小时 × 1000 token/s ≈ 百万级事件）。
    /// 超限时优先丢弃 RoundDelta（由 round 完成的 final 覆盖兜底，且
    /// 后台恢复走快照）；结构性事件强制入队（挤出最旧）。恢复可见时
    /// `chat_view` 的 background resume 会整体丢弃积压再拉快照。
    /// timeline live 事件队列（Phase 2：BlockTranscript 单源；delta 可丢
    /// （checkpoint 自愈 + 快照兜底），结构性事件强制入队。
    pub(crate) timeline_events: Mutex<TimelineEventQueues>,
    /// Composer 草稿持久层（F-N4）：seed → 草稿快照，页面切换/会话切换
    /// 不再随 use_ref 销毁；容量上限见 core_state::MAX_COMPOSER_DRAFTS。
    pub(crate) composer_drafts: Mutex<HashMap<String, crate::composer_bar::Draft>>,
    /// timeline 事件数据版本：入队后递增，UI 侧 timer 比对后 drain。
    pub(crate) timeline_rev: AtomicU64,
    /// BUG-003：resume 意图代次。每次 spawn_resume 递增；异步任务在
    /// set_active_seed / activate_timeline / navigate 三处副作用前校验，
    /// 过期任务立即返回——只有最新点击能切换会话。
    pub(crate) resume_generation: AtomicU64,
    /// 最近一次 typed timeline 快照（`TimelineSnapshot` + 所属 seed：
    /// 权威 turns 历史，resume 旧对话的数据源；chat_view 泵消费 restore）。
    /// seed 标记防竞态：快速切会话时旧快照晚到不会被灌进新会话。
    pub(crate) chat_timeline: Mutex<Option<(String, TimelineSnapshot)>>,
    /// 子代理面板数据：最近一次拉取的子代理 timeline 快照
    /// （`(sub_seed, TimelineSnapshot)`）。`spawn_fetch_subagent_timeline`
    /// 异步拉取后写入，面板轮询 `subagent_timeline_peek` 消费渲染；
    /// 面板关闭即 `consume` 清空——数据不驻留渲染内存。
    pub(crate) subagent_timeline: Mutex<Option<(String, TimelineSnapshot)>>,
    /// 子代理 timeline 拉取在途标记（seed 集合）：500ms 轮询防重入。
    pub(crate) subagent_timeline_fetching: Mutex<std::collections::HashSet<String>>,
    /// 分页元数据：seed → 服务端是否还有更早回合（快照缓存时同步更新）。
    /// ChatView 上滚到窗口顶部且 `expand_window` 已全量放行时据此翻页。
    pub(crate) timeline_has_more: Mutex<std::collections::HashMap<String, bool>>,
    /// 更早回合分页页（`(seed, TimelineSnapshot)`）：`spawn_fetch_earlier`
    /// 异步拉取后入队，chat_view 泵 drain 后 `Transcript::prepend_turns`
    /// 前插（与 `chat_timeline` 的整包替换语义区分）。
    pub(crate) chat_prepend: Mutex<std::collections::VecDeque<(String, TimelineSnapshot)>>,
    /// 分页在途标记（seed 集合）：防止滚动抖动时重复发起同一翻页请求。
    pub(crate) timeline_fetching: Mutex<std::collections::HashSet<String>>,
    /// 快照重拉节流：seed 不匹配时主动 `activate_timeline` 重拉（daemon
    /// 幂等重推快照）；16ms 泵每 tick 都会看到不匹配快照，须限频。
    pub(crate) timeline_refresh_at: Mutex<Instant>,
    /// XAML goalBar 数据源：control 频道 `dashboard_snapshot` 按 seed 缓存。
    /// 后台会话仍会收到 control 事件，不能用单份全局快照承载。
    pub(crate) dashboards: Mutex<HashMap<String, DashboardSnapshot>>,
    /// 桌面通知器（Phase 1：TurnCompleted 预览 + 点击回前台）。
    /// Arc：show 可在独立线程执行（WinRT 调用不进 UI 线程消息泵）。
    pub(crate) notifier: Mutex<Option<Arc<Notifier>>>,
    /// 桌面通知开关（前端本地偏好 ui-preferences.json，不入 daemon config）。
    pub(crate) notif_enabled: AtomicBool,
    /// dashboard 数据版本：事件到达后递增，UI 侧 timer 比对后刷新。
    pub(crate) dashboard_rev: AtomicU64,
}

/// 失联阈值：backoff 1+2+4+8=15s 内 4 次重试仍失败视为失联（daemon 重启/关闭）。
pub(crate) const STALL_THRESHOLD: Duration = Duration::from_secs(15);
/// 重建冷却：网络抖动时避免每 15s 重建一次。
pub(crate) const REBUILD_COOLDOWN: Duration = Duration::from_secs(60);
/// 无 client 自动重连冷却：首次 connect 失败（daemon 初始化窗口）后
/// 尽快恢复，比 stall 重建的 60s 冷却短。
pub(crate) const AUTO_RECONNECT_COOLDOWN: Duration = Duration::from_secs(5);
/// 等待并发连接完成的上限：覆盖 discovery 等待（8s）+ open 协商（10s）+
/// 余量。超过即视为连接失败（调用方重试机制兜底）。
pub(crate) const CONNECT_WAIT_TIMEOUT: Duration = Duration::from_secs(25);
/// ChatView 快照重拉节流：seed 不匹配时主动 activate_timeline 重拉，
/// 16ms 泵每 tick 都会看到不匹配快照，1s 限频防 activate 风暴。
pub(crate) const REFRESH_THROTTLE: Duration = Duration::from_secs(1);
/// 连续失败后 rebuild 冷却指数退避封顶（60s → 120s → 240s → 480s → 960s）。
pub(crate) const REBUILD_BACKOFF_CAP: u32 = 4;

/// Timeline 事件队列上限（保险丝）：窗口不可见期间 vsync 暂停、事件不消费，
/// 无上限会无限积压。delta 可丢（BlockCheckpoint 覆盖自愈 + 快照兜底），
/// 结构性事件强制入队。4K ≈ 前台 60s 缓冲（timeline 每 token 一 delta，
/// checkpoint 自愈 + 快照兜底使其对丢弃更鲁棒）。
pub(crate) const TIMELINE_QUEUE_CAP: usize = 4_000;

/// rebuild 冷却：连续失败后指数拉长（60s→960s 封顶），防止 rebuild
/// 风暴把 daemon 连接数打爆（32 连接信号量 → 静默 drop）。
pub(crate) fn rebuild_cooldown_for(failures: u32) -> Duration {
    REBUILD_COOLDOWN.saturating_mul(1u32 << failures.min(REBUILD_BACKOFF_CAP))
}

/// 无 client 自动重连冷却：同样受失败计数退避保护（5s→320s 封顶）。
pub(crate) fn auto_reconnect_cooldown_for(failures: u32) -> Duration {
    AUTO_RECONNECT_COOLDOWN.saturating_mul(1u32 << failures.min(REBUILD_BACKOFF_CAP + 2))
}

/// 前端本地偏好文件（`%LOCALAPPDATA%\QAQ-Harness\ui-preferences.json`；无
/// LOCALAPPDATA 时落 cwd——与 oobe.done 同目录惯例）。
pub(crate) fn notif_prefs_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base)
        .join("QAQ-Harness")
        .join("ui-preferences.json")
}

/// 写本地通知偏好文件（ui-preferences.json 的 `notificationsEnabled` 键）。
/// 通知开关的单一权威源是后端 config（`notifications_enabled`）；本文件仅作
/// 启动早期（config.load 到达前）的本地镜像，写入方需同时持久化到后端。
pub(crate) fn write_notif_pref(enabled: bool) {
    let path = notif_prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let prefs = serde_json::json!({ "notificationsEnabled": enabled });
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&prefs).unwrap_or_default(),
    );
}

/// 远端 daemon 档案文件（与 ui-preferences.json 同目录，独立文件便于重置）。
pub(crate) fn remote_profile_path() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base)
        .join("QAQ-Harness")
        .join("remote-profile.json")
}

/// 启动时加载远端档案；文件缺失/损坏视为本地模式（不阻塞启动）。
pub(crate) fn load_remote_profile() -> Option<RemoteProfile> {
    let path = remote_profile_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<RemoteProfile>(&raw) {
        Ok(profile) if !profile.base_url.is_empty() => Some(profile),
        Ok(_) => {
            log_diag(&format!(
                "remote profile has empty base_url: {}",
                path.display()
            ));
            None
        }
        Err(error) => {
            log_diag(&format!(
                "remote profile unreadable at {}: {error}",
                path.display()
            ));
            None
        }
    }
}

pub(crate) fn write_remote_profile(profile: &RemoteProfile) -> std::io::Result<()> {
    let path = remote_profile_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(profile).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

pub(crate) fn remove_remote_profile_file() {
    let _ = std::fs::remove_file(remote_profile_path());
}

/// 解析 `fs.list` 返回数组中的单条目（字段与 `qaqh-runtime` 对齐）。
pub(crate) fn parse_remote_fs_entry(value: &Value) -> Option<RemoteFsEntry> {
    Some(RemoteFsEntry {
        name: value.get("name")?.as_str()?.to_string(),
        path: value.get("path")?.as_str()?.to_string(),
        is_dir: value
            .get("is_dir")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_file: value
            .get("is_file")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        size: value.get("size").and_then(Value::as_u64).unwrap_or(0),
    })
}
/// 点击通知 → 激活主窗口（WinUI 3 桌面窗口类名；最小化则恢复）。
pub(crate) fn activate_main_window() {
    unsafe {
        use windows::Win32::winuser as w;
        let class = windows::core::w!("WinUIDesktopWin32WindowClass");
        let hwnd = w::FindWindowW(class, None);
        if hwnd.0.is_null() {
            return;
        }
        let _ = w::ShowWindow(hwnd, w::SW_RESTORE);
        let _ = w::SetForegroundWindow(hwnd);
    }
}
