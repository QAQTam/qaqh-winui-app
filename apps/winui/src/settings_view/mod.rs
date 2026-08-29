//! XAML 原生设置页（P2）— SettingsView 的壳侧承载（路线图 Phase 3 首块）。
//!
//! 数据源：`bridge.core().settings_snapshot()`——`config.load` + `skills.list_tools`
//! + `workspace.status` 合并投影（shell_store::parse_config_load 等）；
//! 500ms rev 比对轮询（同 skills_view 模式）；首次进入 `spawn_config_load(false)`
//! 兜底拉取。
//!
//! 状态模型（D-2 执行权原则，壳/daemon 为数据源）：
//!   - 表单字段为本地草稿（use_state），"保存"按钮一次性 `config.save` 全字段
//!     （camelCase，对齐协议 `save()`）；rev 变化且无未保存修改时刷新草稿；
//!   - lang / fontFamily 等经 `config.save` 提交；theme 本地即时应用且随
//!     `config.save` 持久化（2026-08 后端新增 `theme` 契约字段）；
//!     permissionLevel 经 `config.set_permission_level` 直连；
//!   - workspace 运行模式：`workspace.set_mode` 壳直连；backend.restart 未实现

use std::sync::Arc;
use std::time::Duration;

use windows_reactor::*;

use crate::bridge::{Bridge, SettingsProjection};
use crate::shell_store::SettingsSnapshot;

mod sections;
mod view;

pub use view::settings_view;

/// 快照轮询间隔（同 sidebar / skills_view）。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 分类定义（id + 中文标签 + Fluent Symbol，对齐 Web `categories()`）。
pub const CATEGORIES: [(&str, &str, Symbol); 9] = [
    ("models", "模型", Symbol::Library),
    ("api", "API 密钥", Symbol::Setting),
    ("context", "上下文", Symbol::Document),
    ("subagent", "子代理", Symbol::People),
    ("workspace", "工具套件", Symbol::AllApps),
    ("appearance", "外观", Symbol::Pictures),
    ("multimodal", "多模态", Symbol::Camera),
    ("advanced", "高级", Symbol::Repair),
    ("remote", "远端连接", Symbol::Globe),
];

/// effort 档位（对齐 Web EFFORT_LADDER）。
const EFFORT_LADDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// 工作区运行模式（对齐 Web workspace.mode 取值）。
const WORKSPACE_MODES: [&str; 3] = ["local", "wsl", "remote"];
/// 权限档位滑杆（UAC 安全设置范式）：标题 + 描述，随滑杆实时切换。
const PERMISSION_LADDER: [(&str, &str); 4] = [
    ("Level 1 · 最大锁定", "每个工具调用都需要你确认，最安全。"),
    (
        "Level 2 · 读取自由",
        "工作区读取自动批准；写入、执行、网络操作需要确认。",
    ),
    (
        "Level 3 · 工作区自由",
        "工作区内操作自动批准；跨工作区写入需要一次性文件夹信任。",
    ),
    ("Level 4 · 不受限", "无权限检查（默认）。谨慎使用。"),
];
/// 滑杆档位短标签（对齐滑杆刻度）。
const PERMISSION_TICKS: [&str; 4] = ["保守", "询问", "自动", "全自动"];

/// Windows 11 SettingsCard 语义行：标题/说明 + 右侧原生控件。
fn field_row(label: &str, control: Element) -> Element {
    qaqh_fluent::settings_card(label, "", control)
}

/// 分组卡片：原生 Expander 已全面停用（F-N15 §1.2 定案——Expander 模板
/// VSM → Binding 重连 → GetActivationFactory 80040111 冷路径崩溃，resume
/// 大会话必现；chat 工具块 Expander 实锤，设置页 Expander 未证清白一并
/// 摘除），退化为「section header + 平铺 vstack」。
fn expander_group(title: &str, rows: Vec<Element>) -> Element {
    let mut children = Vec::with_capacity(rows.len() + 1);
    children.push(qaqh_fluent::settings_section_header(title, ""));
    children.extend(rows);
    vstack(children)
        .spacing(qaqh_fluent::tokens::SPACE_2)
        .into()
}

/// 设置页分组缓冲：section 函数照常 `rows.push(...)`，组边界改调
/// [`GroupBuf::section`]；[`GroupBuf::finish`] 统一把每组包进原生
/// Expander 卡片（PLAN.md「Expander 卡片骨架」）。
///
/// 设计动机：9 个 section 函数体内几十处 `rows.push(...)` 零改动，
/// 只换签名与组边界行——原生化改动的 diff 面最小化。
pub(crate) struct GroupBuf {
    /// 当前组标题（None = 尚未开组，行直通输出）。
    title: Option<String>,
    group: Vec<Element>,
    out: Vec<Element>,
}

impl GroupBuf {
    pub(crate) fn new() -> Self {
        Self {
            title: None,
            group: Vec::new(),
            out: Vec::new(),
        }
    }

    /// 开启新组（组名成为 Expander header；上一个组就此封口）。
    pub(crate) fn section(&mut self, title: &str) {
        if let Some(t) = self.title.take() {
            let group = std::mem::take(&mut self.group);
            self.out.push(expander_group(&t, group));
        }
        self.title = Some(title.to_string());
    }

    /// 参数取具体 Element 而非 Into：原 Vec<Element>::push 的调用点
    /// 全部产出 Element（含大量 `.into()` 尾缀），泛型入参会让这些
    /// `.into()` 的目标类型无法推断（E0283）。
    pub(crate) fn push(&mut self, el: Element) {
        if self.title.is_some() {
            self.group.push(el);
        } else {
            self.out.push(el);
        }
    }

    /// 封口末组，输出全部行（组 = Expander 卡片）。
    pub(crate) fn finish(mut self) -> Vec<Element> {
        if let Some(t) = self.title.take() {
            let group = std::mem::take(&mut self.group);
            self.out.push(expander_group(&t, group));
        }
        self.out
    }
}

/// 设置页各分类区块共享的渲染上下文。
///
/// 由 `settings_view` 在创建完所有 hooks 后构造，只读传给各 section 函数；
/// section 内部不再调用 `use_state` / `use_ref`（保证 hooks 顺序稳定）。
pub(crate) struct SettingsCtx {
    pub(crate) bridge: Arc<Bridge>,
    pub(crate) draft: HookRef<SettingsSnapshot>,
    pub(crate) proj_draft: HookRef<SettingsProjection>,
    pub(crate) dirty: HookRef<bool>,
    /// 最近一次非零压缩阈值（开关关闭时保留，重开时恢复——P0 用户反馈：
    /// 关→开不应丢掉调好的 0.95 无条件回落 0.75）。由 view 轮询权威快照
    /// 与滑杆回调双路种子。
    pub(crate) compact_restore: HookRef<f64>,
    pub(crate) d: SettingsSnapshot,
    pub(crate) pd: SettingsProjection,
    pub(crate) set_diag_rev: SetState<u32>,
    pub(crate) set_perm_desc: SetState<u8>,
    pub(crate) set_export_path: SetState<Option<String>>,
    pub(crate) diag_rev: u32,
    pub(crate) perm_desc: u8,
    pub(crate) export_path: Option<String>,
    pub(crate) remote_url: String,
    pub(crate) remote_token: String,
    pub(crate) remote_status: String,
    pub(crate) set_remote_url: SetState<String>,
    pub(crate) set_remote_token: SetState<String>,
    pub(crate) set_remote_status: SetState<String>,
}
