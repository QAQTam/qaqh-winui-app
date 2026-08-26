//! BridgeCore methods: interaction.

use std::sync::atomic::Ordering;

use qaqh_client::TimelineSnapshot;

use super::*;

impl super::BridgeCore {
    // ── 上下文构成分布（composer 底部堆叠条数据源）──────────────────

    /// (stats, rev) 快照：读 `sessions/{seed}/meta.json` 的 `context_stats`
    /// 字段（engine_misc/engine_compact 写入；6 段构成同 Web ContextPanel
    /// 饼图）。rev = meta.json 文件 mtime（变化才刷新）；字段缺失 →
    /// (None, 0)。同步读小文件，轮询间隔内开销可忽略。
    pub(crate) fn context_stats_snapshot(&self) -> (Option<crate::shell_store::ContextStats>, u64) {
        let seed = self.active_seed();
        let path = crate::bridge::context_stats_path(&seed);
        let Ok(meta) = std::fs::metadata(&path) else {
            return (None, 0);
        };
        let rev = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let stats = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|meta| meta.get("context_stats").cloned())
            .and_then(|field| serde_json::from_value(field).ok());
        (stats, rev)
    }

    /// 从 `sessions/{seed}/meta.json` 同步工具模式到本地缓存（变化时 bump
    /// composer_rev）。调用点：`composer_snapshot` 开头——poll_rev 每次都会
    /// 调 snapshot 取 rev，因此外部变化（daemon persist 后、会话切换）在
    /// 250ms 轮询内可见；首帧即读到正确初始值。同步读小文件，开销同
    /// `context_stats_snapshot`。
    pub(crate) fn sync_tool_mode_from_meta(&self) {
        let seed = self.active_seed();
        if seed.is_empty() {
            return;
        }
        let path = crate::bridge::context_stats_path(&seed);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let mode = meta
            .get("tool_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let custom_tools = meta
            .get("custom_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut cur = self
            .composer_tool_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cur.mode != mode || cur.custom_tools != custom_tools {
            *cur = ToolModeState { mode, custom_tools };
            self.composer_rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 从 `sessions/{seed}/meta.json` 同步 AGENT_MODE（0=Normal/2=Code →
    /// "code"，1=Plan → "plan"）。后端会话恢复时按 saved_mode 设置进程级
    /// AGENT_MODE（engine_session.rs）；前端若无此同步会在会话切换/重启后
    /// 停留默认值，导致「规划/执行」显示与后端实际拦截不一致。
    /// 与 `sync_tool_mode_from_meta` 同款：composer_snapshot 开头调用。
    pub(crate) fn sync_mode_from_meta(&self) {
        let seed = self.active_seed();
        if seed.is_empty() {
            return;
        }
        let path = crate::bridge::context_stats_path(&seed);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let mode = meta.get("mode").and_then(|v| v.as_u64()).unwrap_or(0);
        // Normal(0) 与 Code(2) 后端行为等价（均不拦截 PLAN_BLOCKED）；UI
        // 二态统一映射为 "code"（显示「执行」），Plan(1) → "plan"（「规划」）。
        let ui_mode = if mode == 1 { "plan" } else { "code" };
        let mut cur = self.composer_mode.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != ui_mode {
            *cur = ui_mode.to_string();
            self.composer_rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 应用 daemon control 事件到交互队列状态机
    /// 幂等：SSE 重连续传重放事件经 PartialEq 比对不产生多余 rev。
    /// 注意：机器按**事件 seed** 更新（后台会话交互保持挂起），缓存只投影
    /// **active_seed** 的机器，后台会话不会覆盖当前 UI。
    pub(crate) fn apply_interaction_event(&self, seed: &str, ev: InteractionEvent) {
        let ev_kind = match &ev {
            InteractionEvent::AskRequested { .. } => "AskRequested",
            InteractionEvent::AskResolved { .. } => "AskResolved",
            InteractionEvent::PlanRequested { .. } => "PlanRequested",
            InteractionEvent::PlanResolved { .. } => "PlanResolved",
            InteractionEvent::GhostCleanup => "GhostCleanup",
        };
        let mut machines = self.interactions.lock().unwrap_or_else(|e| e.into_inner());
        machines.entry(seed.to_string()).or_default().apply(ev);
        let keys: Vec<String> = machines.keys().cloned().collect();
        drop(machines);
        // 【ask 弹不出诊断】投影前实值：事件类型/事件 seed vs active_seed vs 机器键集。
        log_diag(&format!(
            "[INTERACT] t={} applied ev={ev_kind} ev_seed={seed} active_seed={} machines={keys:?}",
            unix_ms(),
            self.active_seed()
        ));
        self.refresh_interaction_snapshot();
    }

    /// tool 频道变体（permission 队列独立于 ask/plan 状态机）。
    pub(crate) fn apply_tool_permission_event(&self, seed: &str, ev: ToolPermissionEvent) {
        let mut machines = self.interactions.lock().unwrap_or_else(|e| e.into_inner());
        machines.entry(seed.to_string()).or_default().apply_tool(ev);
        drop(machines);
        self.refresh_interaction_snapshot();
    }

    /// 将 active_seed 对应机器的快照写入缓存（无该 seed 机器 → 空交互）。
    /// 快照未变化（PartialEq）不递增 rev——重放/无关会话事件零开销。
    pub(crate) fn refresh_interaction_snapshot(&self) {
        let active = self.active_seed();
        let machines = self.interactions.lock().unwrap_or_else(|e| e.into_inner());
        let hit = machines.get(&active).is_some();
        let next = machines
            .get(&active)
            .map(|m| m.snapshot(&active))
            .unwrap_or_default();
        drop(machines);
        let mut cur = self.interaction.lock().unwrap_or_else(|e| e.into_inner());
        let rev = self.interaction_rev.load(Ordering::Relaxed);
        // 【ask 弹不出诊断】仅在投影将变化或未命中时打（避免稳态刷屏）。
        if *cur != next || !hit {
            log_diag(&format!(
                "[INTERACT] t={} refresh active={active} hit={hit} next_kind={} cur_kind={} rev={rev}",
                unix_ms(),
                next.kind,
                cur.kind
            ));
        }
        if *cur != next {
            *cur = next;
            self.interaction_rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ── XAML Composer native view model ──────────────────────────────

    /// (state, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新底部栏。
    /// Combines typed conversation activity, interaction gates, settings, and
    /// UI-local command feedback.
    /// tool 频道生命周期更新工作阶段（状态栏数据源）。
    pub(crate) fn set_composer_phase(&self, seed: &str, phase: WorkPhase) {
        let now = unix_ms();
        let mut map = self
            .composer_activity
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(seed.to_string()).or_default();
        entry.phase = phase;
        entry.last_activity_at = now;
        drop(map);
        // 状态变化 → 唤醒 composer 刷新（UI timer 比对 rev）。
        self.composer_rev.fetch_add(1, Ordering::Relaxed);
    }

    // ── 子代理追踪（工作状态区数据源）────────────────────────────

    /// 建/更新子代理实例：`ToolCallPrepared`（带 args_so_far 解析 agent_name）
    /// 与 `ToolStarted` 都会到达；后者无参数，只补 started_at。
    pub(crate) fn upsert_subagent(&self, seed: &str, call_id: &str, args_so_far: Option<&str>) {
        let now = unix_ms();
        let mut map = self
            .subagent_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let per_seed = map.entry(seed.to_string()).or_default();
        let entry = per_seed
            .entry(call_id.to_string())
            .or_insert_with(|| SubagentItem {
                call_id: call_id.to_string(),
                // 解析失败回退短哈希：保证胶囊有可读标识。
                name: format!("sub-{}", call_id.chars().take(6).collect::<String>()),
                seed: String::new(),
                state: SubagentState::Working,
                started_at: now,
                finished_at: 0,
            });
        if let Some(args) = args_so_far
            && let Some(name) = parse_agent_name(args)
        {
            entry.name = name;
        }
        drop(map);
        self.composer_rev.fetch_add(1, Ordering::Relaxed);
    }

    /// 按注入 tag 收敛终态：`[SUBAGENT 'name' COMPLETED]` 等。
    /// 只收敛 Working/Lost 项（真实终态优先于幽灵标记）；找不到同名项
    /// （重连丢失 tracker）静默忽略——注入回合本身仍在 transcript 可见。
    pub(crate) fn converge_subagent(&self, seed: &str, name: &str, state: SubagentState) {
        let now = unix_ms();
        let mut map = self
            .subagent_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        if let Some(per_seed) = map.get_mut(seed) {
            for item in per_seed.values_mut() {
                if item.name == name
                    && matches!(item.state, SubagentState::Working | SubagentState::Lost)
                {
                    item.state = state;
                    item.finished_at = now;
                    changed = true;
                }
            }
        }
        drop(map);
        if changed {
            self.composer_rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录 spawn_subagent 返回的子代理 seed（按 call_id 关联 tracker 条目）。
    /// 供子代理面板按需拉取 timeline；条目不存在（重连丢失）则忽略。
    pub(crate) fn record_subagent_seed(&self, seed: &str, call_id: &str, sub_seed: &str) {
        let mut map = self
            .subagent_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        if let Some(per_seed) = map.get_mut(seed)
            && let Some(item) = per_seed.get_mut(call_id)
            && item.seed.is_empty()
        {
            item.seed = sub_seed.to_string();
            changed = true;
        }
        drop(map);
        if changed {
            self.composer_rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 快照时惰性幽灵检测：Working 超过窗口 → Lost（不 bump rev；
    /// 下一轮事件自然刷新，10 分钟级别滞后无感知）。
    pub(crate) fn ghost_sweep(&self, seed: &str, now: u64) {
        let mut map = self
            .subagent_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(per_seed) = map.get_mut(seed) {
            for item in per_seed.values_mut() {
                if item.state == SubagentState::Working
                    && item.started_at > 0
                    && now.saturating_sub(item.started_at) > SUBAGENT_GHOST_TIMEOUT_MS
                {
                    item.state = SubagentState::Lost;
                    item.finished_at = now;
                }
            }
        }
    }

    /// 当前 seed 的子代理胶囊（按启动时刻升序；供 composer_snapshot 合并）。
    pub(crate) fn subagent_items(&self, seed: &str, now: u64) -> Vec<SubagentItem> {
        self.ghost_sweep(seed, now);
        let map = self
            .subagent_tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<SubagentItem> = map
            .get(seed)
            .map(|per_seed| per_seed.values().cloned().collect())
            .unwrap_or_default();
        items.sort_by_key(|i| i.started_at);
        items
    }

    pub(crate) fn composer_snapshot(&self) -> (ComposerState, u64) {
        // meta.json 工具模式 + AGENT_MODE 回填（外部变化/会话切换 250ms 内可见）。
        self.sync_tool_mode_from_meta();
        self.sync_mode_from_meta();
        let rev = self.composer_rev.load(Ordering::Relaxed);
        let active = self.active_seed();
        let now = unix_ms();
        let activity = self
            .composer_activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&active)
            .cloned();
        // hasPendingGate 复用交互队列状态机（permission/ask/plan 任一挂起）。
        let gate = self
            .interactions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&active)
            .map(|m| m.has_pending())
            .unwrap_or(false);
        let is_streaming = activity
            .as_ref()
            .map(|a| a.is_streaming(now))
            .unwrap_or_else(|| self.seed_is_streaming(&active, now));
        let model = activity
            .as_ref()
            .map(|a| a.model.clone())
            .unwrap_or_default();
        let context_tokens = activity.as_ref().map(|a| a.prompt_tokens).unwrap_or(0);
        let context_limit = activity.as_ref().map(|a| a.context_limit).unwrap_or(0);
        let mut state = ComposerState::default();
        state.seed = active.clone();
        state.is_streaming = is_streaming;
        state.has_pending_gate = gate;
        // phase 与 is_streaming 同源门控（F-N5）：SSE 断连后 activity 永远等
        // 不到 Ended，存储的 phase 若不门控，「飞速思考中…/奋力回答中…」
        // 标签将永久残留（is_streaming 自身有 4min stall 超时，phase 沒有）。
        state.phase = if is_streaming {
            activity.map(|a| a.phase).unwrap_or_default()
        } else {
            WorkPhase::Idle
        };
        // 交互挂起优先级最高：即使流式进行中，用户必须先响应弹窗。
        if gate {
            state.phase = WorkPhase::WaitingUser;
        }
        state.model = model;
        state.context_tokens = context_tokens;
        state.context_limit = context_limit;
        // cwd 显示源 = session.list 投影（后端 meta.cwd 持久数据源），不依赖
        // 前端内存态 → daemon 重启后随 sessions 刷新自动回填，永不显示空
        // （修「重启后前端回空 cwd 导致后端逻辑错乱」的显示侧根因）。
        state.cwd = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|s| s.seed == active)
            .and_then(|s| s.cwd.clone());
        // Mode and feedback are UI-local; permission comes from config.load.
        state.mode = self
            .composer_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let tool_mode = self
            .composer_tool_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        state.tool_mode = tool_mode.mode;
        state.tool_mode_custom_tools = tool_mode.custom_tools;
        state.permission_level = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.permission_level)
            // 未加载：0（ComboBox 显示无选中）而非 1——1 会误导显示 L1。
            .unwrap_or(0);
        let fb = self
            .composer_feedback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&active)
            .cloned()
            .unwrap_or_default();
        state.submit_error = fb.submit_error;
        state.send_ack = fb.send_ack;
        // 子代理胶囊（工作状态区第二行）+ 区域模式（goal 预留）。
        let subagents = self.subagent_items(&active, now);
        state.subagents = subagents;
        state.status_zone = if state.subagents.is_empty() {
            "idle".into()
        } else {
            "agent".into()
        };
        (state, rev)
    }

    // ── 原生 ChatView（timeline 单源，Phase 2）──────────────────────

    /// Drain at most `limit` timeline live entries for the active session.
    /// Events from other sessions and active overflow stay queued in FIFO
    /// order for a later frame. `limit == 0` is an O(1) no-op.
    ///
    /// 返回的 entries 保持 `timeline_seq` 单调序（writer 单写保证）。
    pub(crate) fn timeline_drain_limit(
        &self,
        limit: usize,
    ) -> (Vec<qaqh_client::TimelineEntry>, u64) {
        let rev = self.timeline_rev.load(Ordering::Relaxed);
        if limit == 0 {
            return (Vec::new(), rev);
        }
        let active = self.active_seed();
        let mut queue = self
            .timeline_events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entries = queue.drain_seed(&active, limit);
        (entries, rev)
    }

    /// 三态消费 timeline 快照（BUG-003：peek+clone+consume → take 零拷贝）：
    /// - Matched：cached seed == `seed` → move 出快照；
    /// - Stale：cached seed != `seed` → **保留**缓存并返回 None，
    ///   调用方应主动重拉 active seed 快照（avoid take 即弃后快照
    ///   永久丢失、ChatView 停在"加载会话…"）；
    /// - Empty：无缓存 → None。
    pub(crate) fn chat_timeline_take(&self, seed: &str) -> Option<TimelineSnapshot> {
        let mut slot = self.chat_timeline.lock().unwrap_or_else(|e| e.into_inner());
        let matched = slot
            .as_ref()
            .is_some_and(|(cached_seed, _)| cached_seed == seed);
        if matched {
            slot.take().map(|(_, snapshot)| snapshot)
        } else {
            None
        }
    }

    /// 分页：drain 更早回合页（`(seed, TimelineSnapshot JSON)` 队列，按
    /// active_seed 过滤——与 `chat_drain` 同隔离语义）。
    pub(crate) fn chat_prepend_drain(&self) -> Vec<(String, TimelineSnapshot)> {
        let active = self.active_seed();
        self.chat_prepend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .filter(|(seed, _)| *seed == active)
            .collect()
    }
}
