//! BridgeCore methods: state.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::shell_store::{ActivityState, SessionItem};

use super::*;

impl super::BridgeCore {
    /// Arc to self: `BridgeCore` is stored in an `Arc` by the UI-side Bridge.
    pub(crate) fn self_arc(&self) -> Arc<BridgeCore> {
        SHARED_CORE
            .get()
            .expect("bridge core not initialized")
            .clone()
    }

    // ── XAML 侧栏（shell_store 投影）──────────────────────────────

    /// (items, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新列表。
    pub(crate) fn session_snapshot(&self) -> (Vec<SessionItem>, u64) {
        let items = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.session_rev.load(Ordering::Relaxed);
        (items, rev)
    }

    pub(crate) fn active_seed(&self) -> String {
        self.active_seed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_active_seed(&self, seed: &str) {
        *self.active_seed.lock().unwrap_or_else(|e| e.into_inner()) = seed.to_string();
        // Composer 与 Dashboard 都是 active_seed 的投影。即使各自数据本身
        // 没有新事件，会话切换也必须唤醒 UI 重新读取对应 seed 的快照。
        self.composer_rev.fetch_add(1, Ordering::Relaxed);
        self.dashboard_rev.fetch_add(1, Ordering::Relaxed);
        // 交互缓存跟随活动会话：
        // 只显示当前会话的交互，后台会话请求保持挂起直至切回）。
        self.refresh_interaction_snapshot();
        // 标题栏直连：seed/view/title 随活动会话刷新。
        self.refresh_header();
    }

    /// Per-seed streaming projection. Conversation events are canonical once
    /// observed; session.activity is the reconnect/bootstrap fallback before
    /// this client has seen a turn event for that seed.
    pub(crate) fn seed_is_streaming(&self, seed: &str, now: u64) -> bool {
        if let Some(activity) = self
            .composer_activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(seed)
            .cloned()
        {
            return activity.is_streaming(now);
        }
        matches!(
            self.activities
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(seed),
            Some(ActivityState::Starting | ActivityState::Working | ActivityState::WaitingUser)
        )
    }

    // ── XAML 标题栏（header 投影，同 sessions 模式）────────────────

    /// (state, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新 TitleBar。
    pub(crate) fn header_snapshot(&self) -> (HeaderState, u64) {
        let state = self
            .header_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.header_rev.load(Ordering::Relaxed);
        (state, rev)
    }

    /// 壳侧组装标题栏状态：view/seed 来自壳导航与会话
    /// 切换，title 查会话列表，undo/compact disabled 由 conversation 事件
    /// 推断（对齐 Web：`turns.length === 0 || streaming` / `streaming`）。
    /// info_open/compacting/workspace 保留现值（本地状态，
    /// 不经 Web）。每次调用递增 rev（调用方在状态实际变化时触发）。
    pub(crate) fn refresh_header(&self) {
        let view = self
            .current_view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let seed = self.active_seed();
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let title = sessions
            .iter()
            .find(|s| s.seed == seed)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let now = unix_ms();
        let streaming = self.seed_is_streaming(&seed, now);
        let turns = self
            .header_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&seed)
            .copied()
            .unwrap_or(0);
        let compacting = self
            .compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&seed)
            .is_some_and(|status| status == "running");
        // 压缩终态浮出（F-N3）：running 不算终态，completed/failed 透传
        // 给 header 渲染 chip。
        let compact_result = self
            .compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&seed)
            .filter(|status| status.as_str() != "running")
            .cloned();
        // 工作区显示：合并方案——顶部与左侧同源。优先取当前选中组织
        // 工作区的 path（左侧筛选状态），否则回退到活动会话的归属
        // workspace path，再回退到会话 cwd（兼容旧 workspace.set 场景），
        // 保证“左侧选工作区 -> 顶部即显示该路径，左侧恒显未分组”消失。
        let (cur_ws_id, workspaces) = (
            self.current_workspace(),
            self.workspaces
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
        let workspace_display = if let Some(id) = cur_ws_id.as_deref() {
            workspaces
                .iter()
                .find(|w| w.id == id)
                .map(|w| self.display_remote_path(&w.path))
                .unwrap_or_default()
        } else {
            // 未选组织工作区时，按活动会话的组织归属推导
            let sess_ws_id = sessions
                .iter()
                .find(|s| s.seed == seed)
                .and_then(|s| s.workspace_id.as_deref());
            if let Some(ws_id) = sess_ws_id {
                workspaces
                    .iter()
                    .find(|w| w.id == ws_id)
                    .map(|w| self.display_remote_path(&w.path))
                    .unwrap_or_default()
            } else {
                // 兼容：会话有 cwd 但尚未归入组织（旧数据/跨机）
                sessions
                    .iter()
                    .find(|s| s.seed == seed)
                    .and_then(|s| s.cwd.as_deref())
                    .map(|c| self.display_remote_path(c))
                    .unwrap_or_default()
            }
        };
        let mut h = self.header_state.lock().unwrap_or_else(|e| e.into_inner());
        // 保留 workspace_error 优先展示：若有错误则不覆盖显示路径
        let has_error = h.workspace_error.is_some();
        h.view = view;
        h.seed = seed;
        h.title = title;
        h.compacting = compacting;
        h.compact_result = compact_result;
        h.undo_disabled = turns == 0 || streaming;
        h.compact_disabled = streaming;
        // 成功态才同步路径；错误态保留错误文案直到下次成功或用户切工作区
        if !has_error {
            if !workspace_display.is_empty() && h.workspace != workspace_display {
                h.workspace = workspace_display.clone();
            } else if workspace_display.is_empty() && h.workspace.is_empty() {
                // 保持空
            } else if !workspace_display.is_empty() {
                h.workspace = workspace_display.clone();
            }
        }
        // 若当前推导出有路径，自动清除旧错误（左侧选中已说明意图）
        if !workspace_display.is_empty() && h.workspace_error.is_some() {
            h.workspace_error = None;
            if h.workspace != workspace_display {
                h.workspace = workspace_display.clone();
            }
        }
        drop(h);
        self.header_rev.fetch_add(1, Ordering::Relaxed);
    }

    /// Record only the fact that this seed has at least one turn. Exact counts
    /// come from timeline snapshots; the header needs a replay-idempotent
    /// non-empty signal so Undo is enabled for a new session's first live turn.
    pub(crate) fn record_live_turn_started(&self, seed: &str) {
        let mut turns = self.header_turns.lock().unwrap_or_else(|e| e.into_inner());
        let count = turns.entry(seed.to_string()).or_default();
        *count = (*count).max(1);
    }

    /// 翻转标题栏本地开关（info_open）并递增 rev——壳本地状态，
    /// 不再回传 Web（headerAction::Info 通道随 WebView 移除而淘汰）。
    pub(crate) fn toggle_header_flag(&self, flag: HeaderFlag) {
        let mut h = self.header_state.lock().unwrap_or_else(|e| e.into_inner());
        match flag {
            HeaderFlag::Info => h.info_open = !h.info_open,
        }
        drop(h);
        self.header_rev.fetch_add(1, Ordering::Relaxed);
    }

    /// 清除压缩终态标记（F-N3：用户再次发起压缩前重置，failed 常驻的出口）。
    pub(crate) fn clear_compact_result(&self, seed: &str) {
        let removed = self
            .compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(seed)
            .is_some();
        if removed {
            self.refresh_header();
        }
    }

    /// 条件清除：仅当终态仍为 `expected` 时移除（completed 3s 自动淡出用，
    /// 防止竞态误删新发起压缩的 running 状态）。
    pub(crate) fn clear_compact_result_if(&self, seed: &str, expected: &str) {
        let mut map = self
            .compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if map.get(seed).map(|s| s.as_str()) == Some(expected) {
            map.remove(seed);
            drop(map);
            self.refresh_header();
        }
    }

    /// Composer 草稿持久化上限（F-N4）：超出时逐出任意非活跃条目。
    pub(crate) const MAX_COMPOSER_DRAFTS: usize = 32;

    /// 保存草稿快照（F-N4 存旧/写穿）；容量满则先逐出一个非活跃 seed。
    pub(crate) fn save_draft(&self, seed: &str, draft: crate::composer_bar::Draft) {
        let mut map = self
            .composer_drafts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if map.len() >= Self::MAX_COMPOSER_DRAFTS && !map.contains_key(seed) {
            let active = self.active_seed();
            if let Some(victim) = map.keys().find(|k| k.as_str() != active).cloned() {
                map.remove(&victim);
            }
        }
        map.insert(seed.to_string(), draft);
    }

    /// 取出草稿（F-N4 取新；move 语义——取出即离场，避免残留旧文本）。
    pub(crate) fn take_draft(&self, seed: &str) -> Option<crate::composer_bar::Draft> {
        self.composer_drafts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(seed)
    }

    // ── XAML 交互模态（interaction 投影，同 header 模式）────────────

    /// 调试入口：标题栏「Ask 测试」按钮直接注入一个 ask 交互。
    ///
    /// 与 daemon 事件走**同一条** `apply_interaction_event` → 快照 → rev 链路
    /// （仅数据源不同，事件形状与 `interaction_requested` 完全一致）——
    /// 用于二分定位「ask 弹不出」：按钮注入后若弹窗出现 → 前端渲染 OK，
    /// 问题在后端/协议/传输（真实事件未到达）；若仍不弹 → 前端渲染/轮询问题。
    /// （2026-08 标题栏按钮已移除；保留入口供调试，需要时从 main.rs 挂载。）
    #[allow(dead_code)]
    pub(crate) fn spawn_test_ask(&self) {
        let seed = self.active_seed();
        let ev = InteractionEvent::AskRequested {
            id: "test-ask".to_string(),
            questions: vec![
                AskQuestion {
                    id: "tq1".to_string(),
                    question: "Ask 面板渲染链路测试：你能看到这个弹窗吗？".to_string(),
                    options: vec![
                        "看到了，渲染正常".to_string(),
                        "看到了，但样式有问题".to_string(),
                        "没看到（弹窗未出现）".to_string(),
                    ],
                    allow_custom: true,
                },
                AskQuestion {
                    id: "tq2".to_string(),
                    question: "补充说明（必填）".to_string(),
                    options: vec![],
                    allow_custom: true,
                },
            ],
        };
        self.apply_interaction_event(&seed, ev);
    }

    /// (state, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新覆盖层面板。
    pub(crate) fn interaction_snapshot(&self) -> (InteractionState, u64) {
        let state = self
            .interaction
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.interaction_rev.load(Ordering::Relaxed);
        (state, rev)
    }
}
