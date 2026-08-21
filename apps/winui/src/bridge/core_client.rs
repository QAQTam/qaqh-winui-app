//! BridgeCore methods: client.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use qaqh_client::{
    Channel, ChannelStatus, Client, ClientHandlers, ClientOptions, ControlEvent,
    ConversationEvent as DomainConversationEvent, EventBatch, RingingEvent, TimelinePage,
    TimelineStatus,
};

use crate::shell_store::{
    SessionDetail, UsageInfo as UiUsageInfo, activity_event, dashboard_event, session_state_event,
    skills_event,
};

use super::*;

impl super::BridgeCore {
    /// 连接主体（无 `rebuilding` 检查）。`rebuild_client` 在
    /// `rebuilding=true` 下调用本方法——若走 `ensure_client` 会自锁：
    /// 重建永远返回 "client is rebuilding" 失败，client 被 close 后无法
    /// 恢复，所有请求（config.load/session.list/attach）连接失败。
    pub(crate) async fn connect_client(&self) -> Result<Client, String> {
        if let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(client);
        }
        // 连接互斥：壳启动后首屏多个直连请求（backend.connect + 会话
        // 列表 + config.load + 侧栏刷新）几乎同时到达，若无互斥则每个调用
        // 各自 connect_async → 各自 wait_for_daemon spawn daemon（双 daemon
        // 并存触发源）。首个调用者置位并发起连接，其余轮询等待其结果。
        if self.connecting.swap(true, Ordering::AcqRel) {
            return self.wait_connect_result().await;
        }
        log_diag("connect_client: connecting...");
        // 远端档案存在时直连（跳过 discovery/spawn）；否则本地模式。
        let remote_profile = self
            .remote_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let launch_local = remote_profile.is_none();
        let remote_target = remote_profile.map(|profile| qaqh_client::RemoteEndpoint {
            base_url: profile.base_url,
            token: profile.token,
        });
        let result = Client::connect_async(ClientOptions {
            handlers: ClientHandlers {
                on_batch: Arc::new({
                    let core = self.self_arc();
                    move |batch: EventBatch| core.emit_batch(batch)
                }),
                on_status: Arc::new({
                    let core = self.self_arc();
                    move |channel: Channel, status: ChannelStatus| core.emit_status(channel, status)
                }),
                on_reset: Some(Arc::new({
                    let core = self.self_arc();
                    move |reset: qaqh_client::ResetRequired| core.handle_reset(reset)
                })),
                on_timeline_entry: Arc::new({
                    let core = self.self_arc();
                    move |seed: String, entry: qaqh_client::TimelineEntry| {
                        // Phase 2：timeline live 事件入队 → BlockTranscript
                        // 单源渲染（delta 可丢，结构性强制入队）。
                        let is_delta = is_timeline_delta(&entry);
                        let pushed = core
                            .timeline_events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(seed, entry, is_delta);
                        if pushed {
                            core.timeline_rev.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }),
                on_timeline_status: Arc::new({
                    let core = self.self_arc();
                    move |status: TimelineStatus| {
                        // A 方案：缓存状态供失联检测（timeline 流死循环判据）。
                        // WebView 移除：不再 emit timeline.status。
                        *core
                            .timeline_status
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(status);
                    }
                }),
                on_timeline_snapshot: Arc::new({
                    let core = self.self_arc();
                    move |snapshot: TimelinePage| {
                        // 原生 ChatView：缓存权威 turns 历史（resume 数据源）。
                        // seed 标记与层级解包见 `cache_timeline_snapshot`——
                        // 从快照 body 顶层读权威 seed，缓存 `snapshot` 子对象。
                        core.cache_timeline_snapshot(snapshot);
                        // WebView 移除：不再 emit timeline.snapshot（原生 ChatView
                        // 从 chat_timeline 缓存消费）。
                    }
                }),
            },
            launch_daemon_if_missing: launch_local,
            remote: remote_target,
            ..Default::default()
        })
        .await;
        // 无论成败都先复位互斥位，等待者据此退出/复用结果。
        self.connecting.store(false, Ordering::Release);
        let client = result.map_err(|e| {
            log_diag(&format!("connect_client connect failed: {e}"));
            e.to_string()
        })?;
        *self.client.lock().unwrap_or_else(|e| e.into_inner()) = Some(client.clone());
        // WebView 移除：不再 emit backend.status（连接状态由壳本地持有）。
        Ok(client)
    }

    /// 等待并发连接发起者完成：成功 → 复用其 client；失败/超时 → 返回错误
    /// （调用方各自的重试路径——auto-reconnect 冷却 5s 起——负责恢复）。
    pub(crate) async fn wait_connect_result(&self) -> Result<Client, String> {
        let deadline = Instant::now() + CONNECT_WAIT_TIMEOUT;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Some(client) = self
                .client
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return Ok(client);
            }
            if !self.connecting.load(Ordering::Acquire) {
                // 发起者已结束且失败：直接失败，避免每个等待者再各发起一次。
                return Err("backend connect failed (concurrent attempt)".into());
            }
            if Instant::now() >= deadline {
                return Err("backend connect in progress timed out".into());
            }
        }
    }

    /// `ringing.reset_required`: re-bootstrap the affected session and push a
    /// fresh snapshot to the shell (mirrors browserBridge `handleReset`).
    pub(crate) fn handle_reset(&self, reset: qaqh_client::ResetRequired) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("reset: reconnect failed: {err}"));
                    return;
                }
            };
            match client.bootstrap(&reset.seed).await {
                Ok(snapshot) => core
                    .apply_bootstrap_conversation_state(&reset.seed, &snapshot.conversation.state),
                Err(err) => log_diag(&format!("reset: bootstrap {} failed: {err}", reset.seed)),
            }
        });
    }

    pub(crate) fn emit_batch(&self, batch: EventBatch) {
        // XAML 侧栏实时活动状态：control 频道 `session_activity_changed`
        // 增量更新缓存（不触发全量 refresh）。
        if batch.channel == Channel::Control {
            let mut changed = false;
            let mut skills_changed = false;
            let mut list_changed = false;
            for env in &batch.envelopes {
                let RingingEvent::Control(event) = &env.event else {
                    continue;
                };
                if let Some((seed, state)) = activity_event(event) {
                    self.activities
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(seed.clone(), state);
                    let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(item) = sessions.iter_mut().find(|i| i.seed == seed) {
                        item.state = state;
                    }
                    changed = true;
                }
                // XAML 技能页：skills_updated 携带完整 SkillsStatus 载荷，
                // 直接缓存为权威快照（含 seed，batch.seed 兜底）。
                if let Some(mut snap) = skills_event(event) {
                    if snap.seed.is_empty() {
                        snap.seed = batch.seed.clone();
                    }
                    self.skills
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(snap);
                    skills_changed = true;
                }
                // 会话生命周期变更（created/archived/unarchived/deleted）：
                // 归档/删除/新建不再依赖 500ms 轮询，事件到达即全量刷新。
                // （发起方命令成功后的主动 refresh 保留，作为快速路径。）
                if session_state_event(event).is_some() {
                    list_changed = true;
                }
                // 会话标题生成/更新（首 turn 后）：事件到达即重拉 session.list，
                // 三处显示（标题栏/标签页/侧栏）同步刷新——不再依赖重启。
                if matches!(event, ControlEvent::SessionMetaChanged { .. }) {
                    list_changed = true;
                }
                // XAML composer goalBar：dashboard_snapshot 携带完整
                // DashboardSnapshot 载荷（tasks/recent_edits/current_todo_id），
                // 直接缓存为权威快照（终局架构：Web 移除后 XAML 直消费）。
                if let Some(mut snap) = dashboard_event(event) {
                    if snap.seed.is_empty() {
                        snap.seed = batch.seed.clone();
                    }
                    self.apply_dashboard(snap);
                }
                if let ControlEvent::OperationFailed { error, .. } = event
                    && error.code == "compact_failed"
                {
                    let mut feedback = self
                        .composer_feedback
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    feedback.entry(batch.seed.clone()).or_default().submit_error =
                        error.message.clone();
                    drop(feedback);
                    self.composer_rev.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(ev) = interaction_event(event) {
                    self.apply_interaction_event(&batch.seed, ev);
                }
                // 子代理终态推送：注入被回合 lap 边界吸收（无独立注入回合）
                // 时，TurnStarted 收敛信号缺失——后端补发 `SubagentStatus`
                // 控制面事件，此处直接收敛 tracker，避免幽灵 Lost 假阴性。
                if let ControlEvent::SubagentStatus { name, state, .. } = event
                    && let Some(sub_state) = subagent_state_from_tag(state)
                {
                    self.converge_subagent(&batch.seed, name, sub_state);
                }
            }
            if changed {
                self.session_rev.fetch_add(1, Ordering::Relaxed);
                // Info 面板打开过（缓存存在）→ 活动状态变化（回合边界信号）
                // 时顺手刷新用量（低频触发，bootstrap 一次成本可接受）。
                let info_active = self
                    .info
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some();
                if info_active {
                    self.spawn_refresh_info(self.active_seed());
                }
            }
            if skills_changed {
                self.skills_rev.fetch_add(1, Ordering::Relaxed);
            }
            if list_changed {
                // 异步刷新：session.list + session.activity → 投影 → rev++，
                // 侧栏/标签页 timer 比对 rev 后刷新（各视图同一数据源）。
                self.spawn_refresh_sessions();
            }
        } else if batch.channel == Channel::Tool {
            // Rust 直连交互队列（读路径直连）：tool 频道权限请求
            // （permission 优先于 ask/plan，对齐 Web pendingInteractions 组装）。
            for env in &batch.envelopes {
                let RingingEvent::Tool(event) = &env.event else {
                    continue;
                };
                if let Some(ev) = tool_permission_event(event) {
                    self.apply_tool_permission_event(&batch.seed, ev);
                    // 权限请求挂起：状态栏切到"等待用户"。
                    self.set_composer_phase(&batch.seed, WorkPhase::WaitingUser);
                }
                // 工作状态：工具执行生命周期（tool_started 带工具名）。
                match event {
                    qaqh_client::ToolEvent::ToolCallPrepared {
                        name,
                        tool_call_id,
                        args_so_far,
                        ..
                    } if name == "spawn_subagent" => {
                        // 流式预览即登记：agent_name 仅此处可解析。
                        self.upsert_subagent(&batch.seed, tool_call_id, Some(args_so_far));
                    }
                    qaqh_client::ToolEvent::ToolStarted {
                        name, tool_call_id, ..
                    } => {
                        self.set_composer_phase(&batch.seed, WorkPhase::Tool(name.clone()));
                        if name == "spawn_subagent" {
                            // Started 无参数：仅确保实例存在（Prepared 已登记）。
                            self.upsert_subagent(&batch.seed, tool_call_id, None);
                        }
                    }
                    qaqh_client::ToolEvent::ToolFinished {
                        tool_call_id,
                        result,
                        ..
                    } => {
                        // 工具完成 → 回到生成（后续 thinking/answering delta 会覆盖）。
                        self.set_composer_phase(&batch.seed, WorkPhase::Thinking);
                        // spawn_subagent 的 Finished 仅确认 spawn 成功，子代理仍在
                        // 后台运行：不销毁 tracker 条目，终态由 [SUBAGENT ...]
                        // 注入 tag 收敛（ToolFinished ≠ 子代理完成）。
                        // 解析 spawn 返回的 seed 存入 tracker：子代理面板的数据源
                        // （形状探测：json_ok 含 process_id + seed 字段才认）。
                        if let Some(seed) = parse_spawn_seed(&result.model.text) {
                            self.record_subagent_seed(&batch.seed, &tool_call_id, &seed);
                        }
                    }
                    _ => {}
                }
            }
            // Phase 2：Tool 频道 transcript 事件退役——工具块由 timeline
            // `ToolUpdated`/`ToolProgress` 单源驱动（BlockTranscript），
            // 此处仅保留工作状态（composer phase）等遥测消费。
        } else if batch.channel == Channel::Conversation {
            // Rust 直连 composer（读路径直连）：conversation 事件活动追踪
            // ——isStreaming（卡死检测）+ usage_updated 缓存（model/context）。
            // 无条件挂载：streaming 信号同时驱动标题栏 undo/compact disabled
            // （对齐 Web `streaming()` 判定）。事件高频（流式 delta），处理
            // 为 O(1) 时间戳写入，rev 每 batch 递增一次（XAML 250ms 轮询
            // 稀释，无害）。
            let now = unix_ms();
            let mut turn_boundary = false;
            let mut turn_ended = false;
            let mut compact_status_update: Option<String> = None;
            let mut live_usage: Option<(UiUsageInfo, u64, String)> = None;
            {
                let mut map = self
                    .composer_activity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let activity = map.entry(batch.seed.clone()).or_default();
                for env in &batch.envelopes {
                    let RingingEvent::Conversation(event) = &env.event else {
                        continue;
                    };
                    if let Some(ev) = conversation_activity_event(event) {
                        if matches!(event, DomainConversationEvent::TurnStarted { .. }) {
                            // A new session activates timeline before its first
                            // turn, so that snapshot contains zero turns. Keep
                            // Undo enabled after the live turn without waiting
                            // for another full timeline snapshot. max(1) is
                            // replay-idempotent; exact count is restored by the
                            // next authoritative snapshot.
                            self.record_live_turn_started(&batch.seed);
                            // 子代理注入收敛：`[SUBAGENT 'name' STATE]` 回合
                            // 是子代理终态的唯一可靠信号（ToolFinished 仅
                            // spawn 确认）。
                            if let DomainConversationEvent::TurnStarted { user_text, .. } = event
                                && let Some((name, state)) = parse_subagent_injection(user_text)
                            {
                                self.converge_subagent(&batch.seed, &name, state);
                            }
                        }
                        if matches!(
                            &ev,
                            ConversationActivityEvent::Started | ConversationActivityEvent::Ended
                        ) {
                            turn_boundary = true;
                            turn_ended |= matches!(&ev, ConversationActivityEvent::Ended);
                            // 撤销直发：turn 事件带 turn_id，缓存最近回合。
                            let tid = match event {
                                DomainConversationEvent::TurnStarted { turn_id, .. }
                                | DomainConversationEvent::TurnCompleted { turn_id, .. }
                                | DomainConversationEvent::TurnFailed { turn_id, .. } => {
                                    Some(turn_id.as_str())
                                }
                                DomainConversationEvent::ConversationCancelled { turn_id } => {
                                    turn_id.as_deref()
                                }
                                _ => None,
                            };
                            if let Some(tid) = tid {
                                self.last_turn_ids
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(batch.seed.clone(), tid.to_string());
                            }
                        }
                        activity.apply(ev, now);
                    }
                    if turn_ended {
                        // 桌面通知：回合完成预览（Phase 1；仅活动会话）。
                        if matches!(event, DomainConversationEvent::TurnCompleted { .. }) {
                            self.maybe_notify_turn_completed(&batch.seed);
                        }
                    }
                    match event {
                        DomainConversationEvent::UsageUpdated {
                            usage,
                            context_limit,
                            model,
                            ..
                        } => {
                            live_usage = Some((
                                UiUsageInfo {
                                    prompt_tokens: u64::from(usage.prompt_tokens),
                                    completion_tokens: u64::from(usage.completion_tokens),
                                    reasoning_tokens: u64::from(usage.reasoning_tokens),
                                    total_tokens: u64::from(usage.total_tokens),
                                    prompt_cache_hit_tokens: u64::from(
                                        usage.prompt_cache_hit_tokens,
                                    ),
                                    prompt_cache_miss_tokens: u64::from(
                                        usage.prompt_cache_miss_tokens,
                                    ),
                                    cache_usage_reported: usage
                                        .cache_usage_reported
                                        .unwrap_or(false),
                                },
                                u64::from(*context_limit),
                                model.clone(),
                            ));
                        }
                        DomainConversationEvent::CompactStarted { .. } => {
                            compact_status_update = Some("running".into());
                            let mut feedback = self
                                .composer_feedback
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            feedback
                                .entry(batch.seed.clone())
                                .or_default()
                                .submit_error
                                .clear();
                        }
                        DomainConversationEvent::CompactFinished { status, .. } => {
                            compact_status_update = Some(
                                serde_json::to_value(status)
                                    .ok()
                                    .and_then(|value| value.as_str().map(str::to_string))
                                    .unwrap_or_else(|| "failed".into()),
                            )
                        }
                        _ => {}
                    }
                }
            }
            if let Some(status) = compact_status_update.as_ref() {
                self.compact_statuses
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(batch.seed.clone(), status.clone());
            }
            let is_active_seed = batch.seed == self.active_seed();
            if is_active_seed {
                if let Some((usage, context_limit, model)) = live_usage {
                    let mut info = self.info.lock().unwrap_or_else(|e| e.into_inner());
                    let detail = info.get_or_insert_with(SessionDetail::default);
                    detail.usage = usage;
                    detail.context_limit = context_limit;
                    detail.model = model;
                    drop(info);
                    self.info_rev.fetch_add(1, Ordering::Relaxed);
                }
            }
            if compact_status_update.is_some() && is_active_seed {
                self.refresh_header();
            }
            self.composer_rev.fetch_add(1, Ordering::Relaxed);
            // 标题栏直连：turn 边界（streaming 翻转）刷新 undo/compact disabled。
            if turn_boundary {
                self.refresh_header();
            }
            // Totals remain authoritative in persisted bootstrap state. Refresh
            // once at the request boundary; live UsageUpdated above supplies the
            // current request without polling.
            if is_active_seed && turn_ended {
                let info_open = self
                    .header_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .info_open;
                if info_open {
                    self.spawn_refresh_info(batch.seed.clone());
                }
            }
            // Phase 2：conversation 频道 transcript 事件退役——turn/round/
            // delta 由 timeline 单源驱动（BlockTranscript）；此处保留的
            // activity/usage/compact 消费属遥测与控制平面（composer、header）。
        }
        // WebView 移除：ringing.batch 不再转发 Web（原生直连消费上方各分支）。
    }

    pub(crate) fn emit_status(&self, channel: Channel, status: ChannelStatus) {
        self.channel_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(channel, status);
    }

    // ── A 方案：daemon 失联检测与 client 重建（WORKFLOW §7）────────────────
    //
    // 背景：daemon 重启后旧 lease（server_epoch/client_session_id）失效。
    // SSE 重连带旧 epoch 的 Last-Event-ID 被 daemon 静默按 0 处理（从头
    // 回放，ringing_http.rs parse_sse_cursor）；ringing 通道回放可恢复，但
    // timeline 客户端对回放的旧 seq 报 Protocol error（qaqh-client
    // timeline.rs L257），重连死循环——事件流永久断，表现为"后端在处理
    // 但前端 UI 不更新"。修复：检测失联后重建 client（重新 open 拿新
    // epoch），并恢复已激活 seed 的流（快照驱动，前端零改动自愈）。

    /// 失联检测（pump 每 50ms 调用；纯内存轻量检查，无锁嵌套）。
    pub(crate) fn check_daemon_health(&self) {
        if self.rebuilding.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        // 无 client（首次 connect 失败/从未建立）时自动重连：壳只在
        // 启动时发一次 backend.connect，若恰逢 daemon 初始化窗口而失败
        // （open 超时/连接拒绝），原逻辑没有任何机制再触发 connect（health
        // 仅覆盖"已建立后 stall"），页面会永久失败直到手动刷新/重启。
        // 此处以独立冷却自动重试，直到 client 建立。
        let client_missing = self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none();
        if client_missing {
            let last = self
                .last_auto_reconnect_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let reconnect_cooldown =
                auto_reconnect_cooldown_for(self.rebuild_failures.load(Ordering::Relaxed));
            if now.duration_since(*last) >= reconnect_cooldown {
                *self
                    .last_auto_reconnect_at
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = now;
                log_diag("health: no client; auto-reconnecting");
                self.rebuild_client();
            }
            return;
        }
        // 退避冷却：连续失败后指数拉长重建间隔（60s→960s 封顶），防止
        // rebuild 风暴把 daemon 连接数打爆（32 连接信号量 → 静默 drop）。
        let rebuild_cooldown = rebuild_cooldown_for(self.rebuild_failures.load(Ordering::Relaxed));
        let cooldown_ok = {
            let last = self
                .last_rebuild_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            now.duration_since(*last) >= rebuild_cooldown
        };
        if !cooldown_ok {
            return;
        }
        if self.compute_stall(now) {
            self.rebuild_client();
        }
    }

    /// 任一活跃流失联持续超阈值即视为 daemon 失联。
    pub(crate) fn compute_stall(&self, now: Instant) -> bool {
        // 1) timeline 流非 Open/Closed 持续超阈值——daemon 重启后
        //    timeline 回放 Protocol error 死循环的专属判据。
        if let Some(status) = self
            .timeline_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let healthy = matches!(
                status,
                TimelineStatus::Open { .. } | TimelineStatus::Closed { .. }
            );
            let mut since = self
                .timeline_stall_since
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if healthy {
                *since = None;
            } else if since.is_none() {
                *since = Some(now);
            } else if now.duration_since(since.unwrap()) >= STALL_THRESHOLD {
                log_diag("health: timeline stream stalled, rebuilding client");
                return true;
            }
        }

        // 2) ringing 三通道无一 Open 持续超阈值——daemon 完全不可达场景。
        let statuses = self
            .channel_status
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let any_open = statuses
            .values()
            .any(|status| matches!(status, ChannelStatus::Open { .. }));
        let any_tracked = !statuses.is_empty();
        let mut since = self
            .channels_stall_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if any_open || !any_tracked {
            *since = None;
        } else if since.is_none() {
            *since = Some(now);
        } else if now.duration_since(since.unwrap()) >= STALL_THRESHOLD {
            log_diag("health: all ringing channels stalled, rebuilding client");
            return true;
        }

        false
    }

    /// 重建 client：停旧（close）→ 重新 open（新 epoch）→ 恢复已激活的流。
    pub(crate) fn rebuild_client(&self) {
        self.rebuilding.store(true, Ordering::Relaxed);
        *self
            .last_rebuild_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Instant::now();
        log_diag("health: rebuilding client (daemon stall detected)");
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            // 1) 停旧 client 及其全部任务（renewal + 3 通道 + timeline 流）。
            // Per-seed compact state belongs to one daemon epoch. Clear it before
            // reconnect; authoritative bootstrap repopulates each restored seed.
            core.compact_statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            core.refresh_header();
            let old = core.client.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(client) = old {
                client.close();
                log_diag("health: closed stale client");
            }
            // 2) 重新协商（新 server_epoch + client_session_id；
            //    launch_daemon_if_missing 兜底拉起 daemon）。用内部
            //    connect_client：此时 rebuilding=true，走 ensure_client
            //    会自锁失败（历史 bug：A 方案重建从未成功）。
            match core.connect_client().await {
                Ok(_) => {
                    log_diag("health: reconnected with fresh session");
                    core.rebuild_failures.store(0, Ordering::Relaxed);
                }
                Err(err) => {
                    log_diag(&format!("health: reconnect failed: {err}"));
                    core.rebuild_failures.fetch_add(1, Ordering::Relaxed);
                    core.rebuilding.store(false, Ordering::Relaxed);
                    core.reset_stall_timers();
                    return;
                }
            }
            // 3) 恢复已 attach 的 seed（XAML 侧栏）+ Web 最近激活的 seed。
            let seeds: Vec<String> = {
                let mut set = core
                    .attached
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let tseed = core
                    .last_timeline_seed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if !tseed.is_empty() {
                    set.insert(tseed);
                }
                set.into_iter().collect()
            };
            for seed in &seeds {
                core.restore_seed(seed).await;
            }
            // 4) 状态复位（WebView 移除：不再 emit backend.status）。
            core.rebuilding.store(false, Ordering::Relaxed);
            core.reset_stall_timers();
            core.spawn_refresh_sessions();
            log_diag("health: rebuild complete");
        });
    }

    pub(crate) fn reset_stall_timers(&self) {
        *self
            .timeline_stall_since
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .channels_stall_since
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .timeline_status
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 恢复单个 seed：attach（session_resume 语义）→ 每通道 bootstrap 快照
    /// → timeline 流（快照 watermark 续传）。前端 ringingMonitor /
    /// timelineMonitor 收到快照后全量重建；SSE 回放由 applied event_id
    /// 去重（壳侧 ringingStore 历史 L868），无重复应用。
    pub(crate) async fn restore_seed(&self, seed: &str) {
        let client = match self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            Some(c) => c,
            None => {
                log_diag(&format!("health: restore {seed}: no client"));
                return;
            }
        };
        if let Err(err) = client.attach(seed).await {
            log_diag(&format!("health: attach {seed} failed: {err}"));
            return;
        }
        match client.bootstrap(seed).await {
            Ok(snapshot) => {
                self.apply_bootstrap_conversation_state(seed, &snapshot.conversation.state)
            }
            Err(err) => log_diag(&format!("health: bootstrap {seed} failed: {err}")),
        }
        match client.activate_timeline(seed).await {
            // WebView 移除：timeline 快照经 on_timeline_snapshot 缓存直连。
            Ok(_snapshot) => {}
            Err(err) => log_diag(&format!("health: timeline activate {seed} failed: {err}")),
        }
    }
}
