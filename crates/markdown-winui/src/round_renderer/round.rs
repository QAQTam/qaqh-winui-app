use super::util::extract_json_str;
use super::*;
use crate::ToolBody;

impl RoundView {
    pub(super) fn new(round_num: u32) -> Self {
        Self {
            round_num,
            ..Self::default()
        }
    }

    /// 追加 Answering 增量（委托 [`AnswerView::live_delta`]，同一份答案状态机）。
    pub(super) fn answer_delta(&mut self, delta: &str) -> bool {
        self.answer.live_delta(delta)
    }

    /// BlockCheckpoint 覆盖（委托 [`AnswerView::live_checkpoint`]）。
    pub(super) fn answer_checkpoint(&mut self, text: &str) -> bool {
        self.answer.live_checkpoint(text)
    }

    /// RoundCompleted：以权威 answer 全量重建（忽略流式累积差异）。
    pub(super) fn finalize(&mut self, thinking: Option<&str>, answer: Option<&str>) -> bool {
        let mut changed = false;
        if let Some(t) = thinking {
            if self.thinking.as_deref() != Some(t) {
                self.thinking = Some(t.to_string());
                changed = true;
            }
        }
        if let Some(a) = answer {
            if self.final_raw.as_deref() != Some(a) {
                // final_raw 不同 ⇒ 文本不同或仍流式 ⇒ finalize_text 必返回 true
                self.answer.finalize_text(a);
                self.final_raw = Some(a.to_string());
                changed = true;
            }
            if self.output_loading || self.output_error.is_some() {
                self.output_loading = false;
                self.output_error = None;
                changed = true;
            }
        }
        changed
    }

    /// ToolCalling 增量：累积并尝试提取工具名（upsert by id）。
    pub(super) fn tool_delta(&mut self, delta: &str) -> bool {
        if self.tool_calls.last().is_some_and(|c| c.done) {
            return false; // 上一张卡已完成
        }
        if delta.is_empty() {
            return false;
        }
        self.tool_raw.push_str(delta);
        self.upsert_current_card().is_some()
    }

    pub(super) fn tool_checkpoint(&mut self, text: &str) -> bool {
        if self.tool_raw == text {
            return false;
        }
        self.tool_raw.clear();
        self.tool_raw.push_str(text);
        self.upsert_current_card().is_some()
    }

    /// 把当前累积的卡写入 tool_calls（同 id 更新，否则新建）。
    fn upsert_current_card(&mut self) -> Option<ToolCardView> {
        let card = self.current_tool_card()?;
        if let Some(existing) = self
            .tool_calls
            .iter_mut()
            .find(|c| !c.id.is_empty() && c.id == card.id)
        {
            existing.name = card.name.clone();
            existing.args_display.clone_from(&card.args_display);
            existing.args_json.clone_from(&card.args_json);
            existing.body.clone_from(&card.body);
            existing.changes.clone_from(&card.changes);
        } else {
            self.tool_calls.push(card.clone());
        }
        Some(card)
    }

    /// 从累积 raw 提取工具卡（原型简化解析：`"name":"..."` 与 `"id":"..."`）。
    fn current_tool_card(&self) -> Option<ToolCardView> {
        if self.tool_raw.trim().is_empty() {
            return None;
        }
        let id = extract_json_str(&self.tool_raw, "id").unwrap_or_default();
        let name = extract_json_str(&self.tool_raw, "name");
        Some(ToolCardView {
            id,
            name,
            args_display: self.tool_raw.clone(),
            args_json: Some(self.tool_raw.clone()),
            body: ToolBody::Text(self.tool_raw.clone()),
            changes: None,
            done: false,
            failed: false,
            failure: None,
            provider: false,
            started: true,
        })
    }

    /// 工具调用完成（RoundCompleted 时收尾所有卡）。
    pub(super) fn finish_tool_cards(&mut self) -> bool {
        let mut changed = false;
        for card in &mut self.tool_calls {
            changed |= !card.done;
            card.done = true;
        }
        changed
    }
}
