use super::*;
use crate::render_final;
use markdown_core::ast::Inline;
use markdown_core::gfm_live_table::GfmTableTracker;
use markdown_core::live_table::{LiveTableTracker, TableHiddenSpan, TableSnapshot};
use markdown_core::parse_final;

/// TurnFailed 的 DomainError（`{error_id, code, message, retryable, ..}`）
/// 提取为 UI 可显示文本；未知形状时给出兜底文案。
pub(super) fn extract_failed_error(error: &serde_json::Value) -> String {
    let code = error
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("turn_failed");
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Model request failed");
    let retryable = error
        .get("retryable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if retryable {
        format!("{code}: {message}（可重试）")
    } else {
        format!("{code}: {message}")
    }
}

/// 极简 JSON 字符串提取（原型用；正式实现由应用层工具卡解析器承担）。
pub(super) fn extract_json_str(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = raw.find(&needle)?;
    let after = &raw[idx + needle.len()..];
    let colon = after.find(':')?;
    let value = &after[colon + 1..].trim_start();
    let value = value.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

/// raw → (可见字面拼接, 字面/表格交错序列)。
///
/// 按表格隐藏区间切分：区间外字面进 Text 段，区间内进 Table 段
/// （sealed 表格为完整快照；打开表格含残行 partial 作为网格末行）。
/// 残行不重复出现在 Text 段（`open_tail_start` 截断）。
/// 字面/表格分段：协议表格（```table）与 GFM 表格（|...|）各自隐藏
/// 区间合并（GFM tracker 围栏感知 → 两者 span 不重叠），按起点排序后
/// 统一产出可见字面 + 表格段。尾部字面到两个 tracker 中更早的残行起点。
pub(super) fn split_segments(
    raw: &str,
    protocol_tracker: &LiveTableTracker,
    gfm_tracker: &GfmTableTracker,
) -> (String, Vec<LiveSegment>) {
    let mut spans: Vec<(TableHiddenSpan, TableSnapshot)> = protocol_tracker.tables_with_spans();
    spans.extend(gfm_tracker.tables_with_spans());
    spans.sort_by_key(|(s, _)| s.start);
    let mut segments: Vec<LiveSegment> = Vec::new();
    let mut visible = String::new();
    let mut prev = 0usize;
    for (span, snap) in spans {
        if span.start > prev {
            let text = raw[prev..span.start].to_string();
            visible.push_str(&text);
            segments.push(LiveSegment::Text(text));
        }
        segments.push(LiveSegment::Table(table_snapshot_to_data(snap)));
        prev = span.end;
    }
    // 尾部字面：从 prev 到残行起点（残行已在网格末行，不重复显示）
    let tail_end = [
        protocol_tracker.open_tail_start(),
        gfm_tracker.open_tail_start(),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(raw.len());
    if prev < tail_end {
        let text = raw[prev..tail_end].to_string();
        visible.push_str(&text);
        segments.push(LiveSegment::Text(text));
    }
    (visible, segments)
}

/// 协议表格快照 → 渲染用 TableData（单元格包一层纯文本 Inline）。
pub(super) fn table_snapshot_to_data(s: TableSnapshot) -> TableData {
    TableData {
        headers: s
            .headers
            .iter()
            .map(|h| vec![Inline::Text(h.clone())])
            .collect(),
        rows: s
            .rows
            .iter()
            .map(|r| r.iter().map(|c| vec![Inline::Text(c.clone())]).collect())
            .collect(),
    }
}

/// 按 tool_call_id upsert 工具卡（同 id 覆盖状态，保持卡位置稳定）。
pub(super) fn upsert_tool_card(round: &mut RoundView, card: ToolCardView) -> bool {
    if let Some(existing) = round.tool_calls.iter_mut().find(|c| c.id == card.id) {
        if *existing == card {
            return false;
        }
        *existing = card;
    } else {
        round.tool_calls.push(card);
    }
    true
}

pub(super) fn same_rendered_turn(old: &TurnView, new: &TurnView) -> bool {
    old.turn_id == new.turn_id
        && old.user_text == new.user_text
        && old.status == new.status
        && old.failed_error == new.failed_error
        && old.rounds.len() == new.rounds.len()
        && old
            .rounds
            .iter()
            .zip(&new.rounds)
            .all(|(old, new)| same_rendered_round(old, new))
}

pub(super) fn same_rendered_round(old: &RoundView, new: &RoundView) -> bool {
    old.round_num == new.round_num
        && old.thinking == new.thinking
        && old.answer == new.answer
        && old.tool_calls == new.tool_calls
        && old.output_loading == new.output_loading
        && old.output_error == new.output_error
}

/// RestoredTurn → TurnView（历史回合直接落 Final；restore 与分页前插共用）。
pub(super) fn to_turn_view(t: RestoredTurn) -> TurnView {
    TurnView {
        turn_id: t.turn_id.clone(),
        user_text: t.user_text,
        status: t.status,
        failed_error: None,
        rounds: t
            .rounds
            .into_iter()
            .map(|r| {
                let mut round = RoundView::new(r.round_num);
                round.thinking = r.thinking;
                round.tool_calls = r.tool_calls;
                round.answer = match r.answer {
                    Some(a) => {
                        let blocks = parse_final(&a);
                        round.final_raw = Some(a);
                        AnswerView::Final {
                            rich: render_final(&blocks),
                            blocks,
                        }
                    }
                    None => AnswerView::Final {
                        rich: RichTextOutput::default(),
                        blocks: Vec::new(),
                    },
                };
                Rc::new(round)
            })
            .collect(),
        mutation_rev: 0,
    }
}
