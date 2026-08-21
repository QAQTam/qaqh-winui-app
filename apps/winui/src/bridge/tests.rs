//! Bridge unit tests.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use qaqh_client::{Channel, ChannelStatus, TimelineStatus};
use serde_json::{Value, json};

use crate::shell_store::{ActivityState, DashboardSnapshot};

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_subagent_injection_matches_all_terminal_tags() {
        assert_eq!(
            parse_subagent_injection("[SUBAGENT 'explore' COMPLETED]\n\nanswer"),
            Some(("explore".to_string(), SubagentState::Done))
        );
        assert_eq!(
            parse_subagent_injection("[SUBAGENT 'x' ERROR exit=1]"),
            Some(("x".to_string(), SubagentState::Error))
        );
        assert_eq!(
            parse_subagent_injection("[SUBAGENT 'x' TIMEOUT after 120s]"),
            Some(("x".to_string(), SubagentState::Timeout))
        );
        assert_eq!(
            parse_subagent_injection("[SUBAGENT 'x' CANCELLED]"),
            Some(("x".to_string(), SubagentState::Cancelled))
        );
        // 非注入文本 / 运行中标签不匹配。
        assert_eq!(parse_subagent_injection("normal user message"), None);
        assert_eq!(parse_subagent_injection("[SUBAGENT 'x' RUNNING]"), None);
    }

    #[test]
    fn parse_agent_name_reads_agent_name_field() {
        assert_eq!(
            parse_agent_name(
                r#"{"task_description":"t","agent_name":"explore_task","timeout_secs":120}"#
            ),
            Some("explore_task".to_string())
        );
        // 流式截断 / 非法 JSON / 空 → None（回退短哈希标识）。
        assert_eq!(
            parse_agent_name(r#"{"task_description":"t","agent_nam"#),
            None
        );
        assert_eq!(parse_agent_name(""), None);
    }

    #[test]
    fn subagent_tracker_lifecycle_converges_on_injection_tag() {
        let core = test_core();
        // ToolCallPrepared 解析 agent_name → ToolStarted 补时间戳。
        core.upsert_subagent(
            "seed-a",
            "call-1",
            Some(r#"{"agent_name":"explore_a","task_description":"t"}"#),
        );
        core.upsert_subagent("seed-a", "call-1", None);
        let items = core.subagent_items("seed-a", 1_000);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "explore_a");
        assert_eq!(items[0].state, SubagentState::Working);

        // 注入 tag 收敛（只收敛同名 Working/Lost 项）。
        core.converge_subagent("seed-a", "explore_a", SubagentState::Done);
        let items = core.subagent_items("seed-a", 2_000);
        assert_eq!(items[0].state, SubagentState::Done);
        assert!(items[0].finished_at > 0, "finished_at set by real clock");

        // 幽灵：Working 超过窗口 → Lost（真实时钟无法在单测中模拟，
        // 直接拨回 started_at 到窗口外）。
        core.upsert_subagent("seed-a", "call-2", Some(r#"{"agent_name":"ghost_b"}"#));
        {
            let mut map = core.subagent_tracker.lock().unwrap();
            map.get_mut("seed-a")
                .unwrap()
                .get_mut("call-2")
                .unwrap()
                .started_at = 1_000;
        }
        let items = core.subagent_items("seed-a", 1_000 + SUBAGENT_GHOST_TIMEOUT_MS + 1);
        let ghost = items
            .iter()
            .find(|i| i.name == "ghost_b")
            .expect("ghost item");
        assert_eq!(ghost.state, SubagentState::Lost);
        // 迟到注入仍覆盖 Lost。
        core.converge_subagent("seed-a", "ghost_b", SubagentState::Done);
        let items = core.subagent_items("seed-a", 1_000 + SUBAGENT_GHOST_TIMEOUT_MS + 2);
        let ghost = items
            .iter()
            .find(|i| i.name == "ghost_b")
            .expect("ghost item");
        assert_eq!(ghost.state, SubagentState::Done);
    }

    fn test_core() -> BridgeCore {
        BridgeCore {
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
            current_view: Mutex::new(String::new()),
            remote_profile: Mutex::new(None),
            remote_fs_listing: Mutex::new(RemoteFsListing::default()),
            remote_fs_rev: AtomicU64::new(0),
            remote_fs_preview: Mutex::new(None),
            remote_fs_preview_rev: AtomicU64::new(0),
            settings: Mutex::new(None),
            settings_rev: AtomicU64::new(0),
            settings_proj: Mutex::new(SettingsProjection::default()),
            settings_proj_rev: AtomicU64::new(0),
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
            timeline_events: Mutex::new(TimelineEventQueues::default()),
            timeline_rev: AtomicU64::new(0),
            resume_generation: AtomicU64::new(0),
            chat_timeline: Mutex::new(None),
            subagent_timeline: Mutex::new(None),
            subagent_timeline_fetching: Mutex::new(HashSet::new()),
            timeline_has_more: Mutex::new(std::collections::HashMap::new()),
            chat_prepend: Mutex::new(std::collections::VecDeque::new()),
            timeline_fetching: Mutex::new(std::collections::HashSet::new()),
            timeline_refresh_at: Mutex::new(Instant::now() - Duration::from_secs(3600)),
            dashboards: Mutex::new(HashMap::new()),
            notifier: Mutex::new(None),
            notif_enabled: AtomicBool::new(true),
            dashboard_rev: AtomicU64::new(0),
        }
    }

    fn reconnecting() -> TimelineStatus {
        TimelineStatus::Reconnecting {
            seed: "s1".into(),
            retry_ms: 1000,
            cursor: 3,
        }
    }

    // ── Timeline 事件队列（seed 隔离；Phase 2 单源）─────────────────

    fn timeline_entry(seq: u64, turn_id: &str) -> qaqh_client::TimelineEntry {
        qaqh_client::TimelineEntry {
            timeline_seq: seq,
            turn_id: turn_id.to_string(),
            round_num: Some(0),
            event: qaqh_client::TimelineEvent::TextDelta {
                block_id: "text:b1".into(),
                fragment_seq: seq,
                delta: "x".into(),
            },
        }
    }

    /// timeline_drain_limit 只返回 active_seed 的事件：后台会话增量不污染
    /// 活动会话的 BlockTranscript（切换瞬间残留事件同样被丢弃）。
    #[test]
    fn timeline_drain_filters_by_active_seed() {
        let core = test_core();
        core.set_active_seed("sA");
        {
            let mut q = core.timeline_events.lock().unwrap();
            assert!(q.push("sA".into(), timeline_entry(1, "t1"), true));
            assert!(q.push("sB".into(), timeline_entry(2, "t2"), true));
            assert!(q.push("sA".into(), timeline_entry(3, "t3"), true));
        }
        let (events, _) = core.timeline_drain_limit(usize::MAX);
        assert_eq!(events.len(), 2, "只返回活动会话 sA 的事件");
        assert!(events.iter().all(|e| e.turn_id != "t2"));

        // 切换后：sA 的残留事件不再泄漏到 sB（sB 自有 b1 正常返回）。
        core.set_active_seed("sB");
        assert!(core.timeline_events.lock().unwrap().push(
            "sA".into(),
            timeline_entry(4, "t4"),
            true
        ));
        let (events, _) = core.timeline_drain_limit(usize::MAX);
        assert_eq!(events.len(), 1, "只返回 sB 自身事件");
        assert_eq!(events[0].turn_id, "t2", "sA 残留事件不泄漏");
        // 有界 drain 保留其它 seed（防后台积压被误清）；切回 sA 可消费。
        assert_eq!(
            core.timeline_events
                .lock()
                .unwrap()
                .drain_seed("sA", usize::MAX)[0]
                .turn_id,
            "t4",
            "sA 残留保留待切回"
        );
    }

    #[test]
    fn timeline_drain_limit_preserves_other_seeds_and_active_remainder() {
        let core = test_core();
        core.set_active_seed("sA");
        {
            let mut q = core.timeline_events.lock().unwrap();
            assert!(q.push("sA".into(), timeline_entry(1, "a1"), true));
            assert!(q.push("sB".into(), timeline_entry(2, "b1"), true));
            assert!(q.push("sA".into(), timeline_entry(3, "a2"), true));
        }

        let (none, _) = core.timeline_drain_limit(0);
        assert!(none.is_empty());
        assert_eq!(
            core.timeline_events.lock().unwrap().is_empty(),
            false,
            "limit=0 must preserve the queue"
        );

        let (first, _) = core.timeline_drain_limit(1);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].turn_id, "a1");

        core.set_active_seed("sB");
        let (background, _) = core.timeline_drain_limit(1);
        assert_eq!(background[0].turn_id, "b1");

        core.set_active_seed("sA");
        let (remainder, _) = core.timeline_drain_limit(1);
        assert_eq!(remainder[0].turn_id, "a2");
    }

    #[test]
    fn timeline_queue_eviction_prefers_delta_over_structure() {
        let structural = qaqh_client::TimelineEntry {
            timeline_seq: 1,
            turn_id: "t1".into(),
            round_num: None,
            event: qaqh_client::TimelineEvent::TurnOpened {
                user_text: "hi".into(),
            },
        };
        let delta = timeline_entry(2, "t1");
        let mut queues = TimelineEventQueues::default();
        assert!(queues.push("sA".into(), structural, false));
        assert!(queues.push("sA".into(), delta, true));
        assert!(queues.evict_one("sA"));

        let remaining = queues.drain_seed("sA", usize::MAX);
        assert_eq!(remaining.len(), 1);
        assert!(matches!(
            remaining[0].event,
            qaqh_client::TimelineEvent::TurnOpened { .. }
        ));
    }

    // ── 交互队列状态机（Rust 直连读路径）──────────────────────────────

    /// 真实 daemon control 事件（qaqh-domain `ControlEvent` snake_case）。
    fn ask_requested_event(id: &str, turn_id: &str) -> Value {
        json!({
            "type": "interaction_requested",
            "interaction_id": id,
            "turn_id": turn_id,
            "mode": "single",
            "questions": [
                { "id": "q1", "question": "继续？", "options": ["是", "否"], "allow_custom": true }
            ],
        })
    }

    #[test]
    fn parses_ask_requested_with_snake_case_keys() {
        let ev = parse_interaction_event(&ask_requested_event("i1", "t1")).expect("parse");
        let InteractionEvent::AskRequested { id, questions } = ev else {
            panic!("expected AskRequested");
        };
        assert_eq!(id, "i1");
        assert_eq!(questions.len(), 1);
        // daemon snake_case `allow_custom` → 壳 camelCase 形状。
        assert_eq!(questions[0].allow_custom, true);
        assert_eq!(questions[0].options, vec!["是", "否"]);
    }

    #[test]
    fn parses_plan_review_requested_with_nullable_todo() {
        let ev = parse_interaction_event(&json!({
            "type": "plan_review_requested",
            "interaction_id": "p1",
            "turn_id": "t2",
            "plan_content": "1. 修 bug",
            "review_type": "todo_activation",
            "todo_items": [
                { "id": "td1", "title": "修 bug", "description": "", "complexity": "small" }
            ],
        }))
        .expect("parse");
        let InteractionEvent::PlanRequested {
            id,
            plan_content,
            review_type,
            todo_items,
        } = ev
        else {
            panic!("expected PlanRequested");
        };
        assert_eq!(id, "p1");
        assert_eq!(plan_content, "1. 修 bug");
        assert_eq!(review_type, "todo_activation");
        assert_eq!(todo_items.len(), 1);
        assert_eq!(todo_items[0].complexity, "small");
        // todo_items 可为 null → 空 Vec。
        let null_ev = parse_interaction_event(&json!({
            "type": "plan_review_requested",
            "interaction_id": "p2",
            "turn_id": "t3",
            "plan_content": "x",
            "review_type": "",
            "todo_items": null,
        }))
        .expect("parse");
        let InteractionEvent::PlanRequested { todo_items, .. } = null_ev else {
            panic!("expected PlanRequested");
        };
        assert!(todo_items.is_empty());
    }

    #[test]
    fn ghost_cleanup_only_for_rejection_codes() {
        let ev = parse_interaction_event(&json!({
            "type": "operation_failed",
            "occurrence_id": "o1",
            "scope": "session",
            "error": { "code": "ask_rejected", "error_id": "e1", "message": "rejected" },
        }))
        .expect("parse");
        assert!(matches!(ev, InteractionEvent::GhostCleanup));
        // 其他错误码不触发自愈。
        let ev2 = parse_interaction_event(&json!({
            "type": "operation_failed",
            "occurrence_id": "o2",
            "scope": "session",
            "error": { "code": "tool_failed", "error_id": "e2", "message": "boom" },
        }));
        assert!(ev2.is_none());
    }

    #[test]
    fn parses_tool_permission_requested_and_finished() {
        let ev = parse_tool_permission_event(&json!({
            "type": "tool_permission_requested",
            "tool_call_id": "tc1",
            "turn_id": "t9",
            "round_num": 1,
            "tool_name": "shell",
            "reason": "run cmd",
            "paths": ["C:/x"],
            "category": "exec",
            "level": 2,
            "risk": "high",
            "consequence": "执行命令",
        }))
        .expect("parse");
        let ToolPermissionEvent::Requested {
            tool_call_id,
            paths,
            level,
            risk,
            ..
        } = ev
        else {
            panic!("expected Requested");
        };
        assert_eq!(tool_call_id, "tc1");
        assert_eq!(paths, vec!["C:/x"]);
        assert_eq!(level, 2);
        assert_eq!(risk, "high");

        let done = parse_tool_permission_event(&json!({
            "type": "tool_finished",
            "tool_call_id": "tc1",
            "turn_id": "t9",
            "round_num": 1,
            "result": { "exit_code": 0 },
        }))
        .expect("parse");
        assert!(matches!(done, ToolPermissionEvent::Resolved { .. }));
    }

    #[test]
    fn machine_permission_priority_and_resolution() {
        let mut m = InteractionMachine::default();
        // permission 请求先到。
        m.apply_tool(
            parse_tool_permission_event(&json!({
                "type": "tool_permission_requested",
                "tool_call_id": "tc1",
                "turn_id": "t9",
                "round_num": 1,
                "tool_name": "shell",
                "reason": "run",
                "paths": [],
                "category": "exec",
                "level": 2,
                "risk": "high",
                "consequence": "执行",
            }))
            .expect("parse"),
        );
        // ask 后到——permission 仍优先（对齐 Web pendingInteractions[0]）。
        m.apply(parse_interaction_event(&ask_requested_event("i1", "t1")).expect("parse"));
        let snap = m.snapshot("seed1");
        assert_eq!(snap.kind, "permission");
        assert_eq!(snap.id, "tc1");
        assert_eq!(snap.tool_name, "shell");
        assert_eq!(snap.seed, "seed1");

        // tool_finished 释放 permission → ask 上位。
        m.apply_tool(ToolPermissionEvent::Resolved {
            tool_call_id: "tc1".into(),
        });
        let snap = m.snapshot("seed1");
        assert_eq!(snap.kind, "ask");
        assert_eq!(snap.id, "i1");
        assert_eq!(snap.questions.len(), 1);

        // interaction_resolved 清除 ask → 空（kind=""，XAML 判空关闭）。
        m.apply(InteractionEvent::AskResolved { id: "i1".into() });
        let snap = m.snapshot("seed1");
        assert!(snap.kind.is_empty());
        assert!(snap.id.is_empty());
    }

    #[test]
    fn machine_plan_flow_and_ghost_cleanup() {
        let mut m = InteractionMachine::default();
        m.apply(
            parse_interaction_event(&json!({
                "type": "plan_review_requested",
                "interaction_id": "p1",
                "turn_id": "t2",
                "plan_content": "plan",
                "review_type": "todo_activation",
                "todo_items": null,
            }))
            .expect("parse"),
        );
        let snap = m.snapshot("s");
        assert_eq!(snap.kind, "plan");
        assert_eq!(snap.plan_content, "plan");

        // 幽灵自愈：operation_failed 清除挂起面板。
        m.apply(InteractionEvent::GhostCleanup);
        assert!(m.snapshot("s").kind.is_empty());

        // 不匹配 id 的 resolved 不清除（对齐 Web reducer 的 id 匹配）。
        m.apply(
            parse_interaction_event(&json!({
                "type": "plan_review_requested",
                "interaction_id": "p2",
                "turn_id": "t4",
                "plan_content": "p2",
                "review_type": "",
            }))
            .expect("parse"),
        );
        m.apply(InteractionEvent::PlanResolved { id: "p1".into() });
        assert_eq!(m.snapshot("s").kind, "plan");
        m.apply(InteractionEvent::PlanResolved { id: "p2".into() });
        assert!(m.snapshot("s").kind.is_empty());
    }

    #[test]
    fn apply_interaction_event_is_idempotent_for_replay() {
        let core = test_core();
        core.set_active_seed("seed1");
        let ev = ask_requested_event("i1", "t1");
        core.apply_interaction_event("seed1", parse_interaction_event(&ev).expect("parse"));
        let rev1 = core.interaction_rev.load(Ordering::Relaxed);
        assert_eq!(core.interaction_snapshot().0.kind, "ask");
        // SSE 重放同一事件：快照无变化 → rev 不递增（幂等）。
        core.apply_interaction_event("seed1", parse_interaction_event(&ev).expect("parse"));
        let rev2 = core.interaction_rev.load(Ordering::Relaxed);
        assert_eq!(rev1, rev2);
    }

    #[test]
    fn compact_failure_is_visible_in_active_composer() {
        let core = test_core();
        core.set_active_seed("seedA");
        let error = qaqh_client::ControlEvent::OperationFailed {
            occurrence_id: "occ-1".into(),
            scope: qaqh_client::ErrorScope::Conversation,
            error: qaqh_client::DomainError {
                error_id: "err-1".into(),
                code: "compact_failed".into(),
                message: "provider rejected compact request".into(),
                retryable: true,
                dedupe_key: Some("compact_failed".into()),
            },
            operation_id: None,
        };
        core.emit_batch(qaqh_client::EventBatch {
            schema: "qaqh.ringing".into(),
            version: 1,
            channel: Channel::Control,
            seed: "seedA".into(),
            server_epoch: "epoch".into(),
            from_stream_seq: 1,
            to_stream_seq: 1,
            envelopes: vec![qaqh_client::RingingEventEnvelope::new(
                "seedA",
                1,
                1,
                1,
                "event-1",
                qaqh_client::RingingEvent::Control(error),
            )],
        });
        assert_eq!(
            core.composer_snapshot().0.submit_error,
            "provider rejected compact request"
        );
    }

    #[test]
    fn compact_status_cache_follows_active_seed_and_bootstrap() {
        let core = test_core();
        *core.current_view.lock().unwrap() = "chat".into();
        core.compact_statuses
            .lock()
            .unwrap()
            .insert("seedA".into(), "running".into());
        core.compact_statuses
            .lock()
            .unwrap()
            .insert("seedB".into(), "completed".into());

        core.set_active_seed("seedA");
        assert!(core.header_snapshot().0.compacting);
        core.set_active_seed("seedB");
        assert!(!core.header_snapshot().0.compacting);

        core.apply_bootstrap_conversation_state(
            "seedB",
            &serde_json::json!({"compact_status": "running"}),
        );
        assert!(core.header_snapshot().0.compacting);
        core.apply_bootstrap_conversation_state(
            "seedA",
            &serde_json::json!({"compact_status": "failed"}),
        );
        assert!(
            core.header_snapshot().0.compacting,
            "background bootstrap must not overwrite the active seed projection"
        );
    }

    #[test]
    fn interaction_cache_follows_active_seed() {
        let core = test_core();
        // 会话 A 请求 ask；active 尚未设置 → 缓存为空（后台不打扰当前显示）。
        core.apply_interaction_event(
            "seedA",
            parse_interaction_event(&ask_requested_event("iA", "tA")).expect("parse"),
        );
        assert!(core.interaction_snapshot().0.kind.is_empty());
        // 切到 A → 缓存投影 A 的交互。
        core.set_active_seed("seedA");
        assert_eq!(core.interaction_snapshot().0.kind, "ask");
        assert_eq!(core.interaction_snapshot().0.id, "iA");
        // 会话 B 的交互事件不覆盖当前显示（A 保持）。
        core.apply_interaction_event(
            "seedB",
            parse_interaction_event(&ask_requested_event("iB", "tB")).expect("parse"),
        );
        assert_eq!(core.interaction_snapshot().0.id, "iA");
        // 切到 B → B 的交互上位；切回 A → A 恢复（状态机按 seed 保留）。
        core.set_active_seed("seedB");
        assert_eq!(core.interaction_snapshot().0.id, "iB");
        core.set_active_seed("seedA");
        assert_eq!(core.interaction_snapshot().0.id, "iA");
    }

    // ── native composer state ─────────────────────────────────────────

    #[test]
    fn parses_conversation_activity_events() {
        use ConversationActivityEvent as E;
        // turn_started → Started。
        let ev = parse_conversation_activity_event(&json!({
            "type": "turn_started", "turn_id": "t1", "user_text": "hi"
        }))
        .expect("parse");
        assert!(matches!(ev, E::Started));
        // 终态 → Ended。
        for ty in ["turn_completed", "turn_failed", "conversation_cancelled"] {
            let ev = parse_conversation_activity_event(&json!({ "type": ty, "turn_id": "t1" }))
                .expect("parse");
            assert!(matches!(ev, E::Ended), "{ty}");
        }
        // usage_updated → Usage（snake_case usage 字段）。
        let ev = parse_conversation_activity_event(&json!({
            "type": "usage_updated",
            "turn_id": "t1", "round_num": 1,
            "usage": { "prompt_tokens": 1234, "total_tokens": 2000 },
            "context_limit": 200000,
            "model": "gpt-5",
        }))
        .expect("parse");
        let E::Usage {
            prompt_tokens,
            context_limit,
            model,
        } = ev
        else {
            panic!("expected Usage");
        };
        assert_eq!(prompt_tokens, 1234);
        assert_eq!(context_limit, 200000);
        assert_eq!(model, "gpt-5");
        // round_delta → Delta（阶段细分；thinking/answering 生效）。
        let ev = parse_conversation_activity_event(&json!({
            "type": "round_delta", "turn_id": "t1", "round_num": 1,
            "kind": "thinking", "delta": "x",
        }))
        .expect("parse");
        assert!(matches!(ev, E::Delta(WorkPhase::Thinking)));
        let ev = parse_conversation_activity_event(&json!({
            "type": "round_delta", "turn_id": "t1", "round_num": 1,
            "kind": "answering", "delta": "y",
        }))
        .expect("parse");
        assert!(matches!(ev, E::Delta(WorkPhase::Answering)));
        // round_delta 缺 kind → 不视为活动事件（None）。
        assert!(
            parse_conversation_activity_event(&json!({
                "type": "round_delta", "turn_id": "t1",
            }))
            .is_none()
        );
        // 其余流式事件 → Touched。
        for ty in [
            "block_checkpoint",
            "round_completed",
            "provider_retrying",
            "provider_tool_status",
        ] {
            let ev = parse_conversation_activity_event(&json!({ "type": ty, "turn_id": "t1" }))
                .expect("parse");
            assert!(matches!(ev, E::Touched), "{ty}");
        }
        // compact/未知 → None（不视为活动）。
        assert!(parse_conversation_activity_event(&json!({ "type": "compact_started" })).is_none());
        assert!(parse_conversation_activity_event(&json!({ "type": "bogus" })).is_none());
    }

    #[test]
    fn composer_streaming_stall_detection() {
        let mut a = ComposerActivity::default();
        // 无活动 turn → 非流式。
        assert!(!a.is_streaming(1_000));
        // turn_started → 流式（时间戳未知保守 true，随后精确）。
        a.apply(ConversationActivityEvent::Started, 1_000);
        assert!(a.is_streaming(1_000));
        // 4 分钟内 → 流式。
        assert!(a.is_streaming(1_000 + COMPOSER_STALL_TIMEOUT_MS - 1));
        // 超时 → 非流式（卡死）。
        assert!(!a.is_streaming(1_000 + COMPOSER_STALL_TIMEOUT_MS));
        // 活动事件刷新时间戳 → 恢复流式。
        a.apply(ConversationActivityEvent::Touched, 10_000);
        assert!(a.is_streaming(10_001));
        // 终态 → 非流式。
        a.apply(ConversationActivityEvent::Ended, 11_000);
        assert!(!a.is_streaming(11_000));
    }

    #[test]
    fn composer_recovers_streaming_when_start_event_was_missed() {
        let mut a = ComposerActivity::default();
        a.apply(ConversationActivityEvent::Touched, 10_000);
        assert!(a.is_streaming(10_001));
        a.apply(ConversationActivityEvent::Ended, 11_000);
        assert!(!a.is_streaming(11_000));
    }

    #[test]
    fn active_seed_switch_invalidates_composer_and_dashboard_projections() {
        let core = test_core();
        let composer_before = core.composer_rev.load(Ordering::Relaxed);
        let dashboard_before = core.dashboard_rev.load(Ordering::Relaxed);
        core.set_active_seed("seed1");
        assert!(core.composer_rev.load(Ordering::Relaxed) > composer_before);
        assert!(core.dashboard_rev.load(Ordering::Relaxed) > dashboard_before);
    }

    #[test]
    fn first_live_turn_enables_undo_without_waiting_for_a_snapshot() {
        let core = test_core();
        core.set_active_seed("seed1");
        assert!(core.header_snapshot().0.undo_disabled);
        core.record_live_turn_started("seed1");
        core.refresh_header();
        assert!(!core.header_snapshot().0.undo_disabled);
        // Replayed TurnStarted remains idempotent for the non-empty projection.
        core.record_live_turn_started("seed1");
        assert_eq!(core.header_turns.lock().unwrap().get("seed1"), Some(&1));
    }

    #[test]
    fn composer_uses_session_activity_before_first_turn_event() {
        let core = test_core();
        core.activities
            .lock()
            .unwrap()
            .insert("seed1".into(), ActivityState::Working);
        core.set_active_seed("seed1");
        assert!(core.composer_snapshot().0.is_streaming);
        core.activities
            .lock()
            .unwrap()
            .insert("seed1".into(), ActivityState::Idle);
        assert!(!core.composer_snapshot().0.is_streaming);
    }

    #[test]
    fn dashboard_cache_follows_active_seed() {
        use crate::shell_store::DashboardTask;

        let core = test_core();
        let snapshot = |seed: &str, task: &str| DashboardSnapshot {
            seed: seed.into(),
            tasks: vec![DashboardTask {
                id: format!("{seed}-todo"),
                subject: task.into(),
                description: String::new(),
                status: "completed".into(),
                evidence: Some(format!("{task} evidence")),
            }],
            recent_edits: Vec::new(),
            current_todo_id: Some(format!("{seed}-todo")),
        };
        core.apply_dashboard(snapshot("seedA", "A task"));
        core.apply_dashboard(snapshot("seedB", "B task"));

        core.set_active_seed("seedA");
        let (a, _) = core.dashboard_snapshot();
        let a = a.expect("seed A snapshot");
        assert_eq!(a.tasks[0].subject, "A task");
        assert_eq!(a.tasks[0].evidence.as_deref(), Some("A task evidence"));
        core.set_active_seed("seedB");
        let (b, _) = core.dashboard_snapshot();
        let b = b.expect("seed B snapshot");
        assert_eq!(b.tasks[0].subject, "B task");
        assert_eq!(b.tasks[0].evidence.as_deref(), Some("B task evidence"));
        core.set_active_seed("seedC");
        assert!(core.dashboard_snapshot().0.is_none());
    }

    #[test]
    fn composer_snapshot_uses_typed_activity_and_local_state() {
        let core = test_core();
        core.set_active_seed("seed1");
        // Canonical conversation events drive activity and usage.
        let now = unix_ms();
        {
            let mut map = core.composer_activity.lock().unwrap();
            let a = map.entry("seed1".into()).or_default();
            a.apply(ConversationActivityEvent::Started, now);
            a.apply(
                ConversationActivityEvent::Usage {
                    prompt_tokens: 42,
                    context_limit: 200_000,
                    model: "gpt-5".into(),
                },
                now,
            );
        }
        let (s, _) = core.composer_snapshot();
        assert!(s.is_streaming);
        assert_eq!(s.model, "gpt-5");
        assert_eq!(s.context_tokens, 42);
        assert_eq!(s.context_limit, 200_000);
        assert_eq!(s.seed, "seed1");
        // UI-local state owns mode and send feedback; config owns permission.
        // 默认与后端 AGENT_MODE=0（Normal，不拦截 PLAN_BLOCKED）对齐 → 显示「执行」。
        assert_eq!(s.mode, "code");
        // 权限未配置（settings 缓存空）→ 0（"加载中"），不再误报 L1（2026-08 修复）。
        assert_eq!(s.permission_level, 0);
        assert_eq!(s.queue_count, 0);
        assert_eq!(s.send_ack, 0);
        assert_eq!(s.submit_error, "");
        *core.composer_mode.lock().unwrap() = "code".into();
        core.composer_feedback
            .lock()
            .unwrap()
            .entry("seed1".into())
            .or_default()
            .send_ack = 7;
        let (s2, _) = core.composer_snapshot();
        assert_eq!(s2.mode, "code");
        assert_eq!(s2.send_ack, 7);
        // Pending gates come from the typed interaction machine.
        assert!(!s.has_pending_gate);
        core.apply_interaction_event(
            "seed1",
            parse_interaction_event(&ask_requested_event("i1", "t1")).expect("parse"),
        );
        assert!(core.composer_snapshot().0.has_pending_gate);
    }

    #[test]
    fn timeline_stall_triggers_only_after_threshold() {
        let core = test_core();
        let now = Instant::now();
        // 首次出现非 Open 状态：开始计时，不触发。
        *core.timeline_status.lock().unwrap() = Some(reconnecting());
        assert!(!core.compute_stall(now));
        assert!(core.timeline_stall_since.lock().unwrap().is_some());
        // 未到阈值：仍不触发。
        *core.timeline_stall_since.lock().unwrap() =
            Some(now - STALL_THRESHOLD + Duration::from_secs(1));
        assert!(!core.compute_stall(now));
        // 超过阈值：触发。
        *core.timeline_stall_since.lock().unwrap() =
            Some(now - STALL_THRESHOLD - Duration::from_secs(1));
        assert!(core.compute_stall(now));
    }

    /// 快照缓存回归：seed 以 body 顶层为准（refresh/并发路径 last_timeline_seed
    /// 陈旧时不错标），且缓存解包 `snapshot` 子对象（timeline_turns 可解析）。
    /// 此前的两个根因——last_timeline_seed 错标 → 无限 deferred；缓存完整
    /// body → 解析恒空 → restore 空历史——曾导致 ChatView 历史永不恢复。
    #[test]
    fn timeline_snapshot_caches_authoritative_seed_and_inner() {
        let core = test_core();
        // 模拟陈旧标记（spawn_timeline_refresh 路径不更新 last_timeline_seed）。
        *core.last_timeline_seed.lock().unwrap() = "stale-seed".to_string();
        let body = serde_json::json!({
            "schema": "qaqh.Ringing",
            "version": 1,
            "server_epoch": "e1",
            "seed": "s1",
            "snapshot": {
                "watermark": 7,
                "turns": [
                    {"turn_id":"t1","created_seq":1,"user_text":"hi","sealed":true,"state":"completed","rounds":[]},
                    {"turn_id":"t2","created_seq":2,"user_text":"again","sealed":false,"state":"running","rounds":[]}
                ]
            },
            "has_more": false,
            "total_turns": 2
        });
        core.cache_timeline_snapshot(serde_json::from_value(body).expect("typed page"));
        // seed 标记取 body 权威值，不受 last_timeline_seed 陈旧影响。
        let (cached_seed, cached) = core.chat_timeline.lock().unwrap().clone().expect("cached");
        assert_eq!(cached_seed, "s1");
        // 解包 snapshot 子对象：turns 可直接解析（完整 body 会恒空）。
        assert_eq!(cached.turns.len(), 2, "must cache typed snapshot inner");
        assert_eq!(cached.turns[0].turn_id, "t1");
        // 连带投影：header_turns / last_turn_ids 以权威 seed 写入。
        assert_eq!(core.header_turns.lock().unwrap().get("s1"), Some(&2));
        assert_eq!(
            core.last_turn_ids
                .lock()
                .unwrap()
                .get("s1")
                .map(String::as_str),
            Some("t2")
        );
        // 三态 take：匹配 move 消费；不匹配保留（stale 不丢）。
        let taken = core.chat_timeline_take("s1");
        assert!(taken.is_some());
        assert!(core.chat_timeline.lock().unwrap().is_none());
    }

    /// BUG-003 回归：take 三态——匹配 move 消费、不匹配保留、空态 None。
    #[test]
    fn chat_timeline_take_matches_consumes_and_stale_preserves() {
        let core = test_core();
        let body = |seed: &str| {
            serde_json::json!({
                "schema": "qaqh.Ringing",
                "version": 1,
                "server_epoch": "e1",
                "seed": seed,
                "snapshot": {
                    "watermark": 1,
                    "turns": [
                        {"turn_id":"t1","created_seq":1,"user_text":"hi","sealed":true,"state":"completed","rounds":[]}
                    ]
                },
                "has_more": false,
                "total_turns": 1
            })
        };
        core.cache_timeline_snapshot(serde_json::from_value(body("s1")).expect("typed page"));
        // Matched：move 消费（零拷贝语义：消费后缓存清空）。
        let snap = core.chat_timeline_take("s1");
        assert!(snap.is_some());
        assert!(core.chat_timeline.lock().unwrap().is_none());
        // Stale：缓存其它 seed 时请求不匹配 → None 且缓存保留。
        core.cache_timeline_snapshot(serde_json::from_value(body("s2")).expect("typed page"));
        assert!(core.chat_timeline_take("s1").is_none());
        assert!(core.chat_timeline.lock().unwrap().is_some());
        // 后续匹配请求仍可消费（stale 保留不丢）。
        assert!(core.chat_timeline_take("s2").is_some());
        // Empty：缓存已空 → None。
        assert!(core.chat_timeline_take("s2").is_none());
    }

    /// BUG-003 回归：每次 resume 意图递增代次（作废在途任务）；
    /// already-active 重导航同样作废旧任务。
    #[test]
    fn resume_generation_marks_latest_intent() {
        let core = test_core();
        assert_eq!(core.resume_generation.load(Ordering::Relaxed), 0);
        // already-active 分支走同步路径（不依赖 SHARED_CORE），
        // 但同样必须先递增代次——重导航也要作废在途任务。
        core.set_active_seed("s1");
        core.spawn_resume("s1");
        assert_eq!(core.resume_generation.load(Ordering::Relaxed), 1);
        core.spawn_resume("s1");
        assert_eq!(core.resume_generation.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn open_timeline_status_resets_stall_timer() {
        let core = test_core();
        *core.timeline_stall_since.lock().unwrap() = Some(Instant::now() - Duration::from_secs(60));
        *core.timeline_status.lock().unwrap() = Some(TimelineStatus::Open {
            seed: "s1".into(),
            server_epoch: "e1".into(),
            cursor: 9,
        });
        assert!(!core.compute_stall(Instant::now()));
        assert!(core.timeline_stall_since.lock().unwrap().is_none());
    }

    #[test]
    fn all_channels_stalled_triggers_but_single_open_resets() {
        let core = test_core();
        let now = Instant::now();
        let mut reconnecting_map: HashMap<Channel, ChannelStatus> = HashMap::new();
        for ch in [Channel::Control, Channel::Conversation, Channel::Tool] {
            reconnecting_map.insert(
                ch,
                ChannelStatus::Reconnecting {
                    retry_ms: 1_000,
                    last_cursor: 0,
                },
            );
        }
        *core.channel_status.lock().unwrap() = reconnecting_map;
        // 开始计时，不触发。
        assert!(!core.compute_stall(now));
        assert!(core.channels_stall_since.lock().unwrap().is_some());
        // 超过阈值：触发。
        *core.channels_stall_since.lock().unwrap() =
            Some(now - STALL_THRESHOLD - Duration::from_secs(1));
        assert!(core.compute_stall(now));

        // 任一通道 open → 重置计时。
        let core2 = test_core();
        *core2.channels_stall_since.lock().unwrap() =
            Some(now - STALL_THRESHOLD - Duration::from_secs(1));
        core2.channel_status.lock().unwrap().insert(
            Channel::Conversation,
            ChannelStatus::Open {
                server_epoch: "e1".into(),
                cursor: 0,
            },
        );
        assert!(!core2.compute_stall(now));
        assert!(core2.channels_stall_since.lock().unwrap().is_none());
    }

    #[test]
    fn untracked_or_null_status_never_stalls() {
        // 无 client（状态为 null / 空）：不触发、不残留计时。
        let core = test_core();
        assert!(!core.compute_stall(Instant::now()));
        assert!(core.channels_stall_since.lock().unwrap().is_none());
        assert!(core.timeline_stall_since.lock().unwrap().is_none());
    }

    #[test]
    fn rebuild_cooldown_blocks_repeated_rebuilds() {
        let core = test_core();
        *core.last_rebuild_at.lock().unwrap() = Instant::now();
        // 冷却期内即使 stall 也不触发 rebuild（check 的 cooldown 分支）。
        *core.timeline_status.lock().unwrap() = Some(reconnecting());
        *core.timeline_stall_since.lock().unwrap() =
            Some(Instant::now() - STALL_THRESHOLD - Duration::from_secs(1));
        core.check_daemon_health();
        // rebuild_client 未执行（冷却）：rebuilding 保持 false。
        assert!(!core.rebuilding.load(Ordering::Relaxed));
    }

    #[test]
    fn rebuild_cooldown_backs_off_after_failures() {
        // 无失败：60s；每失败翻倍，封顶 960s。
        assert_eq!(rebuild_cooldown_for(0), Duration::from_secs(60));
        assert_eq!(rebuild_cooldown_for(1), Duration::from_secs(120));
        assert_eq!(rebuild_cooldown_for(2), Duration::from_secs(240));
        assert_eq!(rebuild_cooldown_for(3), Duration::from_secs(480));
        assert_eq!(rebuild_cooldown_for(4), Duration::from_secs(960));
        // 超过封顶不再增长（防溢出/无限退避）。
        assert_eq!(rebuild_cooldown_for(5), Duration::from_secs(960));
        assert_eq!(rebuild_cooldown_for(u32::MAX), Duration::from_secs(960));
        // 自动重连冷却同样退避（5s → 10/20/40/80/160/320 封顶）。
        assert_eq!(auto_reconnect_cooldown_for(0), Duration::from_secs(5));
        assert_eq!(auto_reconnect_cooldown_for(6), Duration::from_secs(320));
        assert_eq!(auto_reconnect_cooldown_for(99), Duration::from_secs(320));
    }
}
