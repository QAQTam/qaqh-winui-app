use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::coalesce::coalesce_adjacent_deltas;
use super::util::{
    extract_failed_error, same_rendered_round, same_rendered_turn, to_turn_view, upsert_tool_card,
};
use super::*;
use crate::protocol::{ConversationEvent, ProviderToolState, RoundDeltaKind};
use crate::{ChangeStats, ToolBody, change_stats_from_result, tool_body_from_result};

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已挂载 turn 数（规模观测）。
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    pub fn turns(&self) -> &[TurnView] {
        &self.turns
    }

    /// 当前渲染窗口（尾部连续区间切片）——list_view 的实际数据源。
    /// 窗口化后每帧 clone 量 ≤ `WINDOW_DEFAULT_LEN`。
    pub fn window_turns(&self) -> &[TurnView] {
        &self.turns[self.window_start..]
    }

    /// 窗口内回合数（= list_view 行数）。
    pub fn window_len(&self) -> usize {
        self.turns.len() - self.window_start
    }

    /// 当前变更版本号（见 [`Self::rev`] 注释）。
    pub fn mutation_rev(&self) -> u64 {
        self.rev
    }

    /// 向前扩展窗口（预加载更早回合）：起点前移 `by`，钳制到 0。
    /// 返回实际前移量；0 = 已全量放行（调用方短路，避免无谓渲染）。
    /// 扩展后窗口脱离「跟随尾部」模式（用户上滚浏览中）。
    pub fn expand_window(&mut self, by: usize) -> usize {
        let moved = self.window_start.min(by);
        if moved > 0 {
            self.window_start -= moved;
            self.tail_following = false;
            self.bump_rev();
        }
        moved
    }

    /// 是否已全量放行（窗口覆盖全部 turns）。
    pub fn window_full(&self) -> bool {
        self.window_start == 0
    }

    /// 窗口是否处于「跟随尾部」模式（调用方据此决定新回合到达时是否
    /// 调用 [`Self::slide_window_tail`]）。
    pub fn tail_following(&self) -> bool {
        self.tail_following
    }

    /// 窗口滑向末尾（跟随尾部语义）：起点右移，保持窗口大小为
    /// `WINDOW_DEFAULT_LEN`，并恢复「跟随尾部」模式。由调用方在「新
    /// turn 到达且本帧跟随尾部」时显式调用——用户上滚浏览时**不要**
    /// 调用（窗口保持，避免视口跳动）。
    ///
    /// 同时驱逐最早期的 turn（超出 [`MAX_RETAINED_BEFORE_WINDOW`] 缓冲），
    /// 以限制长对话的内存占用。
    pub fn slide_window_tail(&mut self) {
        let old_start = self.window_start;
        let old_len = self.turns.len();
        let old_following = self.tail_following;
        let keep = WINDOW_DEFAULT_LEN;
        if self.turns.len() > keep {
            self.window_start = self.window_start.max(self.turns.len() - keep);
        }
        self.tail_following = true;
        self.evict_old_turns();
        if self.window_start != old_start
            || self.turns.len() != old_len
            || self.tail_following != old_following
        {
            self.bump_rev();
        }
    }

    /// 驱逐渲染窗口前超出缓冲的最旧 turn，释放其内存。
    ///
    /// 保留 `MAX_RETAINED_BEFORE_WINDOW` 个 turn 在窗口前作为上滚预加载缓冲；
    /// 超出部分从 `turns` 头部移除，同时重建 `turn_index` 并校正 `window_start`。
    /// 用户上滚超出缓冲时由 `spawn_fetch_earlier`（服务端分页）兜底。
    fn evict_old_turns(&mut self) {
        let keep_before = MAX_RETAINED_BEFORE_WINDOW.min(self.window_start);
        let evict = self.window_start - keep_before;
        if evict == 0 {
            return;
        }
        self.turns.drain(..evict);
        self.window_start -= evict;
        self.turn_index = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t.turn_id.clone(), i))
            .collect();
    }

    /// 裁剪到仅保留渲染窗口内的 turn（会话缓存用，降低 clone 体积）。
    /// 返回后 `turns` 只含 `window_turns()` 的内容，`window_start` 归零。
    /// 被裁剪的旧 turn 在切回时由 daemon 权威快照恢复。
    pub fn trim_to_window(&mut self) {
        if self.window_start == 0 {
            return;
        }
        self.turns.drain(..self.window_start);
        self.window_start = 0;
        self.turn_index = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t.turn_id.clone(), i))
            .collect();
        self.bump_rev();
    }

    /// 前插一页更早的回合（分页加载：resume 只取尾部页，上滚翻页把更早
    /// 的页插到最前）。已存在的 turn_id 跳过（页码边界可能重叠）；窗口
    /// 起点右移 `n` 保持渲染窗口位置（新回合在窗口**前面**，chat_view
    /// 以 `n` 做锚定补偿，视口不跳）。返回实际前插数；0 = 无新回合。
    pub fn prepend_turns(&mut self, turns: Vec<RestoredTurn>) -> usize {
        let known: HashSet<&str> = self.turns.iter().map(|t| t.turn_id.as_str()).collect();
        let fresh: Vec<RestoredTurn> = turns
            .into_iter()
            .filter(|t| !known.contains(t.turn_id.as_str()))
            .collect();
        if fresh.is_empty() {
            return 0;
        }
        let n = fresh.len();
        let mut new_turns: Vec<TurnView> = fresh.into_iter().map(to_turn_view).collect();
        new_turns.append(&mut self.turns);
        self.turns = new_turns;
        self.turn_index = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t.turn_id.clone(), i))
            .collect();
        self.window_start += n;
        self.bump_rev();
        n
    }

    /// 快照恢复：用权威 turns（timeline 快照解析产物）整体替换当前状态。
    /// 历史回合直接落 Final（不再流式）；此后增量事件照常 append。
    ///
    /// 幂等语义：快照是权威全量（daemon timeline 快照），增量事件在其后
    /// 到达；调用方在 seed 切换时应先重置（`Transcript::new`）再 restore。
    pub fn restore(&mut self, turns: Vec<RestoredTurn>) {
        let previous: HashMap<String, TurnView> = std::mem::take(&mut self.turns)
            .into_iter()
            .map(|turn| (turn.turn_id.clone(), turn))
            .collect();
        let mut restored = Vec::with_capacity(turns.len());
        for turn in turns {
            let turn_id = turn.turn_id.clone();
            let mut view = to_turn_view(turn);
            if let Some(old) = previous.get(&turn_id) {
                for round in &mut view.rounds {
                    if let Some(old_round) = old
                        .rounds
                        .iter()
                        .find(|old_round| old_round.round_num == round.round_num)
                    {
                        if same_rendered_round(old_round, round) {
                            *round = old_round.clone();
                        } else {
                            Rc::make_mut(round).mutation_rev =
                                old_round.mutation_rev.wrapping_add(1);
                        }
                    }
                }
                view.mutation_rev = if same_rendered_turn(old, &view) {
                    old.mutation_rev
                } else {
                    old.mutation_rev.wrapping_add(1)
                };
            }
            restored.push(view);
        }
        self.turns = restored;
        self.turn_index = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t.turn_id.clone(), i))
            .collect();
        // 窗口化：只渲染最近 N 个回合（长会话 restore 成本与总回合数解耦）。
        // 再叠加 **round 预算**：固定 30 turns 窗口在超大回合下仍会一次
        // mount 数千元素（实测单 turn 可达 100+ rounds、40 turns 共 680
        // rounds / 1783 blocks → 切换标签秒级卡顿）。预算从尾部累计
        // rounds，超限即收缩窗口（保留最新回合，裁剪最旧）。
        let mut budget = RESTORE_ROUND_BUDGET;
        let mut start_budget = 0usize;
        for (i, t) in self.turns.iter().enumerate().rev() {
            if budget < t.rounds.len().max(1) {
                // 当前 turn 超预算：保留它（含 i），但不再向前扩展。
                start_budget = i;
                break;
            }
            budget -= t.rounds.len().max(1);
            start_budget = i;
        }
        self.window_start = start_budget.max(self.turns.len().saturating_sub(WINDOW_DEFAULT_LEN));
        self.tail_following = true;
        // 立即驱逐窗口前超出缓冲的最旧 turn：restore 是全量权威快照，
        // 但内存只保留可立即浏览的部分（窗口 + 上滚缓冲），更早历史
        // 由 daemon 分页按需回放（on_top_reached → spawn_fetch_earlier）。
        // 释放 9MB 级长会话的解析产物驻留，避免切换标签内存台阶。
        self.evict_old_turns();
        self.bump_rev();
    }

    /// Apply one protocol event to the canonical presentation model.
    pub fn apply(&mut self, ev: &ConversationEvent) -> TranscriptChange {
        let target_turn_id = (!ev.turn_id().is_empty()).then(|| ev.turn_id().to_owned());
        let target_round_num = ev.round_num();
        let change = match ev {
            ConversationEvent::TurnStarted { turn_id, user_text } => {
                if let Some(&index) = self.turn_index.get(turn_id) {
                    let turn = &mut self.turns[index];
                    if turn.user_text == *user_text {
                        return TranscriptChange::default();
                    }
                    turn.user_text.clone_from(user_text);
                    self.bump_turn(index);
                    return TranscriptChange::structural(true);
                }
                let index = self.turns.len();
                self.turns.push(TurnView {
                    turn_id: turn_id.clone(),
                    user_text: user_text.clone(),
                    status: TurnStatus::Running,
                    failed_error: None,
                    rounds: Vec::new(),
                    mutation_rev: 0,
                });
                self.turn_index.insert(turn_id.clone(), index);
                TranscriptChange::structural(true)
            }
            ConversationEvent::TurnCompleted { turn_id } => {
                let Some(&index) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                if self.turns[index].status == TurnStatus::Completed {
                    return TranscriptChange::default();
                }
                self.turns[index].status = TurnStatus::Completed;
                TranscriptChange::structural(false)
            }
            ConversationEvent::TurnFailed { turn_id, error } => {
                let Some(&index) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                if self.turns[index].status == TurnStatus::Failed {
                    return TranscriptChange::default();
                }
                self.turns[index].status = TurnStatus::Failed;
                self.turns[index].failed_error = Some(extract_failed_error(&error));
                // Best-effort：若流式内容已完整输出（如 [DONE] 后断连误判），
                // 把 Live 答案冻结为 Final 富文本，避免"永远 Live"状态残留。
                let mut changed = false;
                for round in &mut self.turns[index].rounds {
                    let round = Rc::make_mut(round);
                    if let AnswerView::Streaming { raw, .. } = &round.answer
                        && !raw.is_empty()
                    {
                        let raw_text = raw.clone();
                        if round.finalize(None, Some(&raw_text)) {
                            round.mutation_rev = round.mutation_rev.wrapping_add(1);
                            changed = true;
                        }
                    }
                }
                TranscriptChange::structural(changed)
            }
            ConversationEvent::Unknown => TranscriptChange::default(),
            ConversationEvent::RoundDelta {
                turn_id,
                round_num,
                kind,
                delta,
            } => {
                let Some(&turn) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                let (_, round) = self.round_mut(turn, *round_num);
                match kind {
                    RoundDeltaKind::Answering => round
                        .answer_delta(delta)
                        .then(|| TranscriptChange::live(true))
                        .unwrap_or_default(),
                    RoundDeltaKind::Thinking => {
                        if delta.is_empty() {
                            return TranscriptChange::default();
                        }
                        let t = round.thinking.get_or_insert_with(String::new);
                        t.push_str(delta);
                        TranscriptChange::live(true)
                    }
                    RoundDeltaKind::ToolCalling => round
                        .tool_delta(delta)
                        .then(|| TranscriptChange::structural(true))
                        .unwrap_or_default(),
                }
            }
            ConversationEvent::BlockCheckpoint {
                turn_id,
                round_num,
                kind,
                text,
            } => {
                let Some(&turn) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                let (_, round) = self.round_mut(turn, *round_num);
                match kind {
                    RoundDeltaKind::Answering => round
                        .answer_checkpoint(text)
                        .then(|| TranscriptChange::live(true))
                        .unwrap_or_default(),
                    RoundDeltaKind::Thinking => {
                        if round.thinking.as_deref() == Some(text) {
                            return TranscriptChange::default();
                        }
                        round.thinking = Some(text.clone());
                        TranscriptChange::live(true)
                    }
                    RoundDeltaKind::ToolCalling => round
                        .tool_checkpoint(text)
                        .then(|| TranscriptChange::structural(true))
                        .unwrap_or_default(),
                }
            }
            ConversationEvent::ProviderToolStatus {
                turn_id,
                round_num,
                call_id,
                tool_kind,
                state,
            } => {
                let Some(&turn) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                let (_, round) = self.round_mut(turn, *round_num);
                // provider 内建工具卡：无参数流，展开区显示执行状态。
                let label = match state {
                    ProviderToolState::InProgress => "进行中…".to_string(),
                    ProviderToolState::Searching => "搜索中…".to_string(),
                    ProviderToolState::Completed => String::new(),
                };
                let card = ToolCardView {
                    id: call_id.clone(),
                    name: Some(tool_kind.clone()),
                    args_display: label,
                    args_json: None,
                    body: ToolBody::Empty,
                    changes: None,
                    done: *state == ProviderToolState::Completed,
                    failed: false,
                    failure: None,
                    provider: true,
                    started: true,
                };
                // upsert by call_id（replaceable 语义：同 id 覆盖状态）。
                upsert_tool_card(round, card)
                    .then(|| TranscriptChange::structural(true))
                    .unwrap_or_default()
            }
            ConversationEvent::ToolCallPrepared {
                tool_call_id,
                turn_id,
                round_num,
                name,
                args_so_far,
            } => {
                let turn = self.ensure_turn(turn_id);
                let (_, round) = self.round_mut(turn, *round_num);
                let card = ToolCardView {
                    id: tool_call_id.clone(),
                    name: Some(name.clone()),
                    args_display: args_so_far.clone(),
                    args_json: Some(args_so_far.clone()),
                    body: if args_so_far.trim().is_empty() {
                        ToolBody::Empty
                    } else {
                        ToolBody::Text(args_so_far.clone())
                    },
                    changes: None,
                    done: false,
                    failed: false,
                    failure: None,
                    provider: false,
                    // Prepared 预览：尚未真正执行，前端不渲染。
                    started: false,
                };
                upsert_tool_card(round, card)
                    .then(|| TranscriptChange::structural(true))
                    .unwrap_or_default()
            }
            ConversationEvent::ToolStarted {
                tool_call_id,
                turn_id,
                round_num,
                name,
            } => {
                let turn = self.ensure_turn(turn_id);
                let (_, round) = self.round_mut(turn, *round_num);
                let existing = round
                    .tool_calls
                    .iter()
                    .find(|card| card.id == *tool_call_id)
                    .cloned();
                let card = ToolCardView {
                    id: tool_call_id.clone(),
                    name: Some(name.clone()),
                    args_display: existing
                        .as_ref()
                        .map(|card| card.args_display.clone())
                        .unwrap_or_default(),
                    args_json: existing.as_ref().and_then(|card| card.args_json.clone()),
                    body: existing
                        .as_ref()
                        .map(|card| card.body.clone())
                        .unwrap_or_default(),
                    changes: existing.and_then(|card| card.changes),
                    done: false,
                    failed: false,
                    failure: None,
                    provider: false,
                    started: true,
                };
                upsert_tool_card(round, card)
                    .then(|| TranscriptChange::structural(true))
                    .unwrap_or_default()
            }
            ConversationEvent::ToolFinished {
                tool_call_id,
                turn_id,
                round_num,
                result,
            } => {
                let turn = self.ensure_turn(turn_id);
                let (_, round) = self.round_mut(turn, *round_num);
                // 结果摘要（对齐 timeline 块 summary）；失败保留 error 摘要。
                let summary = result
                    .get("summary")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        result.get("error").and_then(|e| {
                            e.get("message")
                                .and_then(|m| m.as_str())
                                .map(str::to_string)
                        })
                    })
                    .unwrap_or_default();
                let existing = round
                    .tool_calls
                    .iter()
                    .find(|card| card.id == *tool_call_id)
                    .cloned();
                let name = existing.as_ref().and_then(|card| card.name.clone());
                let args_json = existing.as_ref().and_then(|card| card.args_json.clone());
                let candidate_body = name
                    .as_deref()
                    .map(|name| tool_body_from_result(name, args_json.as_deref(), result))
                    .unwrap_or_default();
                let body = match (candidate_body, existing.as_ref().map(|card| &card.body)) {
                    (ToolBody::Empty, Some(existing_body)) => existing_body.clone(),
                    (
                        ToolBody::Text(_),
                        Some(existing_body @ (ToolBody::Code(_) | ToolBody::Diff(_))),
                    ) => existing_body.clone(),
                    (candidate_body, _) => candidate_body,
                };
                let changes = change_stats_from_result(result, &body)
                    .or_else(|| existing.as_ref().and_then(|card| card.changes.clone()));
                let card = ToolCardView {
                    id: tool_call_id.clone(),
                    name,
                    args_display: summary,
                    args_json,
                    body,
                    changes,
                    done: true,
                    failed: false,
                    failure: None,
                    provider: false,
                    started: true,
                };
                upsert_tool_card(round, card)
                    .then(|| TranscriptChange::structural(true))
                    .unwrap_or_default()
            }
            ConversationEvent::CodeChanged {
                tool_call_id,
                turn_id,
                round_num,
                lines_added,
                lines_removed,
                files_created,
                files_deleted,
                file,
            } => {
                let turn = self.ensure_turn(turn_id);
                let (_, round) = self.round_mut(turn, *round_num);
                let Some(card) = round
                    .tool_calls
                    .iter_mut()
                    .find(|card| card.id == *tool_call_id)
                else {
                    return TranscriptChange::default();
                };
                let changes = ChangeStats {
                    lines_added: *lines_added,
                    lines_removed: *lines_removed,
                    files_created: *files_created,
                    files_deleted: *files_deleted,
                    file: file.clone(),
                };
                if card.changes.as_ref() == Some(&changes) {
                    TranscriptChange::default()
                } else {
                    card.changes = Some(changes);
                    TranscriptChange::structural(true)
                }
            }
            ConversationEvent::RoundCompleted {
                turn_id,
                round_num,
                thinking,
                answer,
                output_ref,
                is_final: _,
            } => {
                let Some(&turn) = self.turn_index.get(turn_id) else {
                    return TranscriptChange::default();
                };
                let (_, round) = self.round_mut(turn, *round_num);
                let mut changed = false;
                if let Some(t) = thinking {
                    if round.thinking.as_ref() != Some(t) {
                        round.thinking = Some(t.clone());
                        changed = true;
                    }
                }
                changed |= round.finish_tool_cards();

                // External content remains a model state. The app drains the
                // request and resolves it asynchronously through qaqh-client.
                if let Some(ref_uri) = output_ref
                    && answer.is_none()
                {
                    if round.output_ref.as_ref() == Some(ref_uri)
                        && (round.output_loading
                            || matches!(round.answer, AnswerView::Final { .. }))
                    {
                        let change = changed
                            .then(|| TranscriptChange::structural(true))
                            .unwrap_or_default();
                        if change.changed() {
                            self.bump_round(turn, *round_num);
                        }
                        return change;
                    }
                    round.output_ref = Some(ref_uri.clone());
                    round.output_loading = true;
                    round.output_error = None;
                    self.pending_outputs.push(PendingOutput {
                        turn_id: turn_id.clone(),
                        round_num: *round_num,
                        reference: ref_uri.clone(),
                    });
                    self.bump_round(turn, *round_num);
                    return TranscriptChange::structural(true);
                }
                changed |= round.finalize(None, answer.as_deref());
                changed
                    .then(|| TranscriptChange::structural(true))
                    .unwrap_or_default()
            }
        };
        if change.changed() {
            if let Some(turn_id) = target_turn_id
                && let Some(&index) = self.turn_index.get(&turn_id)
            {
                if let Some(round_num) = target_round_num {
                    self.bump_round(index, round_num);
                } else {
                    self.bump_turn(index);
                }
            } else {
                self.bump_rev();
            }
        }
        change
    }

    /// Apply a presentation-frame batch.
    ///
    /// Adjacent deltas for the same `(turn, round, kind)` are concatenated
    /// before parsing. A burst of token events therefore updates the live
    /// RichText/TextBlock tail once per dispatcher frame instead of repeatedly
    /// reparsing the growing answer on the UI thread.
    pub fn apply_frame(
        &mut self,
        events: impl IntoIterator<Item = ConversationEvent>,
    ) -> TranscriptChange {
        let mut update = TranscriptChange::default();
        for event in coalesce_adjacent_deltas(events) {
            update.merge(self.apply(&event));
        }
        update
    }

    /// Apply one already-coalesced presentation event.
    ///
    /// The app frame pump uses this when enforcing a wall-clock reducer budget
    /// across a queued batch. It intentionally skips the temporary Vec and
    /// coalescing pass because the app-side deferred queue has already grouped it.
    pub fn apply_coalesced(&mut self, event: ConversationEvent) -> TranscriptChange {
        self.apply(&event)
    }

    /// Drain external content requests created while applying completion events.
    pub fn take_pending_outputs(&mut self) -> Vec<PendingOutput> {
        std::mem::take(&mut self.pending_outputs)
    }

    /// 外置正文拉取完成：以权威文本重建（对应 `output_ref` 加载路径）。
    pub fn resolve_output(
        &mut self,
        turn_id: &str,
        round_num: u32,
        text: &str,
    ) -> TranscriptChange {
        let Some(&turn) = self.turn_index.get(turn_id) else {
            return TranscriptChange::default();
        };
        let (_, round) = self.round_mut(turn, round_num);
        let change = round
            .finalize(None, Some(text))
            .then(|| TranscriptChange::structural(true))
            .unwrap_or_default();
        if change.changed() {
            self.bump_round(turn, round_num);
        }
        change
    }

    /// Mark external content resolution as failed while preserving the live
    /// preview. The UI can surface the failure instead of rendering blank text.
    pub fn fail_output(
        &mut self,
        turn_id: &str,
        round_num: u32,
        message: impl Into<String>,
    ) -> TranscriptChange {
        let Some(&turn) = self.turn_index.get(turn_id) else {
            return TranscriptChange::default();
        };
        let message = message.into();
        let (_, round) = self.round_mut(turn, round_num);
        if !round.output_loading && round.output_error.as_deref() == Some(message.as_str()) {
            return TranscriptChange::default();
        }
        round.output_loading = false;
        round.output_error = Some(message);
        self.bump_round(turn, round_num);
        TranscriptChange::structural(true)
    }

    fn bump_rev(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    fn bump_turn(&mut self, turn: usize) {
        self.turns[turn].mutation_rev = self.turns[turn].mutation_rev.wrapping_add(1);
        self.bump_rev();
    }

    fn bump_round(&mut self, turn: usize, round_num: u32) {
        if let Some(round) = self.turns[turn]
            .rounds
            .iter_mut()
            .find(|round| round.round_num == round_num)
        {
            let round = Rc::make_mut(round);
            round.mutation_rev = round.mutation_rev.wrapping_add(1);
        }
        self.bump_turn(turn);
    }

    fn round_mut(&mut self, turn: usize, round_num: u32) -> (usize, &mut RoundView) {
        let turn_view = &mut self.turns[turn];
        if let Some(r) = turn_view
            .rounds
            .iter()
            .position(|r| r.round_num == round_num)
        {
            (r, Rc::make_mut(&mut turn_view.rounds[r]))
        } else {
            turn_view.rounds.push(Rc::new(RoundView::new(round_num)));
            let idx = turn_view.rounds.len() - 1;
            (idx, Rc::make_mut(&mut turn_view.rounds[idx]))
        }
    }

    /// 定位 turn；Tool 频道事件可能先于 Conversation 频道的 TurnStarted 到达
    /// （双 SSE 频道无顺序保证），此时自动创建空 turn 兜底，避免工具卡丢失。
    fn ensure_turn(&mut self, turn_id: &str) -> usize {
        if let Some(&index) = self.turn_index.get(turn_id) {
            return index;
        }
        let index = self.turns.len();
        self.turns.push(TurnView {
            turn_id: turn_id.to_string(),
            user_text: String::new(),
            status: TurnStatus::Running,
            failed_error: None,
            rounds: Vec::new(),
            mutation_rev: 0,
        });
        self.turn_index.insert(turn_id.to_string(), index);
        index
    }
}
