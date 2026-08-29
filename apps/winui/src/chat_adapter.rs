//! Timeline → ChatView presentation 转换（Phase 2 收窄后）。
//!
//! transcript 渲染（snapshot + live + prepend）统一由 timeline 驱动
//! （BlockTranscript 单源）；conversation/tool 频道的 transcript 映射
//! （render_event/restored_turns）已随 Phase 2/3 退役（净删约 400 行），
//! 剩余领域事件（usage/dashboard/activity/audit）由 bridge 直接以 typed
//! 形式消费，不再经过本适配器。
//!
//! 本模块只做 serde 形状转换：`qaqh-client` 的 timeline 类型与
//! `markdown-winui::timeline_protocol` 形状一致（对齐 `qaqh-domain`），
//! 零胶水 roundtrip；协议漂移由编译器/转换失败暴露。

/// qaqh-client timeline entry → markdown-winui timeline entry。
///
/// 形状已对齐 `qaqh-domain`（serde 零胶水 roundtrip）；转换失败返回
/// `None`（协议漂移时防御性丢弃，绝不 panic）。
pub fn timeline_entry(entry: &qaqh_client::TimelineEntry) -> Option<markdown_winui::TimelineEntry> {
    serde_json::to_value(entry)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// qaqh-client timeline snapshot → markdown-winui timeline snapshot
/// （restore / 分页前插共用；失败返回空快照——调用方按空会话处理）。
pub fn timeline_snapshot(
    snapshot: &qaqh_client::TimelineSnapshot,
) -> Option<markdown_winui::TimelineSnapshot> {
    serde_json::to_value(snapshot)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// 快照内容指纹（chat_view 泵 restore 前去重用）。
///
/// 手写字段游走 + `discriminant` 哈希，零分配 O(内容总量)——相比
/// [`timeline_snapshot`] 的 serde_json::Value 双程转换（建树 + 解析，
/// 切换卡顿的单帧大头）成本低两个数量级。比对语义：与上次已恢复快照
/// 完全一致 → 跳过转换与 restore（transcript 由 timeline 事件单调
/// 演进，内容 ⊇ 该快照，重放只会倒退）。
///
/// 注意：工具块全部展示字段（summary/output/diff/progress/失败/权限）
/// 必须纳入——任何一项变化都代表渲染结果变化，漏掉会导致陈旧 UI。
pub fn snapshot_fingerprint(snapshot: &qaqh_client::TimelineSnapshot) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    snapshot.watermark.hash(&mut h);
    for turn in &snapshot.turns {
        turn.turn_id.hash(&mut h);
        turn.created_seq.hash(&mut h);
        turn.user_text.hash(&mut h);
        turn.sealed.hash(&mut h);
        std::mem::discriminant(&turn.state).hash(&mut h);
        // failure 类型（qaqh_domain::TimelineFailure）未被 qaqh-client
        // 导出——不具名，靠字段访问推断；其它处同理全部内联。
        if let Some(f) = &turn.failure {
            1u8.hash(&mut h);
            f.code.hash(&mut h);
            f.message.hash(&mut h);
        } else {
            0u8.hash(&mut h);
        }
        for round in &turn.rounds {
            round.round_num.hash(&mut h);
            round.sealed.hash(&mut h);
            round.is_final.hash(&mut h);
            for block in &round.blocks {
                block.block_id.hash(&mut h);
                block.block_order.hash(&mut h);
                std::mem::discriminant(&block.kind).hash(&mut h);
                std::mem::discriminant(&block.state).hash(&mut h);
                block.text.hash(&mut h);
                if let Some(tool) = &block.tool {
                    1u8.hash(&mut h);
                    tool.tool_call_id.hash(&mut h);
                    tool.name.hash(&mut h);
                    std::mem::discriminant(&tool.state).hash(&mut h);
                    if let Some(v) = &tool.summary {
                        1u8.hash(&mut h);
                        v.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                    if let Some(v) = &tool.args_json {
                        1u8.hash(&mut h);
                        v.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                    if let Some(v) = &tool.output {
                        1u8.hash(&mut h);
                        v.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                    if let Some(v) = &tool.diff {
                        1u8.hash(&mut h);
                        v.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                    tool.progress.hash(&mut h);
                    if let Some(f) = &tool.failure {
                        1u8.hash(&mut h);
                        f.code.hash(&mut h);
                        f.message.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                    if let Some(p) = &tool.permission {
                        1u8.hash(&mut h);
                        p.reason.hash(&mut h);
                        for path in &p.paths {
                            path.hash(&mut h);
                        }
                        p.category.hash(&mut h);
                        p.level.hash(&mut h);
                        p.risk.hash(&mut h);
                        p.consequence.hash(&mut h);
                    } else {
                        0u8.hash(&mut h);
                    }
                } else {
                    0u8.hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// entry 转换 roundtrip：TextDelta 保持字段完整。
    #[test]
    fn timeline_entry_roundtrips() {
        let entry = qaqh_client::TimelineEntry {
            timeline_seq: 7,
            turn_id: "t1".into(),
            round_num: Some(0),
            event: qaqh_client::TimelineEvent::TextDelta {
                block_id: "text:b1".into(),
                fragment_seq: 3,
                delta: "hel".into(),
            },
        };
        let converted = timeline_entry(&entry).expect("convert");
        assert_eq!(converted.timeline_seq, 7);
        assert_eq!(converted.turn_id, "t1");
        assert!(matches!(
            converted.event,
            markdown_winui::TimelineEvent::TextDelta {
                block_id,
                fragment_seq: 3,
                delta,
            } if block_id == "text:b1" && delta == "hel"
        ));
    }

    /// 快照转换 roundtrip：turns/rounds/blocks 层级完整。
    #[test]
    fn timeline_snapshot_roundtrips() {
        let snapshot = qaqh_client::TimelineSnapshot {
            watermark: 3,
            turns: vec![qaqh_client::TimelineTurn {
                turn_id: "t1".into(),
                created_seq: 1,
                user_text: "hi".into(),
                sealed: true,
                state: qaqh_client::TimelineTurnState::Completed,
                failure: None,
                rounds: vec![qaqh_client::TimelineRound {
                    round_num: 0,
                    sealed: true,
                    is_final: true,
                    blocks: vec![qaqh_client::TimelineBlock {
                        block_id: "text:b1".into(),
                        block_order: 0,
                        kind: qaqh_client::TimelineBlockKind::Text,
                        state: qaqh_client::TimelineBlockState::Sealed,
                        text: "answer".into(),
                        tool: None,
                    }],
                }],
            }],
        };
        let converted = timeline_snapshot(&snapshot).expect("convert");
        assert_eq!(converted.watermark, 3);
        assert_eq!(converted.turns.len(), 1);
        assert_eq!(converted.turns[0].rounds[0].blocks[0].text, "answer");
        assert_eq!(
            converted.turns[0].rounds[0].blocks[0].kind,
            markdown_winui::TimelineBlockKind::Text
        );
    }
}
