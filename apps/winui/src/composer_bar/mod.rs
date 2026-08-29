//! XAML 原生 Composer 底部栏（P6 输入框迁移块）— Web `ComposerDock` 的壳侧承载。
//!
//! 布局（挂载于 main.rs right_content row0 内层 grid row1，chat 视图可见；
//! 2026-08 精简后单卡两区，见 docs/nextdev/composer-streamline.md 批次 A）：
//! ```text
//! ├ queue 行（"n 条后续任务已排队" + 列表 + 删除）              │  ← 可选
//! ├ slash 菜单（composer 上方覆盖层 cell）                      │  ← 可选
//! └ 悬浮卡（elevated_command_surface：LayerFill 圆角 8 + elevation 16）
//!   ├ 顶部条：拖拽 grip（横向居中）+ ⤢ 沉浸式（A6 右端）         │
//!   ├ TextBox（多行无自绘边框，直接坐进卡；Enter 发送）          │
//!   ├ submitError 行 / 附件预览行                               │
//!   ├ footer：附件 | 工具模式 | 模式 chip | [工作目录空态入口] | 权限 | 发送
//!   └ 卡底 2px 贴边 token 分布线 + 右端短计数 caption（A3）      │
//! ```
//! 工作目录 chip（A2）：已选目录后显示于标题栏（header.rs footer），
//! 卡上仅保留未选目录时的一次性入口。
//!
//! 数据源：`bridge.core().composer_snapshot()`（Web `shell.setComposer`
//! 投影，250ms rev 轮询；goal 功能冻结后 `dashboard_snapshot()` 不再
//! 消费，bridge 通道保留待复活）。`sendAck` 变化 → 清空草稿（悲观清空）；`seed` 变化 → 重置
//! 草稿（会话切换）。
//!
//! 草稿态（text/附件/slash 状态）为纯 UI 态：`use_ref` 真实存储 +
//! `use_state<u64>` 版本号触发重渲染（SetState 无 get，回调从 ref 读写，
//! UI 线程单线程安全）。提交时经 bridge 直连 action 进协议——
//! 每字符零同步（IME 原生），状态单源在 bridge 本地缓存 + daemon 协议。
//!
//! 复刻偏差（对齐项目既有偏差记录风格）：
//! - textarea 62→180px 自动高度 → TextBox 固定高（72px + 滚动）
//! - 毛玻璃 backdrop-filter → LayerFill + 圆角（壳内统一，同 info_panel）
//! - 附件图片预览（object URL）→ 一期仅文件名 + 大小（阶段 B 临时文件）
//! - 附件菜单 → 原生 MenuFlyout；slash 菜单仍在卡片上方 cell
//! - Enter 发送走 KeyboardAccelerator（reactor 无 KeyDown；Shift+Enter
//!   因带修饰键不匹配 accelerator → TextBox 默认换行保留）

use std::sync::atomic::AtomicU64;
use std::time::Duration;

use qaqh_types::tool_mode::{CUSTOM, MINIMAL, MINIMAL_B, MINIMAL_C, STANDARD};

/// 快照轮询间隔（同 interaction_overlay：交互响应优先）。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
// A1 去卡中卡：输入区直接坐进卡片（Fluent 2 输入基准），空态 56px。
const INPUT_MIN_HEIGHT: f64 = 48.0;
const INPUT_DEFAULT_HEIGHT: f64 = 56.0;
const INPUT_AUTO_MAX_HEIGHT: f64 = 180.0;
const INPUT_MANUAL_MAX_HEIGHT: f64 = 360.0;

/// 诊断日志（统一落 `log/composer/`，见 [`crate::app_log`] 模块文档）。
fn log_diag(msg: &str) {
    crate::app_log::write("composer", msg);
}

/// 附件 id 计数器（同 Web makeImageId/makeTextId 的进程内唯一语义）。
static ATT_ID: AtomicU64 = AtomicU64::new(0);

/// 工具模式五选一（PLAN-TOOL-MODES.md；顺序对齐 daemon
/// standard/minimal/minimal:b/minimal:c/custom；minimal:dsh 已移除）。
const TOOL_MODE_OPTIONS: [&str; 5] = ["标准", "极限·8", "极限·6", "极限·4", "创造"];

/// 创造模式默认基底（D6：预设为基底 + 勾选增删；此处用全量已注册工具，
/// 避免从零勾选漏基础工具导致模型半瘫；精细勾选 UI 后续再做）。
const CUSTOM_MODE_DEFAULT_TOOLS: &[&str] = &[
    "bash",
    "pwsh",
    "exec",
    "write",
    "edit_file_v2",
    "read_file",
    "glob",
    "grep",
    "apply_patch",
    "copy_range",
    "delete",
    "confirm_apply",
    "todo",
    "ask",
    "web_fetch",
    "read_image",
    "process",
    "skills",
];

/// 工具模式字符串 → ComboBox 选中索引（空 = standard）。
fn tool_mode_index(mode: &str) -> i32 {
    match mode {
        MINIMAL => 1,
        MINIMAL_B => 2,
        MINIMAL_C => 3,
        CUSTOM => 4,
        _ => 0,
    }
}

/// ComboBox 选中索引 → 工具模式字符串（与 `tool_mode_index` 互逆）。
/// 未初始化/未知索引统一落到 standard，由调用方的空态 guard 拦截。
fn tool_mode_from_index(index: i32) -> &'static str {
    match index {
        1 => MINIMAL,
        2 => MINIMAL_B,
        3 => MINIMAL_C,
        4 => CUSTOM,
        _ => STANDARD,
    }
}

/// 权限四档菜单项（A4 语义 chip）：档位名 + 一句话说明（全量语义见
/// settings_view `PERMISSION_LADDER`）。`on_item_clicked` 回传整段文本，
/// 由 [`permission_menu_level`] 解析档位。
pub(crate) const PERMISSION_MENU: [(u64, &str); 4] = [
    (1, "L1 保守 · 每个工具调用都需确认"),
    (2, "L2 询问 · 读取自动批准，写入/执行/网络需确认"),
    (3, "L3 自动 · 工作区内操作自动批准"),
    (4, "L4 全自动 · 无权限检查（默认，谨慎）"),
];

/// 权限菜单文本 → 档位（精确匹配 [`PERMISSION_MENU`] 条目；未知返回 None）。
pub(crate) fn permission_menu_level(label: &str) -> Option<u64> {
    PERMISSION_MENU
        .iter()
        .find(|(_, text)| label == *text)
        .map(|(lvl, _)| *lvl)
}

/// 权限选择是否放行（Bug#2 守卫语义等价迁移，A4 控件形态变化）：
/// rendered==0（config 未加载/失败）→ 任何选择事件都跳过——旧 ComboBox 的
/// SelectionChanged 守卫；MenuFlyout 虽无程序化同步事件，仍保留同语义拦截。
/// 同值选择 → 无操作（对齐旧 ComboBox 的 `lvl != rendered_pl` 判定）。
pub(crate) fn permission_change_allowed(rendered_pl: u64, lvl: u64) -> bool {
    rendered_pl != 0 && lvl != rendered_pl
}

/// 工具模式 SelectionChanged 是否应放行（真实用户点击）而非跳过（程序化同步）。
/// - **非空渲染值**：仅当新值 != 渲染值时才可能是用户点击。渲染期设置
///   `selected_index` 会触发同值 SelectionChanged（同步事件），必须跳过。
/// - **空渲染值**（新会话 `meta.json` 的 `tool_mode` 为空，渲染为 standard(0)）：
///   index 0 是挂载/会话切换时的程序化同步事件，跳过；index != 0 必为用户点击
///   （空态下点"标准"不会产生 SelectionChanged，因当前已选中 0），放行。
///
/// BUG-017：旧守卫把"渲染值空"一律当作未初始化同步事件丢弃，导致新会话（meta
/// tool_mode 恒为空）的工具模式选择永久失效——选极限模式看起来选中、实际从未发送。
pub fn tool_mode_change_is_user(rendered_tm: &str, index: i32) -> bool {
    let next = tool_mode_from_index(index);
    next != rendered_tm && !(rendered_tm.is_empty() && index == 0)
}

// ── slash 命令（对齐 Web `slashCommands.ts` 常量表，纯展示）────────

const SLASH_COMMANDS: &[(&str, &str, &str)] = &[
    ("/settings", "设置", "打开应用设置"),
    ("/model", "模型", "切换对话模型"),
    ("/effort", "强度", "调整推理强度"),
    ("/usage", "用量", "查看用量详情（info 面板）"),
];

/// 匹配候选（对齐 Web `matchSlashCommands`：仅 "/" 开头时返回）。
fn match_slash_commands(value: &str) -> Vec<(String, String, String)> {
    let Some(rest) = value.strip_prefix('/') else {
        return Vec::new();
    };
    let query = rest.trim().to_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|(cmd, label, _)| {
            query.is_empty()
                || cmd
                    .strip_prefix('/')
                    .unwrap_or(cmd)
                    .to_lowercase()
                    .contains(&query)
                || label.to_lowercase().contains(&query)
        })
        .map(|(c, l, d)| (c.to_string(), l.to_string(), d.to_string()))
        .collect()
}

// ── 附件（本地草稿态）───────────────────────────────────────────

#[derive(Clone)]
pub(crate) enum AttachmentKind {
    Image { mime_type: String },
    Text,
}

#[derive(Clone)]
pub(crate) struct AttachmentItem {
    id: String,
    kind: AttachmentKind,
    file_name: String,
    size: u64,
    path: String,
    /// 图片缩略图临时文件路径（%TEMP%/qaqh-preview-*；渲染转 file:// URI，
    /// 移除/清空时删除；仅 Image 附件有值）。
    preview_path: Option<String>,
}

impl AttachmentItem {
    fn size_label(&self) -> String {
        format_size(self.size)
    }
}

/// 字节大小格式化（对齐 Web `formatSize`：B/KB/MB 一位小数）。
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// 千分位数字格式化（token 计数；对齐 info_panel fmt_thousands）。
fn fmt_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// token 计数短格式（A3 卡底 caption）：999 / 11.2K / 1.23M；全量计数走 tooltip。
fn fmt_tokens_short(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    }
}

/// 工作区 chip 显示标签：取路径末两段（长路径截断，完整路径走 tooltip）。
/// A2 起同时供标题栏工作区 chip 使用（header.rs）。
pub(crate) fn short_cwd(cwd: &str) -> String {
    let parts: Vec<&str> = cwd.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let n = parts.len();
    if n == 0 {
        return cwd.to_string();
    }
    let start = n.saturating_sub(2);
    let mut s = parts[start..].join("\\");
    if start > 0 {
        s = format!("…\\{s}");
    }
    s
}

/// 复制图片到 %TEMP% 做预览源（WinUI Image 不支持 base64，用 file:// 加载）。
/// 返回临时文件路径；失败返回 None（预览降级为仅文件名，不影响发送）。
fn write_preview_copy(src: &str, id: &str) -> Option<String> {
    let ext = std::path::Path::new(src)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    let tmp = std::env::temp_dir().join(format!("qaqh-preview-{id}.{ext}"));
    let tmp_str = tmp.to_string_lossy().to_string();
    std::fs::copy(src, &tmp).ok()?;
    Some(tmp_str)
}

/// 删除预览临时文件（移除附件 / 清空草稿时调用；失败静默，%TEMP% 系统可清）。
fn remove_preview(preview_path: Option<&str>) {
    if let Some(p) = preview_path {
        let _ = std::fs::remove_file(p);
    }
}

/// 草稿态（纯 UI，不进协议；提交时组装载荷）。
#[derive(Clone, Default)]
pub(crate) struct Draft {
    pub(crate) text: String,
    attachments: Vec<AttachmentItem>,
    selected_slash: usize,
    dismissed_slash: Option<String>,
}

impl Draft {
    /// 测试/桥接构造（仅文本；其余默认）。
    pub(crate) fn with_text(text: String) -> Self {
        Self {
            text,
            ..Default::default()
        }
    }
}

/// XAML Composer 底部栏（chat 视图；main.rs 内层 grid row1 挂载）。
pub use view::composer_bar;
mod status;
mod view;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_mode_index_maps_known_modes() {
        assert_eq!(tool_mode_index(STANDARD), 0);
        assert_eq!(tool_mode_index(MINIMAL), 1);
        assert_eq!(tool_mode_index(MINIMAL_B), 2);
        assert_eq!(tool_mode_index(MINIMAL_C), 3);
        assert_eq!(tool_mode_index(CUSTOM), 4);
    }

    #[test]
    fn tool_mode_from_index_roundtrips_known_modes() {
        let modes = [
            (STANDARD, 0),
            (MINIMAL, 1),
            (MINIMAL_B, 2),
            (MINIMAL_C, 3),
            (CUSTOM, 4),
        ];
        for (mode, index) in modes {
            assert_eq!(tool_mode_index(mode), index);
            assert_eq!(tool_mode_from_index(index), mode);
        }
    }

    #[test]
    fn unknown_mode_and_index_fall_back_to_standard() {
        assert_eq!(tool_mode_index("minimal:future"), 0);
        assert_eq!(tool_mode_index(""), 0);
        assert_eq!(tool_mode_from_index(-1), STANDARD);
        assert_eq!(tool_mode_from_index(99), STANDARD);
    }

    #[test]
    fn empty_rendered_mode_accepts_user_clicks_but_skips_sync() {
        // 空态（新会话 meta.tool_mode 为空）：index 0 是挂载/会话切换的
        // 程序化同步事件，跳过；index 1..=4 必为用户点击，放行（BUG-017）。
        assert!(!tool_mode_change_is_user("", 0));
        assert!(tool_mode_change_is_user("", 1)); // 极限·8
        assert!(tool_mode_change_is_user("", 2)); // 极限·6
        assert!(tool_mode_change_is_user("", 3)); // 极限·4
        assert!(tool_mode_change_is_user("", 4)); // 创造
    }

    #[test]
    fn non_empty_rendered_mode_skips_same_value_sync() {
        // 非空渲染值：同值 SelectionChanged 是渲染期同步，跳过。
        for (mode, index) in [
            (STANDARD, 0),
            (MINIMAL, 1),
            (MINIMAL_B, 2),
            (MINIMAL_C, 3),
            (CUSTOM, 4),
        ] {
            assert!(!tool_mode_change_is_user(mode, index), "{mode}@{index}");
        }
        // 用户切到不同档位：放行。
        assert!(tool_mode_change_is_user(STANDARD, 1));
        assert!(tool_mode_change_is_user(MINIMAL, 2));
        assert!(tool_mode_change_is_user(MINIMAL_B, 4));
        assert!(tool_mode_change_is_user(CUSTOM, 0));
        assert!(tool_mode_change_is_user(CUSTOM, 3));
    }

    #[test]
    fn permission_menu_level_resolves_all_four_labels() {
        for (lvl, text) in PERMISSION_MENU {
            assert_eq!(permission_menu_level(text), Some(lvl), "{text}");
        }
        assert_eq!(permission_menu_level("上传图片"), None);
        assert_eq!(permission_menu_level(""), None);
        assert_eq!(permission_menu_level("L5 越权"), None);
    }

    #[test]
    fn permission_change_allowed_preserves_bug2_guard() {
        // Bug#2：rendered==0（config 未加载）一律跳过，任何档位都不写。
        for lvl in [1u64, 2, 3, 4] {
            assert!(!permission_change_allowed(0, lvl), "rendered=0 lvl={lvl}");
        }
        // 同值 = 无操作；跨档位放行。
        assert!(!permission_change_allowed(2, 2));
        assert!(permission_change_allowed(2, 3));
        assert!(permission_change_allowed(4, 1));
    }

    #[test]
    fn tokens_short_format_tiers() {
        assert_eq!(fmt_tokens_short(0), "0");
        assert_eq!(fmt_tokens_short(999), "999");
        assert_eq!(fmt_tokens_short(1_000), "1.0K");
        assert_eq!(fmt_tokens_short(11_168), "11.2K");
        assert_eq!(fmt_tokens_short(999_999), "1000.0K");
        assert_eq!(fmt_tokens_short(1_234_567), "1.23M");
    }

    #[test]
    fn a1_height_constants_meet_fluent2_baseline() {
        // A1 去卡中卡：空态 56（Fluent 2 输入基准），最小 48；上限不变。
        assert_eq!(INPUT_DEFAULT_HEIGHT, 56.0);
        assert_eq!(INPUT_MIN_HEIGHT, 48.0);
        assert_eq!(INPUT_AUTO_MAX_HEIGHT, 180.0);
        assert_eq!(INPUT_MANUAL_MAX_HEIGHT, 360.0);
        assert!(INPUT_MIN_HEIGHT < INPUT_DEFAULT_HEIGHT);
    }
}
