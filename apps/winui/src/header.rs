//! XAML 原生标题栏（P0）— ThreadHeader 的壳侧承载（PLAN-NATIVE-UI.md）。
//!
//! 布局：
//!   TitleBar（SetTitleBar 拖拽区，host 自动接线 host.rs:277-288）
//!   ├── title 槽：TextBlock（会话标题 / 视图名，shell.header 推送）
//!   └── footer 槽：hstack( ①workspace ②location ③console ┃ ④info ⑤undo ⑥compact )
//!        —— ⑧pet 不渲染（壳 stub 恒 false，规划决策）
//!
//! 状态：timer 轮询 `core.header_snapshot()` rev（同 sidebar 500ms 模式，
//! 经 `shell::poll_rev` helper，P-4）。
//! 动作：①②③ 壳直接处理（目录对话框 / 系统 shell / DevTools）；
//!       ④-⑦ emit `shell.headerAction` 回传 Web 执行（状态单一数据源在 Web）。

use std::sync::Arc;
use std::time::Duration;

use windows_reactor::*;

use crate::bridge::{Bridge, HeaderState, log_diag};

/// 标题栏高度（PLAN-NATIVE-UI.md 布局：row 0 = 48px）。
pub const HEADER_HEIGHT: f64 = 48.0;

/// 标题栏动作按钮：icon 上方按钮 + label 默认显示在 icon 之下。
///
/// WinUI Button 的 Icon+Content 只能水平排列，无法「icon 之下带 label」；
/// 自组 vstack(icon 按钮, label)：icon 区域走 Button（hover/按压/active
/// 高亮原生语义），label 区域同样可点——但点击入口**只挂 label 自身**
/// （`on_tapped`），不挂父 vstack：Button 的 Click 与 label 的 Tapped
/// 命中目标不相交，单次点击最多触发一次（曾把 `on_tapped` 挂在父 vstack
/// 上，若 Button 的 Tapped 未被标记 handled，一次点击会串行弹出两个目录
/// 对话框——「二次弹窗」根因之一）。label 常显——裸图标按钮无中文说明，
/// 与 Windows 右键菜单缺 label 同样令人困惑（D6 撤销）。
fn action_button(
    icon: Icon,
    label: &'static str,
    automation_id: &'static str,
    enabled: bool,
    active: bool,
    on_click: impl Fn() + Clone + 'static,
) -> Element {
    let mut btn = button("")
        .icon(icon)
        .subtle()
        .enabled(enabled)
        .tooltip(label)
        .automation_name(label)
        .automation_id(automation_id)
        .on_click(on_click.clone());
    if active {
        btn = btn.accent();
    }
    let btn_el: Element = btn.into();
    let label_el: Element = text_block(label)
        .font_size(10.0)
        .foreground(if enabled {
            ThemeRef::SecondaryText
        } else {
            ThemeRef::DisabledText
        })
        .on_tapped(move || {
            if enabled {
                on_click();
            }
        })
        .into();
    vstack((btn_el, label_el))
        .spacing(1.0)
        .automation_name(label)
        .automation_id(automation_id)
        .tooltip(label)
        .into()
}

/// XAML 标题栏组件（放入外层 Grid row 0；host 检测到 TitleBar 自动接线）。
pub fn header(cx: &mut RenderCx, bridge: Arc<Bridge>) -> Element {
    let (state, set_state) = cx.use_state::<HeaderState>(HeaderState::default());
    let (lang, set_lang) = cx.use_state::<String>("zh".to_string());
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let lang_timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_rev = cx.use_ref::<u64>(0);
    let last_lang_rev = cx.use_ref::<u64>(0);

    // 首次挂载：500ms rev 轮询（同 sidebar 模式；shell::poll_rev helper）。
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_state = set_state.clone();
        let timer = timer.clone();
        let last_rev = last_rev.clone();
        move || {
            crate::shell::poll_rev(
                "header",
                timer,
                last_rev,
                Duration::from_millis(500),
                move || bridge.core().header_snapshot(),
                move |s| set_state.call(s),
            );
        }
    });

    // Locale is owned by config rather than HeaderState. Poll its independent
    // revision so changing language updates tooltips without requiring an
    // unrelated conversation/header event.
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_lang = set_lang.clone();
        let lang_timer = lang_timer.clone();
        let last_lang_rev = last_lang_rev.clone();
        move || {
            crate::shell::poll_rev(
                "header_lang",
                lang_timer,
                last_lang_rev,
                Duration::from_millis(500),
                move || bridge.core().settings_snapshot(),
                move |s| set_lang.call(s.map(|v| v.lang).unwrap_or_else(|| "zh".into())),
            );
        }
    });

    // 系统主题：reactor 由 ActualThemeChanged 驱动更新（engine.rs:733），
    // WebView 移除后无需回传 Web——轮询与 emit_theme_changed 一并删除。

    // ── 点击分发（①②壳直接；③-⑥ 直连动作，协议请求 Rust 直发）──
    // 合并方案：左侧工作区为唯一入口（先选工作区再创建会话）。
    // 顶部按钮不再直调 `workspace.set`（会话级目录），改为代理到
    // 组织工作区：选中/创建组织工作区 + 会话级 set 兜底，保证两处同源。
    #[allow(unused_variables)]
    let on_workspace = {
        let bridge = bridge.clone();
        move || {
            // 远端模式：仍走远端选择器（picked 目录由 remote_picker straight
            // 发 workspace.set，会经后端 manager::set_cwd 自动 attach）
            if bridge.core().remote_profile_snapshot().is_some() {
                crate::remote_picker::open_remote_picker(String::new());
                return;
            }
            match bridge.pick_workspace_directory() {
                Ok(serde_json::Value::String(path)) => {
                    let p = path.clone();
                    // 组织工作区：不存在则创建、存在则去重返回并自动选中
                    // （core_sessions::spawn_workspace_create 已含选中逻辑）
                    bridge.spawn_workspace_create(p.clone());
                    // 会话级兜底：若已有活动会话，同步其 cwd 保证
                    // 左侧筛选立即命中（后端 set_cwd 也会 attach 双保险）
                    if !bridge.core().active_seed().is_empty() {
                        bridge.spawn_workspace_set(p);
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    log_diag(&format!("pick workspace directory failed: {err}"));
                    bridge.report_workspace_error(format!("选择工作区失败：{err}"));
                }
            }
        }
    };
    let on_compact = {
        let bridge = bridge.clone();
        move || {
            // 重发前清除上次终态（F-N3：failed 常驻语义的出口）。
            let seed = bridge.core().active_seed();
            bridge.core().clear_compact_result(&seed);
            bridge.spawn_conversation_command(
                qaqh_client::ConversationCommand::ConversationCompact { turn_id: None },
            )
        }
    };

    // F-N1：壳级返回（native TitleBar back chevron）——active_seed 非空回
    // chat，否则回 home（进设置走 navigate(view, None)，seed 未动 = 天然锚点）。
    let on_back = {
        let bridge = bridge.clone();
        move || {
            let seed = bridge.core().active_seed();
            if seed.is_empty() {
                bridge.navigate("home", None);
            } else {
                bridge.navigate("chat", Some(&seed));
            }
        }
    };

    // ── 刷新（安慰剂 + 真实重拉：会话列表 + 配置 force）────────────
    // 点击 → 按钮转圈 900ms（"正在刷新"仪式感）→ 自动复位。数据重拉是
    // 异步的，各视图 500ms 轮询自动反映结果——刷新本身无需等待。
    let (refreshing, set_refreshing) = cx.use_state::<bool>(false);
    let refresh_timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let on_refresh = {
        let bridge = bridge.clone();
        let set_refreshing = set_refreshing.clone();
        let refresh_timer = refresh_timer.clone();
        move || {
            bridge.spawn_refresh_sessions();
            bridge.spawn_config_load(true);
            set_refreshing.call(true);
            if refresh_timer.borrow().is_none() {
                if let Ok(t) = DispatcherTimer::new(Duration::from_millis(900), {
                    let set_refreshing = set_refreshing.clone();
                    let refresh_timer = refresh_timer.clone();
                    move || {
                        set_refreshing.call(false);
                        if let Some(t) = refresh_timer.borrow().as_ref() {
                            let _ = t.stop();
                        }
                        *refresh_timer.borrow_mut() = None;
                    }
                }) {
                    *refresh_timer.borrow_mut() = Some(t);
                }
            }
        }
    };

    // ── footer 槽：工作区（按钮+路径显示）+ 用量统计 + 压缩 + 刷新 ──
    let divider: Element = border(text_block(""))
        .width(1.0)
        .height(18.0)
        .background(ThemeRef::DividerStroke)
        .vertical_alignment(VerticalAlignment::Center)
        .into();
    let en = lang == "en";
    #[allow(unused_variables)]
    let workspace_label = if en {
        "Choose workspace"
    } else {
        "选择工作区"
    };
    // 工作区路径只读显示（替代原「在资源管理器中打开」按钮）：当前选择
    // 一目了然；过长省略号裁剪（标题栏空间有限），完整路径进 tooltip。
    // 错误文案优先展示（`workspace.set`/picker 失败反馈，SystemCritical）。
    #[allow(unused_variables)]
    let workspace_path: Element = if let Some(err) = &state.workspace_error {
        text_block(err)
            .font_size(11.0)
            .foreground(ThemeRef::SystemCritical)
            .max_width(260.0)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .tooltip(err.clone())
            .automation_name(err)
            .with_key("header-workspace-error")
            .into()
    } else if state.workspace.is_empty() {
        text_block(if en {
            "No workspace"
        } else {
            "未选择工作区"
        })
        .font_size(11.0)
        .foreground(ThemeRef::TertiaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .automation_name("工作区路径（空）")
        .with_key("header-workspace-path-empty")
        .into()
    } else {
        text_block(&state.workspace)
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .max_width(260.0)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .tooltip(state.workspace.clone())
            .automation_name(&state.workspace)
            .with_key("header-workspace-path")
            .into()
    };
    let compact_label = if state.compacting {
        if en {
            "Compacting context…"
        } else {
            "正在压缩上下文…"
        }
    } else if en {
        "Compact context"
    } else {
        "压缩上下文"
    };
    // 压缩终态 chip（F-N3）：completed 绿色 3s 自动淡出（下方 effect 定时
    // 清除，条件清除防竞态误删新压缩的 running），failed 红色常驻至下次压缩。
    let compact_completed = state.compact_result.as_deref() == Some("completed");
    let compact_chip: Element = match state.compact_result.as_deref() {
        Some("completed") => text_block("压缩完成 ✓")
            .font_size(12.0)
            .foreground(ThemeRef::SystemSuccess)
            .automation_name("压缩完成")
            .into(),
        Some("failed") => text_block("压缩失败 ✕")
            .font_size(12.0)
            .foreground(ThemeRef::SystemCritical)
            .automation_name("压缩失败")
            .into(),
        _ => grid(()).into(),
    };
    cx.use_effect_with_cleanup((state.compact_result.clone(),), {
        let bridge = bridge.clone();
        let seed = state.seed.clone();
        move || -> Option<Box<dyn FnOnce()>> {
            if !compact_completed {
                return None;
            }
            let b = bridge.clone();
            let sd = seed.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(3000));
                // 条件清除：期间状态已变（新压缩 running/手动清除）则 no-op。
                b.core().clear_compact_result_if(&sd, "completed");
            });
            None
        }
    });
    let compact_progress: Element = if state.compacting {
        ProgressRing::default()
            .width(16.0)
            .height(16.0)
            .automation_name(compact_label)
            .into()
    } else {
        grid(()).into()
    };
    let refresh_label = if refreshing {
        if en {
            "Refreshing…"
        } else {
            "正在刷新…"
        }
    } else if en {
        "Refresh"
    } else {
        "刷新"
    };
    // 转圈期间图标按钮替换为 ProgressRing（同 compact 模式），禁用防连点。
    let refresh_el: Element = if refreshing {
        ProgressRing::default()
            .width(16.0)
            .height(16.0)
            .automation_name(refresh_label)
            .into()
    } else {
        action_button(
            Icon::symbol(Symbol::Refresh),
            refresh_label,
            "header-refresh",
            true,
            false,
            on_refresh,
        )
    };
    // 工作区按钮/路径已移除（cwd 非持久 bug 挂账；恢复时连同
    // on_workspace/workspace_path 一起回归）。
    let footer: Element = hstack((
        compact_progress,
        action_button(
            Icon::symbol(Symbol::Clear),
            compact_label,
            "header-compact",
            !(state.compacting || state.compact_disabled),
            false,
            on_compact,
        ),
        compact_chip,
        divider.clone(),
        refresh_el,
    ))
    .spacing(6.0)
    .vertical_alignment(VerticalAlignment::Center)
    // Alt+Left：同返回语义（F-N1）；非 settings/skills 视图为 no-op。
    .keyboard_accelerator(KeyboardAccelerator::new(
        VirtualKey::Left,
        VirtualKeyModifiers::Menu,
        {
            let bridge = bridge.clone();
            let on_back = on_back.clone();
            move || {
                let view = bridge.current_view_name();
                if view == "settings" || view == "skills" {
                    on_back();
                }
            }
        },
    ))
    .into();

    // ── TitleBar：title 槽 = 品牌 · 活动会话标识 ─────────────────
    // 布局重构（2026-08）：品牌自左栏移入标题栏；session id 小字随右，
    // 最小化时仍可辨认当前活动会话。
    let title_text = if state.title.is_empty() {
        "QAQ-Harness".to_string()
    } else {
        format!("QAQ-Harness · {}", state.title)
    };
    TitleBar::new(&title_text)
        .back_button_visible(state.view == "settings" || state.view == "skills")
        // BUG-FIX：TitleBar 派生 Default，is_back_button_enabled 默认 false 且
        // title_bar_bindings 无条件下发 → 返回箭头显示但恒灰不可点（回调/快捷键
        // 均已挂好，仅本体被禁用）。可见时恒启用。
        .back_button_enabled(true)
        .on_back_requested(on_back)
        .footer(footer)
        .tall(false)
        .into()
}
