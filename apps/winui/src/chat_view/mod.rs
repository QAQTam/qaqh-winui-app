//! 原生 ChatView：conversation 事件直连 → `Transcript` → reactor 控件树。
//!
//! 数据源：`bridge.chat_drain()`——bridge 在 conversation 频道把 canonical
//! typed events 缓存入队，本组件以 16ms XAML 帧批次 drain，经
//! `chat_adapter::render_event` 映射后先合并同目标的相邻 delta，再喂
//! `Transcript` 状态机；紧凑的模型失效摘要决定是否声明新的 Element 树，
//! `windows-reactor` 负责 keyed diff 与 XAML 提交。
//!
//! 渲染模型（对齐 CHATVIEW-RENDERING-REFERENCE）：
//! - turn 壳：用户气泡 + 状态徽标；
//! - round：思考折叠区 + 工具卡 + 答案（live 字面/表格交错 → final 富文本）；
//! - 协议表格流式渐进（LiveSegment::Table 网格，残行逐字生长）。
//!
//! Mermaid 与代码高亮均走 Rust + 原生 XAML；数学公式仍按字面文本降级。

use std::time::Duration;

mod blocks;
mod cache;
mod tools;
mod turns;
mod view;
mod zoom;

pub use view::chat_view;
pub use zoom::diagram_zoom_overlay;

/// 图表放大请求（壳内 UI 状态）：`final_view` 点击图表卡片时写入，
/// `main.rs` 挂载的 [`diagram_zoom_overlay`] 轮询消费（与 interaction_overlay
/// 同款「写端组件 / 读端覆盖层」分离模式，避免跨组件 props 穿透）。
#[derive(Clone, Debug, PartialEq)]
pub struct DiagramZoomRequest {
    /// 图表定位标签（turn/round 索引，作覆盖层标题）。
    pub label: String,
    /// 当前主题下的 SVG（透明背景 + text→path 后的纯 path 版）。
    pub svg: String,
    /// SVG 自然尺寸（DIP，从 `<svg width height>` 解析；fit 缩放基准）。
    pub width: f64,
    pub height: f64,
}

/// 跟随尾部滚动请求节流：live 流式期间 100ms 一次贴底请求。vsync 泵每
/// 帧都请求会让滚动与用户滚轮/滚动条抢占，并形成"滚动 → 行 realize
/// → 渲染 → 再滚动"反馈循环，UI 线程满载（表现为滚动条卡死）；100ms
/// 是经实机验证的折中（原 50ms 在长文本流式下仍会触发卡顿）。结构性
/// 变化（restore / 新 turn / round 完成）不受此限，立即滚底。
const SCROLL_REQUEST_THROTTLE: Duration = Duration::from_millis(100);

/// Live（非结构性）正文提交间隔：8ms 下限，与 vsync 帧泵配合时在
/// 60Hz/120Hz 屏上都可逐帧提交。协议事件先归并到模型，再由一次
/// retained-mode 更新提交 growing markdown / Element / XAML 文本；结构变化
/// （新 turn、工具卡、round 完成）仍立即提交。滚动请求继续单独按 100ms
/// 节流，避免恢复“滚动 → realize → 渲染 → 滚动”的反馈环。
const LIVE_RENDER_INTERVAL: Duration = Duration::from_millis(8);

/// 单帧事件上限与 reducer 时间预算。数量上限限制极端微小事件，时间预算
/// 则限制长 checkpoint / markdown 解析；任一先到即把余量留给下一帧。
const CHAT_EVENTS_PER_FRAME: usize = 512;
const CHAT_REDUCER_BUDGET: Duration = Duration::from_millis(4);

/// BUG-F1（设置往返空白）：空快照核实轮数上限。空快照（n==0）不得直接
/// 作为「已恢复」凭据——单槽缓存里可能驻留会话创建时期的过期空快照，
/// 直接采信会覆盖真实内容并熔断重拉（last_restored_seed 置位后重拉
/// 永久停止）。先经此轮数主动重拉核实（1s 节流），仍为空才采信为真空
/// 会话并进入恢复终态。
const EMPTY_SNAPSHOT_VERIFY_MAX: u32 = 2;

/// transport drain 闸：8ms 下限使 60Hz/120Hz 的 vsync 回调均可逐帧取数，
/// 避免原 32ms 批处理造成明显的成批吐字。单帧仍受 512 事件上限和 reducer
/// 4ms 墙钟预算保护；滚动与 XAML 提交还有各自独立预算。
const DRAIN_INTERVAL: Duration = Duration::from_millis(8);

/// 字符 shimmer 的相位步长。ProgressRing 由 WinUI 合成器独立动画，文本
/// 光带只需低频推进；按 vsync 每帧重建 RichText 会既过快又浪费 UI 线程。
const SHIMMER_STEP_INTERVAL: Duration = Duration::from_millis(90);

/// 后台时长超过此值（窗口不可见，vsync 回调暂停）→ 恢复时走快照
/// resume：丢弃积压增量事件，拉 timeline 快照一次到位（复用 restore
/// 分支，毫秒级），而不是重放后台期间积压的增量（可能数千条）。
const BACKGROUND_RESUME_AFTER: Duration = Duration::from_secs(30);

/// 显式跟随状态机：Following = 新内容贴底；Idle = 用户上滚离开
/// （浮层"回到最新"按钮出现；点击 → force_tail 回底 + 重新跟随）。
/// 进入跟随必须是显式动作（点击 / 新会话 / restore 到尾部），不做
/// 每帧隐式判定——用户完全掌控"我在底部"的意图。
#[derive(Clone, Copy, PartialEq, Debug)]
enum FollowState {
    Following,
    Idle,
}

/// 顶部预加载分页大小：滚动接近窗口顶部时一次扩展的回合数。
/// 与 `markdown_winui::WINDOW_DEFAULT_LEN` 同量级，可经实机手感调优。
const WINDOW_PAGE: usize = 30;

/// near-top 判定阈值（DIPs）：滚动到距列表顶部此距离内触发预加载。
/// 与 reactor 贴底阈值（120px）对称。
const NEAR_TOP_THRESHOLD_PX: f64 = 120.0;
