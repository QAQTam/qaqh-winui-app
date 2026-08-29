//! BridgeCore methods: timeline.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use qaqh_client::{QueryRequest, TimelinePage, TimelineSnapshot};

use crate::shell_store::{
    ActivityState, DashboardSnapshot, WorkspaceItem, parse_activities, project_session_meta,
};

use super::*;

impl super::BridgeCore {
    /// 服务端是否还有更早回合（上滚翻页判定）。
    pub(crate) fn timeline_has_more(&self, seed: &str) -> bool {
        self.timeline_has_more
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(seed)
            .copied()
            .unwrap_or(false)
    }

    /// 子代理面板：异步拉取子代理 timeline 快照（尾部页）。
    /// 先 attach 子 seed（daemon `owns_seed` 校验要求）再拉；在途防重入
    /// （500ms 轮询抖动只发一次）。结果写 `subagent_timeline` 缓存，
    /// 面板轮询 peek 渲染；子代理已 close（worker 退出）时 attach/拉取
    /// 失败仅记日志——面板显示最后一次成功快照。
    pub(crate) fn spawn_fetch_subagent_timeline(&self, seed: &str) {
        let seed = seed.to_string();
        {
            let mut fetching = self
                .subagent_timeline_fetching
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !fetching.insert(seed.clone()) {
                return; // 已在途
            }
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("subagent_timeline {seed}: connect failed: {err}"));
                    core.subagent_timeline_fetching
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&seed);
                    return;
                }
            };
            // attach 幂等：已 attach 直接成功；失败（子代理已 close）不阻断拉取。
            if let Err(err) = client.attach(&seed).await {
                log_diag(&format!("subagent_timeline {seed}: attach failed: {err}"));
            }
            match client.fetch_timeline_page(&seed, None, None).await {
                Ok(page) => {
                    let turns = page.snapshot.turns.len();
                    *core
                        .subagent_timeline
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some((seed.clone(), page.snapshot));
                    log_diag(&format!("subagent_timeline {seed}: {turns} turns"));
                }
                Err(err) => log_diag(&format!("subagent_timeline {seed}: failed: {err}")),
            }
            core.subagent_timeline_fetching
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&seed);
        });
    }

    /// 子代理面板轮询读：最近一次拉取的 `(sub_seed, snapshot)`。
    pub(crate) fn subagent_timeline_peek(&self) -> Option<(String, TimelineSnapshot)> {
        self.subagent_timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 面板关闭时消费清空——子代理数据移出渲染内存（开面板才拉，关面板即释放）。
    pub(crate) fn subagent_timeline_consume(&self) {
        *self
            .subagent_timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 翻页加载更早回合：`fetch_timeline_page(seed, before_turn)`（纯读，
    /// 不重建 timeline SSE）→ 页入 `chat_prepend` 队列 + chat_rev++，
    /// chat_view 泵 drain 后 `Transcript::prepend_turns` 前插。
    /// 在途防重入（滚动抖动只发一次）；失败保留 has_more（下次滚动重试）。
    pub(crate) fn spawn_fetch_earlier(&self, seed: &str, before_turn: &str) {
        let seed = seed.to_string();
        let before_turn = before_turn.to_string();
        {
            let mut fetching = self
                .timeline_fetching
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !fetching.insert(seed.clone()) {
                return; // 已在途
            }
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("fetch_earlier {seed}: connect failed: {err}"));
                    core.timeline_fetching
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&seed);
                    return;
                }
            };
            match client.fetch_timeline_page(&seed, Some(&before_turn), None).await {
                Ok(page) => {
                    let has_more = page.has_more;
                    core.timeline_has_more
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(seed.clone(), has_more);
                    let turns = page.snapshot.turns.len();
                    if turns == 0 {
                        // 防御：空页（会话已删/竞态）——视为到底，不再翻页。
                        core.timeline_has_more
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(seed.clone(), false);
                    } else {
                        core.chat_prepend
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_back((seed.clone(), page.snapshot));
                        log_diag(&format!(
                            "fetch_earlier {seed}: page before {before_turn} ({turns} turns, has_more={has_more})"
                        ));
                    }
                }
                Err(err) => log_diag(&format!("fetch_earlier {seed}: failed: {err}")),
            }
            core.timeline_fetching
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&seed);
        });
    }

    /// 缓存 timeline 快照（`on_timeline_snapshot` 回调主体；独立方法便于单测）。
    ///
    /// - **seed 标记**：优先从快照 body 顶层读取（daemon 写回请求 seed，
    ///   权威来源）；缺失才回退 `last_timeline_seed`。不能依赖后者——
    ///   `spawn_timeline_refresh` 重拉时不更新它，且并发 resume 交错时它
    ///   会被后设值覆盖，快照被错误标记 → ChatView 泵永远判 stale →
    ///   无限 deferred 循环 → 历史永不恢复（日志风暴实证）。
    /// - **层级解包**：turns 在 `snapshot` 子对象（TimelineSnapshot：
    ///   `{"watermark", "turns"}`）。缓存子对象——消费方
    ///   `chat_adapter::timeline_turns` 直接读顶层 `turns`；缓存完整 body
    ///   则解析恒空 → restore 空历史 → ChatView 恢复后仍空白。
    pub(crate) fn cache_timeline_snapshot(&self, page: TimelinePage) {
        let seed = page.seed.clone();
        let seed = if seed.is_empty() {
            // 防御：client 已校验 seed 字段存在，缺失时回退旧标记。
            self.last_timeline_seed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            seed
        };
        // 分页元数据：完整响应 body 顶层 has_more（true = 还有更早回合，
        // ChatView 上滚翻页依据）。快照缓存整体替换时同步更新。必须在
        // inner 解包**之前**读取——unwrap_or 会 move snapshot。
        let has_more = page.has_more;
        let snapshot = page.snapshot;
        // BUG-004 根因 1：单槽缓存无 active 守卫——过期 resume 任务 /
        // 旧 TimelineStream 的 gap-recovery 快照会覆盖 active 的快照，
        // 造成"串会话 / 空 / 闪"。有 active 且 seed 不匹配时丢弃；
        // active 为空（冷启动/测试）保留旧行为。
        let active = self.active_seed();
        if !active.is_empty() && seed != active {
            log_diag(&format!(
                "timeline snapshot dropped: stale {seed} vs active {active}"
            ));
            return;
        }
        self.timeline_has_more
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(seed.clone(), has_more);
        // 标题栏直连：turns 计数在此缓存（不随 chat_view take 清空）
        // ——undo_disabled 判定源。timeline 快照即权威 turns（block 模型）。
        let turns = snapshot.turns.len();
        let last_turn_id = snapshot.turns.last().map(|t| t.turn_id.clone());
        // 后台转换任务的工作副本（原 snapshot move 进 raw 缓存槽）。
        let snap_clone = snapshot.clone();
        // BUG-003：move 进缓存（零拷贝），先取衍生值再 move。
        *self.chat_timeline.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((seed.clone(), snapshot));
        // P1-B2：serde roundtrip + 指纹移交 tokio blocking 线程——JSON Value
        // 建树/解析是切换帧的最大单点成本，不占 UI 线程。快照全 owned 类型
        // （Send）；单槽覆盖语义与 raw 槽一致（新快照晚到覆盖旧转换结果）。
        // SHARED_CORE 未初始化（单测直构）时跳过派发：就绪槽保持空，
        // UI 泵走「快照缺失 → 重拉」既有路径，语义不变。
        if let Some(core) = self.self_arc_opt() {
            let ready_seed = seed.clone();
            let _ = qaqh_client::runtime_handle().spawn_blocking(move || {
                let fp = crate::chat_adapter::snapshot_fingerprint(&snap_clone);
                let Some(converted) = crate::chat_adapter::timeline_snapshot(&snap_clone) else {
                    return;
                };
                *core
                    .chat_timeline_ready
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some((ready_seed, converted, fp));
            });
        }
        self.header_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(seed.clone(), turns);
        // 撤销直发：快照恢复的历史会话缓存最近回合 id。
        let mut last_turn_ids = self.last_turn_ids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tid) = last_turn_id {
            last_turn_ids.insert(seed.clone(), tid);
        } else {
            last_turn_ids.remove(&seed);
        }
        drop(last_turn_ids);
        self.refresh_header();
    }

    /// 消费后台转换完成的快照（seed 校验：不匹配保留，同 raw 槽 stale 语义）。
    pub(crate) fn chat_timeline_ready_take(
        &self,
        seed: &str,
    ) -> Option<(markdown_winui::TimelineSnapshot, u64)> {
        let mut slot = self
            .chat_timeline_ready
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot.as_ref()?.0 == seed {
            slot.take().map(|(_, snap, fp)| (snap, fp))
        } else {
            None
        }
    }

    /// 主动重拉指定 seed 的 timeline 快照（快照 seed 不匹配时的恢复路径）。
    /// daemon 对重复 activate 幂等（重推快照，无害）；节流 1s 防 16ms 泵
    /// 每 tick 触发。失败静默（快照保留在缓存，下一轮节流到期再试）。
    pub(crate) fn spawn_timeline_refresh(&self, seed: &str) {
        let mut last = self
            .timeline_refresh_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.elapsed() < REFRESH_THROTTLE {
            return;
        }
        *last = Instant::now();
        drop(last);
        let core = self.self_arc();
        let seed = seed.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("timeline refresh {seed}: connect failed: {err}"));
                    return;
                }
            };
            if let Err(err) = client.activate_timeline(&seed).await {
                log_diag(&format!("timeline refresh {seed}: activate failed: {err}"));
            }
        });
    }

    // ── XAML goalBar（dashboard 投影，control 事件驱动）─────────────

    /// (snapshot, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新 goalBar。
    pub(crate) fn dashboard_snapshot(&self) -> (Option<DashboardSnapshot>, u64) {
        let seed = self.active_seed();
        let snap = self
            .dashboards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&seed)
            .cloned();
        let rev = self.dashboard_rev.load(Ordering::Relaxed);
        (snap, rev)
    }

    /// control 频道 `dashboard_snapshot` 按 seed 落缓存并递增 rev。
    pub(crate) fn apply_dashboard(&self, snap: DashboardSnapshot) {
        if snap.seed.is_empty() {
            log_diag("dashboard: ignore snapshot without seed");
            return;
        }
        self.dashboards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(snap.seed.clone(), snap);
        self.dashboard_rev.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn seed_set(&self) -> HashSet<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|s| s.seed.clone())
            .collect()
    }

    /// XAML 侧生成 command_id（无 uuid 依赖；幂等键只需进程内唯一 + 单调）。
    pub(crate) fn next_command_id(&self) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("xaml-{ms}-{n}")
    }

    /// 后台刷新 `session.list` + `session.activity` → 投影进缓存 → rev++。
    /// UI 侧（sidebar timer）读取快照即可，无需跨线程回调。
    pub(crate) fn spawn_refresh_sessions(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            core.refresh_sessions_inner().await;
        });
    }

    pub(crate) async fn refresh_sessions_inner(&self) {
        let client = match self.ensure_client().await {
            Ok(client) => client,
            Err(err) => {
                log_diag(&format!("refresh_sessions: connect failed: {err}"));
                return;
            }
        };
        let list = match client.query(QueryRequest::SessionList).await {
            Ok(v) => v,
            Err(err) => {
                log_diag(&format!("refresh_sessions: session.list failed: {err}"));
                return;
            }
        };
        let acts = match client.query(QueryRequest::SessionActivity).await {
            Ok(v) => v,
            Err(err) => {
                log_diag(&format!("refresh_sessions: session.activity failed: {err}"));
                return;
            }
        };
        let activities: HashMap<String, ActivityState> =
            parse_activities(&acts).into_iter().collect();
        let mut items = Vec::new();
        if let Some(arr) = list.as_array() {
            items.reserve(arr.len());
            for v in arr {
                let seed = v.get("seed").and_then(|s| s.as_str()).unwrap_or("");
                let running = v.get("running").and_then(|r| r.as_bool()).unwrap_or(false);
                if let Some(item) = project_session_meta(v, activities.get(seed).copied(), running)
                {
                    items.push(item);
                }
            }
        }
        let live_seeds: HashSet<String> = items.iter().map(|item| item.seed.clone()).collect();
        self.compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|seed, _| live_seeds.contains(seed));
        *self.sessions.lock().unwrap_or_else(|e| e.into_inner()) = items;
        *self.activities.lock().unwrap_or_else(|e| e.into_inner()) = activities;
        self.session_rev.fetch_add(1, Ordering::Relaxed);
        log_diag(&format!(
            "refresh_sessions: {} sessions",
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len()
        ));
        // 标题栏直连：会话列表刷新后 title 可能变化（重命名/首轮摘要）。
        self.refresh_header();
    }

    // ── Workspace（sidebar workspace 树数据源）────────────────────

    /// (items, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新（同 session_snapshot）。
    pub(crate) fn workspace_snapshot(&self) -> (Vec<WorkspaceItem>, u64) {
        let items = self
            .workspaces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.workspace_rev.load(Ordering::Relaxed);
        (items, rev)
    }
}
