//! 全局导航数据源 + pane 偏好持久化 + 历史会话弹层。
//!
//! 布局重构（2026-08-25 第二次迭代）：左栏回归**原生 NavigationView 控件**
//! （main.rs 根壳，动效/折叠/回弹全套系统行为），本文件只保留：
//! - [`build_nav_items`]：菜单项构造（主模式 / Settings 模式原地分支）
//! - pane 折叠状态持久化（`%LOCALAPPDATA%\QAQ-Harness\ui_prefs.json`）
//! - [`history_dialog`]：历史会话弹层（原会话列表的 resume 能力承载）
//! - [`state_dot`]：会话状态圆点（session_tabs 复用）
//!
//! 历史教训（同日事故）：会话切换**只能**走 [`Bridge::spawn_resume`]——
//! 它自带 attach → set_active_seed → 三道 generation 检查 → 导航的完整
//! 铲链；额外补发无 seed 的 navigate 会以旧 active_seed 渲染 chat，
//! 切换完成前发消息会发进错误会话。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use windows_reactor::*;

use crate::bridge::Bridge;
use crate::settings_view::CATEGORIES;
use crate::shell_store::{ActivityState, SessionItem};
use qaqh_fluent::tokens;

/// 历史列表轮询间隔（与会话标签同源节奏）。
const HISTORY_POLL_INTERVAL: Duration = Duration::from_millis(500);

// ── pane 折叠状态持久化 ─────────────────────────────────────────────────

fn prefs_file() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA").ok().map(|base| {
        PathBuf::from(base)
            .join("QAQ-Harness")
            .join("ui_prefs.json")
    })
}

/// 读取上次 pane 展开状态（文件缺失/损坏静默回退展开态）。
pub fn load_pane_open() -> bool {
    prefs_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("pane_open").and_then(|b| b.as_bool()))
        .unwrap_or(true)
}

/// 写 pane 展开状态（失败静默——偏好丢失可接受，不阻塞 UI）。
pub fn store_pane_open(open: bool) {
    if let Some(p) = prefs_file() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, format!("{{\"pane_open\":{open}}}"));
    }
}

// ── 状态圆点（session_tabs 复用）────────────────────────────────────────

/// 会话活动状态 → Fluent 语义色令牌
pub(crate) fn state_color(state: ActivityState) -> ThemeRef {
    match state {
        ActivityState::Working => ThemeRef::SystemSuccess,
        ActivityState::WaitingUser => ThemeRef::Accent,
        ActivityState::Starting => ThemeRef::SystemAttention,
        ActivityState::Disconnected => ThemeRef::SystemCritical,
        ActivityState::Idle => ThemeRef::SystemNeutral,
    }
}

/// 状态圆点：8px 圆形（Border + 4px 圆角），供 session_tabs 复用
pub(crate) fn state_dot(state: ActivityState) -> Element {
    border(text_block(""))
        .width(8.0)
        .height(8.0)
        .corner_radius(4.0)
        .background(state_color(state))
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

// ── 菜单项构造 ──────────────────────────────────────────────────────────

/// NavigationView 菜单项（随模式原地分支，零嵌套）：
/// - 主模式：主页 / 聊天 / 技能 / 历史 / 设置（tag = 视图名，`history` 特殊）
/// - Settings 模式：九分类（tag = 分类 id）
pub fn build_nav_items(view: &str, _settings_category: &str) -> Vec<NavViewItem> {
    if view == "settings" {
        return CATEGORIES
            .iter()
            .map(|(id, label, symbol)| {
                NavViewItem::new(*label)
                    .tag(*id)
                    .icon(Icon::symbol(*symbol))
            })
            .collect();
    }
    let main_items: [(Symbol, &str, &str); 5] = [
        (Symbol::Home, "主页", "home"),
        (Symbol::Message, "聊天", "chat"),
        (Symbol::Library, "技能", "skills"),
        (Symbol::Clock, "历史", "history"),
        (Symbol::Setting, "设置", "settings"),
    ];
    main_items
        .iter()
        .map(|(symbol, label, tag)| {
            NavViewItem::new(*label)
                .tag(*tag)
                .icon(Icon::symbol(*symbol))
        })
        .collect()
}

// ── 历史会话弹层 ────────────────────────────────────────────────────────

/// 历史会话弹层（ContentDialog 模态）：全部非归档会话，点击 resume。
/// resume 走 [`Bridge::spawn_resume`] 完整链（见模块教训注释）。
pub fn history_dialog(cx: &mut RenderCx, bridge: Arc<Bridge>, set_open: SetState<bool>) -> Element {
    let (items, set_items) = cx.use_state::<Vec<SessionItem>>(Vec::new());
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_rev = cx.use_ref::<u64>(0);

    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_items = set_items.clone();
        let timer = timer.clone();
        let last_rev = last_rev.clone();
        move || {
            let core = bridge.core();
            bridge.spawn_refresh_sessions();
            *last_rev.borrow_mut() = core.session_snapshot().1;
            if let Ok(t) = DispatcherTimer::new(HISTORY_POLL_INTERVAL, {
                let core = core.clone();
                let set_items = set_items.clone();
                let last_rev = last_rev.clone();
                move || {
                    let (items, rev) = core.session_snapshot();
                    if rev != *last_rev.borrow() {
                        *last_rev.borrow_mut() = rev;
                        set_items.call(items);
                    }
                }
            }) {
                *timer.borrow_mut() = Some(t);
            }
        }
    });

    let mut rows: Vec<Element> = Vec::new();
    let mut sessions: Vec<&SessionItem> = items.iter().filter(|s| !s.archived).collect();
    sessions.sort_by(|a, b| b.seed.cmp(&a.seed));
    for s in sessions.iter().take(50) {
        let seed = s.seed.clone();
        let set_open = set_open.clone();
        rows.push(
            border(
                hstack((
                    state_dot(s.state),
                    text_block(&s.title)
                        .font_size(13.0)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .foreground(ThemeRef::PrimaryText),
                ))
                .spacing(10.0)
                .padding(Thickness::xy(12.0, 8.0)),
            )
            .background(ThemeRef::LayerFill)
            .corner_radius(4.0)
            .on_tapped({
                let bridge = bridge.clone();
                let seed = seed.clone();
                let set_open = set_open.clone();
                move || {
                    // 只调 spawn_resume：自带导航与竞态防护（模块教训注释）。
                    bridge.spawn_resume(&seed);
                    set_open.call(false);
                }
            })
            .with_key(format!("history-{}", s.seed))
            .into(),
        );
    }
    if rows.is_empty() {
        rows.push(
            text_block("暂无会话")
                .font_size(12.0)
                .foreground(ThemeRef::SecondaryText)
                .into(),
        );
    }

    let list: Element = scroll_viewer(vstack(rows).spacing(4.0))
        .height(480.0)
        .into();

    // 遮罩卡片（diff_drawer 同款：半透明黑 scrim 拦截输入；点击遮罩关闭）。
    // 弃用 ContentDialog：其 content 槽仅收纯文本，塞不进可点击列表。
    let scrim: Element = border(text_block(""))
        .background(Color {
            a: 140,
            r: 0,
            g: 0,
            b: 0,
        })
        .on_tapped({
            let set_open = set_open.clone();
            move || set_open.call(false)
        })
        .into();
    let card: Element = border(
        vstack((
            hstack((
                text_block("历史会话").font_size(14.0).semibold(),
                button("✕ 关闭").subtle().on_click({
                    let set_open = set_open.clone();
                    move || set_open.call(false)
                }),
            ))
            .spacing(12.0)
            .horizontal_alignment(HorizontalAlignment::Stretch),
            list,
        ))
        .spacing(tokens::SPACE_3),
    )
    .background(ThemeRef::LayerFill)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(8.0)
    .width(420.0)
    .padding(Thickness::xy(tokens::SPACE_4, tokens::SPACE_4))
    .into();
    grid((scrim, card))
        .rows([GridLength::STAR])
        .columns([GridLength::STAR])
        .into()
}
