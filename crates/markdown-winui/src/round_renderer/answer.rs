use super::util::split_segments;
use super::*;
use crate::render_final;
use markdown_core::live::parse_live;
use markdown_core::parse_final;

impl Default for AnswerView {
    fn default() -> Self {
        Self::Streaming {
            raw: String::new(),
            inlines: Vec::new(),
            segments: Vec::new(),
            table_tracker: LiveTableTracker::default(),
            gfm_table_tracker: GfmTableTracker::default(),
        }
    }
}

impl AnswerView {
    /// 当前是否处于流式且有可见内容（失败冻结兜底判定用）。
    pub fn streaming_nonempty(&self) -> bool {
        matches!(self, Self::Streaming { raw, .. } if !raw.trim().is_empty())
    }

    /// 追加增量：累积 raw → 表格跟踪 → 行内预览。
    ///
    /// 未闭合语法跨 delta 边界（`**bo` + `ld**`），必须对**整段 raw**
    /// 重解析（O(段长)；UI 层以 DispatcherQueue 节流合并，同 Web rAF）。
    /// 协议表格（```table）行级确认：表格行从字面剥离进 `segments`，
    /// 残行实时显示在网格末行（逐字生长），字面保留在 Text 段。
    /// 解析结果同步写回 `Self::Streaming`（状态机自持）。
    pub fn live_delta(&mut self, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let Self::Streaming {
            raw,
            inlines,
            segments,
            table_tracker,
            gfm_table_tracker,
        } = self
        else {
            return false; // 终态后忽略 delta（协议保证不会发生）
        };
        raw.push_str(delta);
        // 表格跟踪：增量行扫描（O(新增行)）——协议表格 + GFM 表格。
        table_tracker.feed(raw);
        gfm_table_tracker.feed(raw);
        // 可见字面 = raw 减去表格隐藏区间；segments = 字面/表格交错
        let (visible, segs) = split_segments(raw, table_tracker, gfm_table_tracker);
        *inlines = parse_live(&visible);
        *segments = segs;
        true
    }

    /// BlockCheckpoint 覆盖（自愈）：整段替换，重解析（表格跟踪器重置）。
    /// 相同值幂等（防抖：不产生模型失效）。
    pub fn live_checkpoint(&mut self, text: &str) -> bool {
        let Self::Streaming {
            raw,
            inlines,
            segments,
            table_tracker,
            gfm_table_tracker,
        } = self
        else {
            return false;
        };
        if *raw == text {
            return false;
        }
        table_tracker.reset();
        gfm_table_tracker.reset();
        raw.clear();
        raw.push_str(text);
        table_tracker.feed(raw);
        gfm_table_tracker.feed(raw);
        let (visible, segs) = split_segments(raw, table_tracker, gfm_table_tracker);
        *inlines = parse_live(&visible);
        *segments = segs;
        true
    }

    /// 权威终态：以完整文本全量重建为 Final（冻结，忽略流式累积差异）。
    /// 幂等：已是 Final 时返回 false。
    pub fn finalize_text(&mut self, text: &str) -> bool {
        let Self::Streaming { .. } = self else {
            return false;
        };
        let blocks = parse_final(text);
        let rich = render_final(&blocks);
        *self = Self::Final { rich, blocks };
        true
    }
}
