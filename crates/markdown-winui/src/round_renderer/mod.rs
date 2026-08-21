//! Transcript 状态机：协议事件 → 声明式视图状态。
//!
//! 协议事件只负责增量更新 [`Transcript`]；UI 从 Transcript 声明
//! `Element` 树，稳定 key 与 `windows-reactor` 的 reconciler 负责把状态
//! 变化提交到 XAML 控件树。这里不维护第二套命令式控件 patch 协议。
//!
//! XAML 渲染模型对应：
//! ```text
//! ConversationTranscript (ScrollViewer, 跟随尾部 + 锚点补偿)
//! └─ StackPanel（append-only：新 turn 只 push 尾部）
//!    └─ TurnView
//!       ├─ 用户气泡（TextBlock）
//!       └─ RoundView × N
//!          ├─ Thinking  → Expander（摘要随流更新）
//!          ├─ Answer    → Streaming: 轻量 TextBlock（每帧替换 Inlines）
//!          │              Final:     RichTextBlock（parse_final 一次构建）
//!          └─ ToolCall  → ToolCard（upsert by tool_call_id）
//! ```
//!
//! 核心不变量（协议局域化，见设计讨论）：
//! 1. `RoundCompleted` 前只更新对应 round 的活尾；
//! 2. `RoundCompleted` 后答案冻结，迟到 delta 被忽略；
//! 3. 事件按 `(turn_id, round_num)` O(1) 定位，渲染窗口与完整历史分离；
//! 4. 返回值只描述本帧最低失效等级，不复制 RichText/工具卡等渲染载荷。

use std::collections::HashMap;
use std::rc::Rc;

use markdown_core::ast::{Block, Inline};
use markdown_core::gfm_live_table::GfmTableTracker;
use markdown_core::live_table::LiveTableTracker;

use crate::{ChangeStats, RichTextOutput, TableData, ToolBody};

mod answer;
mod coalesce;
mod round;
#[cfg(test)]
mod tests;
mod transcript;
mod util;

/// The smallest declarative render invalidation needed after a model update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TranscriptInvalidation {
    #[default]
    None,
    Live,
    Structural,
}

/// Compact result of applying one event or one presentation-frame batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptChange {
    pub invalidation: TranscriptInvalidation,
    /// Content mounted or changed height, so a near-tail viewport may need a
    /// post-layout follow request.
    pub extent_changed: bool,
}

impl TranscriptChange {
    pub(crate) const fn live(extent_changed: bool) -> Self {
        Self {
            invalidation: TranscriptInvalidation::Live,
            extent_changed,
        }
    }

    pub(crate) const fn structural(extent_changed: bool) -> Self {
        Self {
            invalidation: TranscriptInvalidation::Structural,
            extent_changed,
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.invalidation = self.invalidation.max(other.invalidation);
        self.extent_changed |= other.extent_changed;
    }

    pub fn changed(&self) -> bool {
        self.invalidation != TranscriptInvalidation::None
    }

    pub fn is_structural(&self) -> bool {
        self.invalidation == TranscriptInvalidation::Structural
    }
}

/// An external final answer requested by `RoundCompleted.output_ref`.
///
/// This is model-owned work, not a UI patch command. The application drains
/// these requests, resolves them through the Ringing content endpoint, then
/// calls [`Transcript::resolve_output`] or [`Transcript::fail_output`].
#[derive(Clone, Debug, PartialEq)]
pub struct PendingOutput {
    pub turn_id: String,
    pub round_num: u32,
    pub reference: serde_json::Value,
}

/// turn 生命周期（渲染用；对齐协议 `TurnStarted/TurnCompleted/TurnFailed`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TurnStatus {
    #[default]
    Running,
    Completed,
    Failed,
}

/// 工具卡视图（流式累积，id 稳定）。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCardView {
    pub id: String,
    /// 从 ToolCalling 流中提取的工具名（未解析出时为 None）。
    pub name: Option<String>,
    /// 参数 raw（原型简化：直接展示累积文本）；provider 卡为状态文案。
    pub args_display: String,
    /// Original structured arguments, retained so patch/read renderers can
    /// derive a useful body even when the terminal receipt is compact.
    pub args_json: Option<String>,
    /// Typed native presentation instead of one undifferentiated text blob.
    pub body: ToolBody,
    /// Optional file-change totals delivered by the code-change channel.
    pub changes: Option<ChangeStats>,
    /// true = 工具卡完成（后续 delta 不再更新）。
    pub done: bool,
    /// Failed 终态标记（渲染 ✕ 红色 + failure 摘要；头部按最差状态显示）。
    pub failed: bool,
    /// 失败摘要（"code: message"，TimelineTool.failure 透传）。
    pub failure: Option<String>,
    /// provider 内建工具卡（web_search 等，`provider_tool_status` 事件）：
    /// 无参数流，展开区显示执行状态（args_display 承载）。
    pub provider: bool,
    /// 工具是否已真正开始执行（ToolStarted / state=Running）。
    /// Prepared（LLM 刚吐出 tool_call 的 replaceable 预览）为 false——
    /// 前端在 started 前不渲染，避免「一收到调用就闪烁」。
    pub started: bool,
}

/// 恢复的回合（timeline 快照解析产物；`Transcript::restore` 消费）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoredRound {
    pub round_num: u32,
    pub thinking: Option<String>,
    /// 答案 markdown 原文（kind=text 块按 block_order 拼接）。
    pub answer: Option<String>,
    pub tool_calls: Vec<ToolCardView>,
}

/// 恢复的 turn（timeline 快照解析产物；`Transcript::restore` 消费）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestoredTurn {
    pub turn_id: String,
    /// 快照里的权威创建序（TimelineTurn.created_seq）；0 = 未知（旧数据），
    /// 排序时退化为 turn_id 数值兜底。
    pub created_seq: u64,
    pub user_text: String,
    pub status: TurnStatus,
    pub rounds: Vec<RestoredRound>,
}

/// 一个 turn 的视图状态（append-only 累积）。
#[derive(Clone, Debug, Default)]
pub struct TurnView {
    pub turn_id: String,
    pub user_text: String,
    pub status: TurnStatus,
    /// TurnFailed 的错误信息（`code: message`，含 retryable 提示），UI 显示用。
    pub failed_error: Option<String>,
    /// Round payloads use copy-on-write identity. Cloning a changed turn for the
    /// declarative tree therefore stays shallow; completed rounds keep the same
    /// `Rc` and are eligible for round-level memo skips.
    pub rounds: Vec<Rc<RoundView>>,
    /// 仅本 turn 可见内容变化时递增。ChatView 的窗口快照用它复用
    /// 未变化 turn 的 `Rc<TurnView>`，避免每个 token 都深拷贝整个窗口。
    pub mutation_rev: u64,
}

/// 一个 round 的视图状态。
#[derive(Clone, Debug, Default)]
pub struct RoundView {
    pub round_num: u32,
    pub thinking: Option<String>,
    pub answer: AnswerView,
    pub tool_calls: Vec<ToolCardView>,
    /// An external final answer is being resolved through the content service.
    pub output_loading: bool,
    /// Content resolution failed; the live preview remains visible and the
    /// error is rendered alongside it instead of silently producing a blank.
    pub output_error: Option<String>,
    /// Last external reference, retained to make replayed completion events
    /// idempotent after the content has already been resolved.
    output_ref: Option<serde_json::Value>,
    /// Authoritative final markdown used to make replayed completions cheap.
    final_raw: Option<String>,
    /// 正在累积的工具调用 raw（未完成的 ToolCalling 流）。
    tool_raw: String,
    /// 仅本 round 的可见内容变化时递增。声明树用它在同一 turn 内跳过
    /// 已完成的 rounds，只重建当前活尾或状态发生变化的工具卡。
    pub mutation_rev: u64,
}

/// 流式答案的可见内容片段（字面文本与表格交错；UI 按序渲染）。
#[derive(Clone, Debug, PartialEq)]
pub enum LiveSegment {
    /// 字面文本片段（表格外内容；未闭合语法照旧字面输出）。
    Text(String),
    /// 流式表格（网格渲染；未闭合表格含残行 partial 作为末行）。
    Table(TableData),
}

/// 答案视图状态机：流式预览 → 权威终态。
///
/// 表格跟踪器自持在 `Streaming` 内（协议表格 + GFM 表格），使状态机可被
/// round 模型（[`RoundView`]）与 block 模型（[`crate::block_transcript`]）
/// 共用——两个 reducer 对答案流的处理是同一份实现。
#[derive(Clone, Debug, PartialEq)]
pub enum AnswerView {
    /// 流式中：字面/表格交错序列（仅已闭合语法；未闭合字面输出；
    /// 协议表格渐进长出，残行逐字生长在网格末行）。
    Streaming {
        raw: String,
        inlines: Vec<Inline>,
        segments: Vec<LiveSegment>,
        table_tracker: LiveTableTracker,
        gfm_table_tracker: GfmTableTracker,
    },
    /// 权威终态：全量块（冻结，不再变化）。
    Final {
        blocks: Vec<Block>,
        rich: RichTextOutput,
    },
}

/// 会话级渲染状态机。
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    turns: Vec<TurnView>,
    /// turn_id → index（长会话 O(1) 寻址，不做全量扫描）。
    turn_index: HashMap<String, usize>,
    /// 渲染窗口起点（turns 下标）：`[window_start, turns.len())` 是实际
    /// 传给 list_view 的回合。restore 后只渲染最近 `WINDOW_DEFAULT_LEN`
    /// 个回合；向上滚动接近顶部时 `expand_window` 前移起点（预加载更早
    /// 回合）。窗口是**渲染投影**：`turns()`/`turn_count()` 仍返回全量，
    /// 增量事件与 key 稳定性不受影响。
    window_start: usize,
    /// 窗口是否处于「跟随尾部」模式：restore 后 true；用户上滚扩展
    /// （`expand_window`）置 false（窗口保持，避免浏览内容跳动）；
    /// `slide_window_tail` 恢复 true。
    tail_following: bool,
    /// External final answers waiting for the application transport to load.
    pending_outputs: Vec<PendingOutput>,
    /// 渲染投影版本号：仅当窗口结构或某个 turn 的可见内容真实变化时递增。
    /// ChatView 据此复用窗口快照；每个 TurnView 另有局部 mutation_rev，
    /// 使变化帧也能结构共享未变化行。协议 no-op 不再使缓存失效。
    rev: u64,
}

/// 默认渲染窗口大小（回合数）：restore 后只渲染最近 N 个回合，与总回合
/// 数解耦，restore/每帧 diff 成本恒定。经实机手感调优。
pub const WINDOW_DEFAULT_LEN: usize = 30;

/// restore 窗口的 round 预算：尾部累计 rounds 超过此值即收缩窗口
/// （超大回合会话：30 turns 可能含 600+ rounds / 1800+ blocks，一次
/// mount 数千 XAML 元素 → 切换标签秒级卡顿）。200 rounds ≈ 500 blocks，
/// debug 构建单次 mount 可接受；可经实机手感调优。
pub const RESTORE_ROUND_BUDGET: usize = 200;

/// 渲染窗口之前最多保留的 turn 数量（上滚预加载缓冲）。
/// 超出此限制的最旧 turn 在 `slide_window_tail` / `restore` 时被驱逐，
/// 以限制长对话的内存占用。用户上滚超出缓冲时由分页拉取兜底。
pub const MAX_RETAINED_BEFORE_WINDOW: usize = 100;

/// restore 时最多保留的 turn 数量（窗口 + 上滚缓冲）。
///
/// `chat_adapter::restored_turns` 在**解析前**只取尾部这么多 turn，
/// 窗口外的历史由 daemon 分页（`spawn_fetch_earlier`）按需回放——
/// 对齐 Reasonix history-slice 的窗口化思路：内存只驻留可立即浏览的
/// 部分，更早内容按页从后端申请。9MB 快照 + 数百 turn 的会话不再
/// 全量解析驻留（restore 秒级 → 毫秒级，内存降低一个量级）。
pub const RESTORE_KEEP_TURNS: usize = WINDOW_DEFAULT_LEN + MAX_RETAINED_BEFORE_WINDOW;
