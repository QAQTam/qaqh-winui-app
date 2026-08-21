use super::util::split_segments;
use super::*;
use crate::ToolBody;
use crate::protocol::{ConversationEvent, ProviderToolState, RoundDeltaKind};
use std::rc::Rc;

fn start_turn(ts: &mut Transcript, turn_id: &str) {
    ts.apply(&ConversationEvent::TurnStarted {
        turn_id: turn_id.into(),
        user_text: "hi".into(),
    });
}

fn restored_turns(n: usize) -> Vec<RestoredTurn> {
    (0..n)
        .map(|i| RestoredTurn {
            turn_id: format!("t{i}"),
            created_seq: i as u64,
            user_text: format!("q{i}"),
            status: TurnStatus::Completed,
            rounds: Vec::new(),
        })
        .collect()
}

/// 生成 t{start}..t{start+count} 区间的 turns（分页页面前插测试用）。
fn restored_turns_range(start: usize, count: usize) -> Vec<RestoredTurn> {
    (start..start + count)
        .map(|i| RestoredTurn {
            turn_id: format!("t{i}"),
            created_seq: i as u64,
            user_text: format!("q{i}"),
            status: TurnStatus::Completed,
            rounds: Vec::new(),
        })
        .collect()
}

/// restore 后只渲染最近 `WINDOW_DEFAULT_LEN` 个回合；短会话（未超过
/// `RESTORE_KEEP_TURNS`）全量保留。
#[test]
fn window_after_restore_is_tail_only() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(40));
    assert_eq!(ts.turn_count(), 40, "未超裁剪阈值 → 全量保留");
    assert_eq!(ts.window_len(), WINDOW_DEFAULT_LEN);
    assert_eq!(ts.window_turns()[0].turn_id, "t10", "窗口 = 最近 30 个");
    assert_eq!(ts.window_turns().last().unwrap().turn_id, "t39");
    assert!(!ts.window_full());
}

/// 大快照（超过窗口 + 上滚缓冲）restore 时：窗口外历史立即驱逐，
/// 内存只驻留 `RESTORE_KEEP_TURNS` 个 turn；更早历史由 daemon 分页
/// 按需回放（expand_window 耗尽缓冲后返回 0，调用方转分页拉取）。
#[test]
fn restore_evicts_history_beyond_buffer() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(300));
    // 窗口前 270 个 → 保留 100 上滚缓冲 → 共 30 + 100 = 130。
    assert_eq!(ts.turn_count(), RESTORE_KEEP_TURNS, "窗口外历史已驱逐");
    assert_eq!(ts.window_len(), WINDOW_DEFAULT_LEN);
    assert_eq!(ts.window_turns()[0].turn_id, "t270", "窗口 = 最新 30 个");
    assert_eq!(ts.window_turns().last().unwrap().turn_id, "t299");
    assert_eq!(ts.turns()[0].turn_id, "t170", "缓冲起点 = 窗口前 100");
    // 上滚缓冲可本地扩展：expand 100 后窗口起点到达 t170。
    assert_eq!(ts.expand_window(100), 100);
    assert_eq!(ts.window_turns()[0].turn_id, "t170");
    assert!(ts.window_full());
    // 缓冲耗尽后再扩展返回 0（调用方转 daemon 分页）。
    assert_eq!(ts.expand_window(10), 0);
}

/// 短会话（少于窗口大小）：窗口 = 全量，window_full 立即为 true。
#[test]
fn window_is_full_for_short_sessions() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(5));
    assert_eq!(ts.window_len(), 5);
    assert!(ts.window_full());
    assert_eq!(ts.expand_window(10), 0, "无更早回合可扩展");
}

/// GFM 表格流式分段：增量喂表头/分隔/数据行 → 产出 Table 段，
/// 表头行从可见字面剥离（骨架先行、逐格填充的输入侧）。
#[test]
fn gfm_table_streams_to_table_segment() {
    let mut rv = RoundView::new(1);
    rv.answer_delta("| A | B |\n");
    rv.answer_delta("| --- | --- |\n");
    rv.answer_delta("| 1 | 2 |\n");
    let AnswerView::Streaming {
        raw,
        segments,
        table_tracker,
        gfm_table_tracker,
        ..
    } = &rv.answer
    else {
        panic!("expected streaming")
    };
    assert!(
        segments.iter().any(|s| matches!(s, LiveSegment::Table(_))),
        "GFM 表格应产出 Table 段"
    );
    let (visible, _) = split_segments(raw, table_tracker, gfm_table_tracker);
    assert!(!visible.contains("| A | B |"), "表头行应从可见字面剥离");
}

/// GFM 表格在 ``` 代码围栏内不激活（FenceState 围栏感知），保持字面。
#[test]
fn gfm_table_inside_code_fence_stays_literal() {
    let mut rv = RoundView::new(1);
    rv.answer_delta("```\n");
    rv.answer_delta("| A | B |\n");
    rv.answer_delta("| --- | --- |\n");
    rv.answer_delta("```\n");
    let AnswerView::Streaming { segments, .. } = &rv.answer else {
        panic!("expected streaming")
    };
    assert!(
        !segments.iter().any(|s| matches!(s, LiveSegment::Table(_))),
        "代码围栏内表格不激活"
    );
}

/// expand_window 前移起点；到 0 后短路（返回 0，避免无谓渲染）。
#[test]
fn expand_window_moves_start_and_short_circuits() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(40));
    assert!(ts.tail_following());
    assert_eq!(ts.expand_window(10), 10);
    assert_eq!(ts.window_len(), 40);
    assert!(ts.window_full());
    assert!(!ts.tail_following(), "用户上滚扩展后脱离跟随尾部");
    assert_eq!(ts.expand_window(10), 0, "已全量放行，短路");
    assert_eq!(ts.window_turns()[0].turn_id, "t0");
    ts.slide_window_tail();
    assert!(ts.tail_following(), "滑动恢复跟随尾部");
}

/// 分页前插：更早一页插到最前，窗口起点右移，turn 顺序正确。
#[test]
fn prepend_turns_puts_earlier_page_in_front() {
    let mut ts = Transcript::new();
    // resume 只拿到尾部页 t10..t39（30 个）。
    ts.restore(restored_turns_range(10, 30));
    assert_eq!(ts.turn_count(), 30);
    assert_eq!(ts.window_len(), 30, "30 个全在窗口内");
    // 上滚翻页：t0..t9 前插。
    let n = ts.prepend_turns(restored_turns_range(0, 10));
    assert_eq!(n, 10);
    assert_eq!(ts.turn_count(), 40);
    assert_eq!(ts.turns().first().unwrap().turn_id, "t0");
    assert_eq!(ts.turns().last().unwrap().turn_id, "t39");
    // 窗口起点右移 10：渲染视图仍是尾部 30 个（t10..t39）。
    assert_eq!(ts.window_len(), 30);
    assert_eq!(ts.window_turns().first().unwrap().turn_id, "t10");
    assert_eq!(ts.expand_window(10), 10, "可继续向前扩展 t0..t9");
}

/// 页码边界重叠去重：重复 turn 跳过，不重复计数。
#[test]
fn prepend_turns_skips_overlapping_turn_ids() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns_range(20, 20));
    // 服务端翻页可能返回 t15..t25（重叠 t20..t24 已加载）。
    let n = ts.prepend_turns(restored_turns_range(15, 10));
    assert_eq!(n, 5, "t20..t24 已存在跳过");
    assert_eq!(ts.turn_count(), 25);
    assert_eq!(ts.turns().first().unwrap().turn_id, "t15");
    // 空页 / 全重叠页 → 0。
    assert_eq!(ts.prepend_turns(Vec::new()), 0);
    assert_eq!(ts.prepend_turns(restored_turns_range(15, 10)), 0);
}

/// slide_window_tail：跟随尾部时窗口保持大小为 WINDOW_DEFAULT_LEN；
/// 用户上滚扩展后调用则回到最近 N 个（由调用方决定何时调用）。
#[test]
fn slide_window_tail_keeps_window_size() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(40));
    // 已是最新 30：滑动无变化。
    ts.slide_window_tail();
    assert_eq!(ts.window_len(), WINDOW_DEFAULT_LEN);
    assert_eq!(ts.window_turns()[0].turn_id, "t10");
    // 用户扩展窗口（上滚预加载）后，跟随尾部时滑回最近 N 个。
    ts.expand_window(10);
    assert_eq!(ts.window_len(), 40);
    ts.slide_window_tail();
    assert_eq!(ts.window_len(), WINDOW_DEFAULT_LEN);
    assert_eq!(ts.window_turns()[0].turn_id, "t10");
}

/// 新 turn 追加（增量事件）不影响窗口起点；窗口是渲染投影，滑动由
/// 调用方在「跟随尾部」时显式 `slide_window_tail`。
#[test]
fn apply_growth_keeps_window_consistent() {
    let mut ts = Transcript::new();
    ts.restore(restored_turns(40));
    start_turn(&mut ts, "t40");
    assert_eq!(ts.turn_count(), 41);
    assert_eq!(ts.window_len(), 31, "起点不动，窗口随尾部增长");
    assert_eq!(ts.window_turns()[0].turn_id, "t10", "起点未变");
    assert_eq!(ts.window_turns().last().unwrap().turn_id, "t40");
    // 跟随尾部：显式滑动，窗口回到最近 N 个。
    ts.slide_window_tail();
    assert_eq!(ts.window_len(), WINDOW_DEFAULT_LEN);
    assert_eq!(ts.window_turns()[0].turn_id, "t11");
}

/// `provider_tool_status` 按 call_id upsert：状态流 进行中→搜索中→完成，
/// 同 id 覆盖不重复加卡；done 随 completed 置位。
#[test]
fn provider_tool_status_upserts_card() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    ts.apply(&ConversationEvent::ProviderToolStatus {
        turn_id: "t1".into(),
        round_num: 0,
        call_id: "call-1".into(),
        tool_kind: "web_search".into(),
        state: ProviderToolState::InProgress,
    });
    assert_eq!(ts.turns()[0].rounds[0].tool_calls.len(), 1);
    let card = &ts.turns()[0].rounds[0].tool_calls[0];
    assert_eq!(card.id, "call-1");
    assert!(card.provider);
    assert!(!card.done);
    assert_eq!(card.args_display, "进行中…");

    // 状态流转：同 id 覆盖。
    ts.apply(&ConversationEvent::ProviderToolStatus {
        turn_id: "t1".into(),
        round_num: 0,
        call_id: "call-1".into(),
        tool_kind: "web_search".into(),
        state: ProviderToolState::Searching,
    });
    ts.apply(&ConversationEvent::ProviderToolStatus {
        turn_id: "t1".into(),
        round_num: 0,
        call_id: "call-1".into(),
        tool_kind: "web_search".into(),
        state: ProviderToolState::Completed,
    });
    let rounds = &ts.turns()[0].rounds;
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].tool_calls.len(), 1, "同 call_id 覆盖不新增卡");
    assert!(rounds[0].tool_calls[0].done);
    assert_eq!(rounds[0].tool_calls[0].args_display, "");
}

/// Tool 频道事件（ToolCallPrepared → ToolStarted → ToolFinished）按
/// tool_call_id upsert；流式时工具卡从「预览」→「执行中」→「完成」。
#[test]
fn tool_channel_events_upsert_card() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");

    // Prepared：预览卡（带 args）。
    ts.apply(&ConversationEvent::ToolCallPrepared {
        tool_call_id: "call-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "exec".into(),
        args_so_far: "{\"cmd\":\"ls\"}".into(),
    });
    let card = &ts.turns()[0].rounds[0].tool_calls[0];
    assert_eq!(card.id, "call-1");
    assert_eq!(card.name.as_deref(), Some("exec"));
    assert!(!card.done);
    assert!(!card.provider);

    // Started：同 id 覆盖（清 args 展示）。
    ts.apply(&ConversationEvent::ToolStarted {
        tool_call_id: "call-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "exec".into(),
    });
    let rounds = &ts.turns()[0].rounds;
    assert_eq!(rounds[0].tool_calls.len(), 1, "同 id 不新增卡");
    assert!(!rounds[0].tool_calls[0].done);

    // Finished：done 置位 + 结果摘要。
    ts.apply(&ConversationEvent::ToolFinished {
        tool_call_id: "call-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        result: serde_json::json!({ "summary": "8 files listed" }),
    });
    let rounds = &ts.turns()[0].rounds;
    assert_eq!(rounds[0].tool_calls.len(), 1);
    assert!(rounds[0].tool_calls[0].done);
    assert_eq!(rounds[0].tool_calls[0].args_display, "8 files listed");
}

#[test]
fn read_tool_keeps_args_and_finishes_as_native_code() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    ts.apply(&ConversationEvent::ToolCallPrepared {
        tool_call_id: "read-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "read_file".into(),
        args_so_far: r#"{"path":"src/lib.rs","start_line":4}"#.into(),
    });
    ts.apply(&ConversationEvent::ToolStarted {
        tool_call_id: "read-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "read_file".into(),
    });
    ts.apply(&ConversationEvent::ToolFinished {
        tool_call_id: "read-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        result: serde_json::json!({
            "summary": "read src/lib.rs",
            "data": {"files": [{"path": "src/lib.rs", "start_line": 4, "total_lines": 8}]},
            "model": {"text": "L4: pub fn answer() -> u32 {\nL5:     42\nL6: }"}
        }),
    });

    let card = &ts.turns()[0].rounds[0].tool_calls[0];
    assert_eq!(
        card.args_json.as_deref(),
        Some(r#"{"path":"src/lib.rs","start_line":4}"#)
    );
    let ToolBody::Code(documents) = &card.body else {
        panic!("read_file should render as native code");
    };
    assert_eq!(documents[0].path.as_deref(), Some("src/lib.rs"));
    assert_eq!(documents[0].start_line, 4);
    assert_eq!(
        documents[0].text.lines().next(),
        Some("pub fn answer() -> u32 {")
    );
}

#[test]
fn code_changed_targets_one_card_and_is_idempotent() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    for call_id in ["edit-1", "edit-2"] {
        ts.apply(&ConversationEvent::ToolCallPrepared {
            tool_call_id: call_id.into(),
            turn_id: "t1".into(),
            round_num: 0,
            name: "edit_file".into(),
            args_so_far: "{}".into(),
        });
    }
    let changed = ConversationEvent::CodeChanged {
        tool_call_id: "edit-2".into(),
        turn_id: "t1".into(),
        round_num: 0,
        lines_added: 7,
        lines_removed: 3,
        files_created: 1,
        files_deleted: 0,
        file: Some("src/lib.rs".into()),
    };
    assert!(ts.apply(&changed).changed());
    assert!(
        !ts.apply(&changed).changed(),
        "same stats should not invalidate twice"
    );

    let cards = &ts.turns()[0].rounds[0].tool_calls;
    assert!(cards[0].changes.is_none());
    assert_eq!(cards[1].changes.as_ref().unwrap().label(), "+7  −3  新建 1");
}

#[test]
fn compact_duplicate_finish_does_not_replace_exact_diff() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    ts.apply(&ConversationEvent::ToolCallPrepared {
        tool_call_id: "edit-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "edit_file".into(),
        args_so_far: r#"{"path":"src/lib.rs"}"#.into(),
    });
    ts.apply(&ConversationEvent::ToolFinished {
            tool_call_id: "edit-1".into(),
            turn_id: "t1".into(),
            round_num: 0,
            result: serde_json::json!({
                "summary": "edited",
                "data": {"files": [{"ops": [{"diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"}]}]},
                "model": {"text": "+1 -1"}
            }),
        });
    ts.apply(&ConversationEvent::ToolFinished {
        tool_call_id: "edit-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        result: serde_json::json!({"summary": "+1 -1", "model": {"text": "+1 -1"}}),
    });
    assert!(matches!(
        ts.turns()[0].rounds[0].tool_calls[0].body,
        ToolBody::Diff(_)
    ));
}

/// Tool 频道与 Conversation 频道无顺序保证：工具事件先于 TurnStarted
/// 到达时自动建 turn，不丢卡。
#[test]
fn tool_event_before_turn_started_creates_turn() {
    let mut ts = Transcript::new();
    ts.apply(&ConversationEvent::ToolCallPrepared {
        tool_call_id: "call-1".into(),
        turn_id: "t1".into(),
        round_num: 0,
        name: "file".into(),
        args_so_far: "{}".into(),
    });
    assert_eq!(ts.turns().len(), 1, "自动建 turn");
    assert_eq!(ts.turns()[0].turn_id, "t1");
    assert_eq!(ts.turns()[0].rounds[0].tool_calls.len(), 1);
}

/// 未知 turn 的 provider 状态：忽略（防跨回合错灌）。
#[test]
fn provider_tool_status_unknown_turn_ignored() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    let change = ts.apply(&ConversationEvent::ProviderToolStatus {
        turn_id: "ghost".into(),
        round_num: 0,
        call_id: "call-1".into(),
        tool_kind: "web_search".into(),
        state: ProviderToolState::Completed,
    });
    assert!(!change.changed());
    assert!(ts.turns()[0].rounds.is_empty());
}

/// 与 QAQ-Harness 工具调用卡（ToolCalling 流）共存：不同 id 各自成卡。
#[test]
fn provider_card_coexists_with_qaqh_tool_card() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    ts.apply(&ConversationEvent::RoundDelta {
        turn_id: "t1".into(),
        round_num: 0,
        kind: RoundDeltaKind::ToolCalling,
        delta: "{\"id\":\"c1\",\"name\":\"web_search\"".into(),
    });
    ts.apply(&ConversationEvent::ProviderToolStatus {
        turn_id: "t1".into(),
        round_num: 0,
        call_id: "call-1".into(),
        tool_kind: "web_search".into(),
        state: ProviderToolState::Searching,
    });
    let cards = &ts.turns()[0].rounds[0].tool_calls;
    assert_eq!(cards.len(), 2);
    assert!(cards.iter().any(|c| c.id == "c1" && !c.provider));
    assert!(cards.iter().any(|c| c.id == "call-1" && c.provider));
}

#[test]
fn round_copy_on_write_preserves_completed_sibling_identity() {
    let mut ts = Transcript::new();
    start_turn(&mut ts, "t1");
    for round_num in [0, 1] {
        ts.apply(&ConversationEvent::RoundDelta {
            turn_id: "t1".into(),
            round_num,
            kind: RoundDeltaKind::Answering,
            delta: format!("round-{round_num}"),
        });
    }

    let completed = ts.turns()[0].rounds[0].clone();
    let active_before = ts.turns()[0].rounds[1].clone();
    let active_rev = active_before.mutation_rev;
    ts.apply(&ConversationEvent::RoundDelta {
        turn_id: "t1".into(),
        round_num: 1,
        kind: RoundDeltaKind::Answering,
        delta: "-next".into(),
    });

    let rounds = &ts.turns()[0].rounds;
    assert!(Rc::ptr_eq(&completed, &rounds[0]));
    assert!(!Rc::ptr_eq(&active_before, &rounds[1]));
    assert_eq!(rounds[1].mutation_rev, active_rev.wrapping_add(1));
}

#[test]
fn restore_reuses_unchanged_round_identity() {
    let snapshot = RestoredTurn {
        turn_id: "t1".into(),
        created_seq: 1,
        user_text: "question".into(),
        status: TurnStatus::Completed,
        rounds: vec![RestoredRound {
            round_num: 0,
            thinking: None,
            answer: Some("answer".into()),
            tool_calls: Vec::new(),
        }],
    };
    let mut ts = Transcript::new();
    ts.restore(vec![snapshot.clone()]);
    let first = ts.turns()[0].rounds[0].clone();
    ts.restore(vec![snapshot]);
    assert!(Rc::ptr_eq(&first, &ts.turns()[0].rounds[0]));
}
