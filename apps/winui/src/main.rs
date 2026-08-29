//! QAQ-Harness WinUI desktop shell — 原生 XAML 视图族。
//!
//! Mica 窗口承载全原生视图族（sidebar/header/composer/chat/interaction/
//! home/skills/settings），`bridge.rs` 通过 `qaqh-client` 直连 daemon
//! （Ringing 协议：三 SSE 频道事件解析 + 命令/查询直发）。

#![windows_subsystem = "windows"]

mod app_log;
mod bridge;
mod chat_adapter;
mod chat_view;
mod composer_bar;
mod diagnostics;
mod diff_drawer;
mod fonts;
mod header;
mod home_view;
mod info_panel;
mod interaction_overlay;
mod oobe_view;
mod remote_picker;
mod session_tabs;
mod settings_view;
mod shell;
mod shell_store;
mod sidebar;
mod skills_view;

use std::time::{Duration, Instant};

use windows_reactor::*;

use qaqh_fluent::motion;

/// 开屏覆盖层最长显示时间：超过后切换为失败文案并露出错误详情。
const SPLASH_TIMEOUT: Duration = Duration::from_secs(60);

/// T15 收敛：单一 UI 泵的节流常量。原 main.rs 存在 5 个各自独立的
/// DispatcherTimer（50ms pump / 250ms view / 250ms splash / 500ms 字体 /
/// 500ms OOBE），现收敛为一个 50ms 热循环泵 + 时间门控，粗粒度检查按
/// 各自周期执行（见 `app` 内单一 use_effect）。
const UI_PUMP_INTERVAL: Duration = Duration::from_millis(50);
const VIEW_SYNC_INTERVAL: Duration = Duration::from_millis(250);
const SLOW_SYNC_INTERVAL: Duration = Duration::from_millis(500);

fn header_component(_: &(), cx: &mut RenderCx) -> Element {
    header::header(cx, bridge::Bridge::shared())
}

fn oobe_component(props: &SetState<bool>, cx: &mut RenderCx) -> Element {
    oobe_view::oobe_view(cx, props.clone())
}

fn session_tabs_component(_: &(), cx: &mut RenderCx) -> Element {
    session_tabs::session_tabs(cx, bridge::Bridge::shared())
}

fn chat_component(_: &(), cx: &mut RenderCx) -> Element {
    chat_view::chat_view(cx, bridge::Bridge::shared())
}

fn composer_component(_: &(), cx: &mut RenderCx) -> Element {
    composer_bar::composer_bar(cx, bridge::Bridge::shared())
}

fn info_panel_component(_: &(), cx: &mut RenderCx) -> Element {
    info_panel::info_panel(cx, bridge::Bridge::shared())
}

fn home_component(_: &(), cx: &mut RenderCx) -> Element {
    home_view::home_view(cx, bridge::Bridge::shared())
}

fn skills_component(_: &(), cx: &mut RenderCx) -> Element {
    skills_view::skills_view(cx, bridge::Bridge::shared())
}

fn settings_component(props: &String, cx: &mut RenderCx) -> Element {
    settings_view::settings_view(cx, bridge::Bridge::shared(), props.clone())
}

fn history_dialog_component(props: &SetState<bool>, cx: &mut RenderCx) -> Element {
    sidebar::history_dialog(cx, bridge::Bridge::shared(), props.clone()).into()
}

fn interaction_component(_: &(), cx: &mut RenderCx) -> Element {
    interaction_overlay::interaction_overlay(cx, bridge::Bridge::shared()).into()
}

fn diagram_zoom_component(_: &(), cx: &mut RenderCx) -> Element {
    chat_view::diagram_zoom_overlay(cx).into()
}

fn diff_drawer_component(_: &(), cx: &mut RenderCx) -> Element {
    diff_drawer::diff_drawer_overlay(cx, bridge::Bridge::shared()).into()
}

fn remote_picker_component(_: &(), cx: &mut RenderCx) -> Element {
    remote_picker::remote_picker_overlay(cx, bridge::Bridge::shared()).into()
}

fn app(cx: &mut RenderCx) -> Element {
    let bridge = bridge::Bridge::shared();
    // 单一 50ms UI 泵（T15 收敛）：bridge.pump 热循环 + 门控粗粒度检查。
    let ui_timer = cx.use_ref::<Option<DispatcherTimer>>(None);

    // ── 启动即拉配置（Bug#2 修复）──────────────────────────────
    // 此前 config.load 只在设置页/OOBE/「刷新」按钮触发：老用户启动后
    // bridge settings 缓存恒为空 → composer 权限 ComboBox 渲染
    // permission_level=0（SelectedIndex=-1 被 WinUI 规范化触发误写 L1）、
    // home 误报「未配置 API Key」、字体/权限投影不生效。
    // 此处首帧挂载即拉一次权威快照（spawn_config_load 幂等：缓存非空跳过）。
    cx.use_effect((), {
        let bridge = bridge.clone();
        move || {
            bridge.spawn_config_load(false);
        }
    });

    // Step 1: 内容区元素——左 XAML 侧栏（可拖拽宽度）+ 右区。
    // 右区 = 内层 Grid 多行（WORKFLOW §8 壳主导视图族）：
    //   - row0 = chat 区（原生 ChatView + Composer）——view=chat 时 STAR；
    //   - row1 = XAML 技能页——view=skills 时 STAR；
    //   - row2 = XAML 首页（P1）——view=home 时 STAR；
    //   - row3 = XAML 设置页（P2）——view=settings 时 STAR。
    // 非当前视图不仅行高 0，还不声明对应 Component：子树直接卸载，
    // use_effect cleanup/drop 停止 timer/on_frame。
    // 侧栏 pane 折叠状态（原生 NavigationView 自管汉堡键/动效），记住上次。
    let (pane_open, set_pane_open) = cx.use_state::<bool>(sidebar::load_pane_open());
    // Settings 分类：左栏（Settings 模式）与 settings_view 共享的单一事实源。
    let (settings_category, set_settings_category) = cx.use_state::<String>("models".to_string());
    // 返回键目标：最近一次非 settings 视图（进入设置前所在栏）。
    let prev_non_settings = cx.use_ref::<String>("chat".to_string());
    let (view, set_view) = cx.use_state::<String>("home".to_string());
    let (_, set_info_open) = cx.use_state::<bool>(false);
    let last_view = cx.use_ref::<String>("home".to_string());
    let last_info_open = cx.use_ref::<bool>(false);
    // 历史会话弹层开关（左栏「历史」条目触发；ContentDialog 承载）。
    let (history_open, set_history_open) = cx.use_state::<bool>(false);

    // ── 字体：settings 快照到达/变化时全局应用（FontFamily 为继承属性，
    // 设置内容根一次即全树生效；空 = 恢复系统默认）。常驻轮询保证
    // 启动后（不打开设置页）也能应用上次保存的字体。──────────────
    let last_font = cx.use_ref::<String>(String::new());
    // ── 主题：settings 快照到达/变化时应用持久化主题（与字体同一慢同步
    // 轮询；此前主题仅在设置页手动切换时生效，启动后恒默认跟随系统）。
    let last_theme = cx.use_ref::<String>(String::new());

    // ── OOBE：首次启动引导（P-6 覆盖层最上层，盖住整个壳）────────────
    // 无完成标志 → 显示引导；config 快照到达且已配置 key → 自动收尾
    // （老用户/标志丢失场景，一闪而过）；设置页「重新运行引导」→ 强制
    // 重开且关闭自动收尾（否则刚打开就被"已配置"判定关掉）。
    let (oobe_visible, set_oobe_visible) = cx.use_state::<bool>(!oobe_view::oobe_done());
    let oobe_auto = cx.use_ref::<bool>(!oobe_view::oobe_done());

    let skills: Element = if view == "skills" {
        grid((component(skills_component, ()).with_key("shell-skills"),))
            .with_key("shell-skills-wrap")
            .rows([GridLength::STAR])
            .columns([GridLength::STAR])
            .grid_row(1)
            .grid_column(0)
            .transition(motion::navigation_enter(), motion::content_exit())
            .into()
    } else {
        Element::Empty
    };
    let home: Element = if view == "home" {
        grid((component(home_component, ()).with_key("shell-home"),))
            .with_key("shell-home-wrap")
            .rows([GridLength::STAR])
            .columns([GridLength::STAR])
            .grid_row(2)
            .grid_column(0)
            .transition(motion::navigation_enter(), motion::content_exit())
            .into()
    } else {
        Element::Empty
    };
    let settings: Element = if view == "settings" {
        grid((component(settings_component, settings_category.clone()).with_key("shell-settings"),))
            .with_key("shell-settings-wrap")
            .rows([GridLength::STAR])
            .columns([GridLength::STAR])
            .grid_row(3)
            .grid_column(0)
            .transition(motion::navigation_enter(), motion::content_exit())
            .into()
    } else {
        Element::Empty
    };
    // 内容区四行视图族（WORKFLOW §8 壳主导）：row0=chat 区（原生 ChatView
    // + Composer 底部栏，view=chat 时 STAR）、row1=skills、row2=home、
    // row3=settings；非当前视图行高 0 且不挂组件，停止后台轮询。
    let chat_visible = view == "chat";
    // reactor 限制：Element::Component 无 modifiers（fork element.rs 注释：
    // Component/Empty 链式修饰静默忽略）——component 上的 grid_row/grid_column
    // 不生效（解绑回归：info 面板落到 chat 区、composer 被覆盖）。
    // 改用普通 grid 包裹承载定位，组件填满 wrapper。
    let composer: Element = if chat_visible {
        grid((component(composer_component, ()).with_key("shell-composer"),))
            .with_key("shell-composer-wrap")
            .rows([GridLength::STAR])
            .columns([GridLength::STAR])
            .grid_row(1)
            .grid_column(0)
            .into()
    } else {
        Element::Empty
    };
    let native_chat: Element = if chat_visible {
        component(chat_component, ())
            .with_key("shell-chat")
            .grid_row(0)
            .grid_column(0)
    } else {
        Element::Empty
    };
    // chat_area 必须显式 STAR 列：缺列定义时 WinUI 按隐式 Auto 测量，
    // chat_view（Star 列 ListView）与 composer 宽度由内容决定而非可用空间
    // （长行不换行撑爆列宽 / 窗口缩放不跟随）——L376-378 覆盖层同款实证。
    let chat_area: Element = if chat_visible {
        grid((native_chat, composer))
            .with_key("shell-chat-area")
            .rows([GridLength::STAR, GridLength::Auto])
            .columns([GridLength::STAR])
            .transition(motion::navigation_enter(), motion::content_exit())
            .into()
    } else {
        Element::Empty
    };
    let right_content: Element = grid((chat_area, skills, home, settings))
        .rows([
            if view == "skills" || view == "home" || view == "settings" {
                GridLength::Pixel(0.0)
            } else {
                GridLength::STAR
            },
            if view == "skills" {
                GridLength::STAR
            } else {
                GridLength::Pixel(0.0)
            },
            if view == "home" {
                GridLength::STAR
            } else {
                GridLength::Pixel(0.0)
            },
            if view == "settings" {
                GridLength::STAR
            } else {
                GridLength::Pixel(0.0)
            },
        ])
        .columns([GridLength::STAR])
        .into();
    // Step 1b: Info 面板右列（P4a）——chat 视图恒显；离开 chat 时
    // 不挂组件以卸载内部轮询，列宽同时归零。
    let info_el: Element = if chat_visible {
        grid((component(info_panel_component, ()).with_key("shell-info"),))
            .with_key("shell-info-wrap")
            .rows([GridLength::STAR])
            .columns([GridLength::STAR])
            .grid_row(0)
            .grid_column(1)
            .into()
    } else {
        Element::Empty
    };
    let info_width = if view == "chat" {
        GridLength::Pixel(info_panel::PANEL_WIDTH)
    } else {
        GridLength::Pixel(0.0)
    };
    // right_body 必须显式 STAR 行：缺行定义时隐式 Auto 行高 = 内容高，
    // chat 记录超高时整行溢出裁剪，composer 被推出可视区、info panel 的
    // scroll_viewer 视口 = 内容高导致滚动失效。
    let right_body: Element = grid((right_content, info_el))
        .columns([GridLength::STAR, info_width])
        .rows([GridLength::STAR])
        .grid_row(1)
        .grid_column(0)
        .into();
    // Session tabs belong to the document workspace, not to the navigation
    // rail. Keeping both in one right-side grid aligns their coordinate space
    // while the transcript retains its narrower centered reading measure.
    let tabs: Element = component(session_tabs_component, ())
        .with_key("shell-tabs")
        .grid_row(0)
        .grid_column(0);
    let right: Element = grid((tabs, right_body))
        .rows([
            GridLength::Pixel(session_tabs::TAB_STRIP_HEIGHT),
            GridLength::STAR,
        ])
        .columns([GridLength::STAR])
        .grid_column(1)
        .into();

    // ── 开屏覆盖层（P-6 同 cell 重叠预留的首次应用）────────────────
    // daemon 冷启动可达数十秒（加载历史会话）；覆盖层用原生 ProgressRing
    // 动画覆盖启动期，桥连上 daemon 即移除。
    // 顺序语义：connected 分支优先于 timeout 分支（超时瞬间后端恰好连上
    // 时覆盖层正常消失，不卡失败态）。超时（[`SPLASH_TIMEOUT`]）后释放
    // 覆盖层，露出壳界面（含标题栏）——覆盖层使命仅为启动期动画。
    let (splash_visible, set_splash_visible) = cx.use_state::<bool>(true);
    let splash_started = cx.use_ref::<Option<Instant>>(None);
    let splash_done = cx.use_ref::<bool>(false);
    // ── 单一 UI 泵（T15 收敛）────────────────────────────────────
    // 原 main.rs 存在 5 个各自独立的 DispatcherTimer：50ms bridge.pump /
    // 250ms view+info_open 同步 / 250ms 开屏检查 / 500ms 字体 / 500ms OOBE。
    // 定时器各自创建、各自滴答（周期 50/250/500 互不成整数比，启动时机与
    // 帧抢占难以统一观察）。现将全部收敛为单一 50ms 热循环泵：
    //   - bridge.pump() 每 tick 必跑（事件队列延迟敏感，绝不跳过）；
    //   - view / info_open / 开屏每 250ms 门控执行；
    //   - 字体 / OOBE 每 500ms 门控执行。
    // 粗粒度检查本体是廉价快照读 + 值比较，挂进泵内无感知；收敛后 main.rs
    // 仅 1 个 DispatcherTimer，与 `shell::poll_rev` 的「合并为单一 UI timer」
    // 设计方向一致（未来组件侧只改该 helper）。
    let last_sync = cx.use_ref::<Option<Instant>>(Some(Instant::now()));
    let last_slow = cx.use_ref::<Option<Instant>>(Some(Instant::now()));
    cx.use_effect((), {
        let bridge = bridge.clone();
        let ui_timer = ui_timer.clone();
        let set_view = set_view.clone();
        let set_info_open = set_info_open.clone();
        let set_oobe_visible = set_oobe_visible.clone();
        let set_splash_visible = set_splash_visible.clone();
        let last_view = last_view.clone();
        let last_info_open = last_info_open.clone();
        let last_font = last_font.clone();
        let last_theme = last_theme.clone();
        let last_sync = last_sync.clone();
        let last_slow = last_slow.clone();
        let prev_non_settings = prev_non_settings.clone();
        let oobe_auto = oobe_auto.clone();
        let splash_started = splash_started.clone();
        let splash_done = splash_done.clone();
        move || {
            if ui_timer.borrow().is_none() {
                match DispatcherTimer::new(UI_PUMP_INTERVAL, move || {
                    // 热路径：bridge 事件队列泵（延迟敏感，绝不跳过）。
                    bridge.pump();

                    let now = Instant::now();

                    // ~250ms：view / info_open 同步 + 开屏检查。
                    let sync_due = last_sync
                        .borrow()
                        .as_ref()
                        .map(|t| now.duration_since(*t) >= VIEW_SYNC_INTERVAL)
                        .unwrap_or(true);
                    if sync_due {
                        *last_sync.borrow_mut() = Some(now);

                        let v = bridge.core().current_view();
                        if v != *last_view.borrow() {
                            // 返回键目标：非 settings 视图持续记为「来路」。
                            if v != "settings" {
                                *prev_non_settings.borrow_mut() = v.clone();
                            }
                            *last_view.borrow_mut() = v.clone();
                            set_view.call(v.clone());
                        }
                        // Info 面板开关（Web shell.setHeader 投影的 info_open；
                        // 面板已常驻（2026-08 决策：省动画），此处仅同步 flag
                        // 供 header 按钮状态使用，不再驱动列宽。
                        let o = bridge.core().header_snapshot().0.info_open;
                        if o != *last_info_open.borrow() {
                            *last_info_open.borrow_mut() = o;
                            log_diag(&format!("main: info_open -> {o}"));
                            set_info_open.call(o);
                        }
                        // 开屏：已 done 直接跳过；未连上则记录起点，并在
                        // connected / 超时（[`SPLASH_TIMEOUT`]）时释放。
                        if !*splash_done.borrow() {
                            if splash_started.borrow().is_none() {
                                *splash_started.borrow_mut() = Some(Instant::now());
                            }
                            if bridge.core().backend_connected() {
                                *splash_done.borrow_mut() = true;
                                set_splash_visible.call(false);
                                log_diag("backend connected; splash dismissed");
                            } else if splash_started
                                .borrow()
                                .as_ref()
                                .is_some_and(|t| t.elapsed() >= SPLASH_TIMEOUT)
                            {
                                *splash_done.borrow_mut() = true;
                                set_splash_visible.call(false);
                                log_diag("backend connection timed out; splash released");
                            }
                        }
                    }

                    // ~500ms：字体 + OOBE。
                    let slow_due = last_slow
                        .borrow()
                        .as_ref()
                        .map(|t| now.duration_since(*t) >= SLOW_SYNC_INTERVAL)
                        .unwrap_or(true);
                    if slow_due {
                        *last_slow.borrow_mut() = Some(now);

                        // 字体：settings 快照到达/变化时全局应用（FontFamily
                        // 为继承属性，设置内容根一次即全树生效；空 = 恢复系统
                        // 默认）。常驻轮询保证启动后（不打开设置页）也生效。
                        let (snap, _) = bridge.core().settings_snapshot();
                        if let Some(snap) = snap {
                            let font = fonts::effective_ui_font(&snap.font_family).to_string();
                            if font != *last_font.borrow() {
                                *last_font.borrow_mut() = font.clone();
                                windows_reactor::set_font_family(Some(&font));
                            }
                            // 主题：快照到达时应用持久化主题（system/空 = 默认
                            // 跟随系统）。与设置页内手动切换的三态映射保持一致。
                            let theme = if snap.theme.is_empty() || snap.theme == "system" {
                                "system"
                            } else {
                                snap.theme.as_str()
                            };
                            if theme != *last_theme.borrow() {
                                *last_theme.borrow_mut() = theme.to_string();
                                let rt = match theme {
                                    "light" => windows_reactor::RequestedTheme::Light,
                                    "dark" | "dark-gray" => windows_reactor::RequestedTheme::Dark,
                                    _ => windows_reactor::RequestedTheme::Default,
                                };
                                windows_reactor::set_requested_theme(rt);
                            }
                        }
                        // OOBE：设置页「重新运行引导」→ 强制重开且本次不自动
                        // 收尾；否则 daemon 已配置主 key（老用户/标志丢失）→
                        // 自动完成（一闪而过）。
                        if oobe_view::take_rerun_request() {
                            *oobe_auto.borrow_mut() = false;
                            set_oobe_visible.call(true);
                            log_diag("oobe: rerun requested");
                        } else if *oobe_auto.borrow() {
                            let configured = bridge
                                .core()
                                .settings_snapshot()
                                .0
                                .is_some_and(|s| s.api_key_configured);
                            if configured {
                                oobe_view::mark_oobe_done();
                                *oobe_auto.borrow_mut() = false;
                                set_oobe_visible.call(false);
                                log_diag("oobe: config present, auto-dismissed");
                            }
                        }
                    }
                }) {
                    Ok(t) => {
                        *ui_timer.borrow_mut() = Some(t);
                        log_diag("ui pump timer created");
                    }
                    Err(e) => log_diag(&format!("ui pump timer failed: {e}")),
                }
            }
        }
    });

    // Step 2: Grid 两行——row0 = XAML 标题栏（48px，SetTitleBar 拖拽区，
    // host 自动接线 host.rs:277-288）；row1 = 内容区（侧栏 | 右区）。
    let titlebar: Element = component(header_component, ())
        .with_key("shell-header")
        .grid_row(0)
        .grid_column(0);
    // ── 全局 NavigationView 壳（原生控件：选中动效/折叠/回弹全套）──
    // 主模式 items = 主页/聊天/技能/历史/设置；Settings 模式 items =
    // 九分类（原地替换，零嵌套）。content 恒为右区（标签条 + 视图族）。
    // 分类导航移交左栏后 settings_view 不再内嵌 NavigationView。
    let nav_items = sidebar::build_nav_items(&view, &settings_category);
    let selected_tag = if view == "settings" {
        settings_category.clone()
    } else {
        view.clone()
    };
    let in_settings = view == "settings";
    let shell_nav: Element = NavigationView::new(nav_items, right)
        .selected_tag(selected_tag)
        .on_selection_changed({
            let bridge = bridge.clone();
            let set_settings_category = set_settings_category.clone();
            let set_history_open = set_history_open.clone();
            move |tag: String| {
                // BUG-F1：NavigationView 对程序性选中变更（selected_tag 下发、
                // MenuItems 清空重建）同样触发 SelectionChanged。设置→返回时
                // 条目集从九分类换回主导航，先触发一次空选（tag=""）回声，
                // 旧代码把它当用户点击 navigate("") → 视图落到未知 tag →
                // 四个视图行全 0 高、无组件挂载 → 内容区整片白屏（恢复出
                // chat 后控件立马卸载）。空 tag 与非可导航 tag 一律忽略。
                if tag.is_empty() {
                    log_diag(&format!(
                        "nav: ignore selection echo (empty tag), view={}",
                        bridge.current_view_name()
                    ));
                    return;
                }
                if bridge.current_view_name() == "settings" {
                    // Settings 模式：tag = 分类 id，只切 section 不动路由。
                    set_settings_category.call(tag);
                } else if tag == "history" {
                    set_history_open.call(true);
                } else if matches!(tag.as_str(), "home" | "chat" | "skills" | "settings") {
                    bridge.navigate(&tag, None);
                } else {
                    log_diag(&format!(
                        "nav: ignore selection echo tag={tag:?}, view={}",
                        bridge.current_view_name()
                    ));
                }
            }
        })
        .pane_display_mode(NavigationViewPaneDisplayMode::Left)
        .pane_open(pane_open)
        .on_pane_open_changed({
            let set_pane_open = set_pane_open.clone();
            move |open: bool| {
                set_pane_open.call(open);
                sidebar::store_pane_open(open);
            }
        })
        .pane_toggle_button_visible(true)
        .back_button_visible(in_settings)
        .back_enabled(in_settings)
        .on_back_requested({
            let bridge = bridge.clone();
            let prev_non_settings = prev_non_settings.clone();
            move || {
                let prev = prev_non_settings.borrow().clone();
                bridge.navigate(&prev, None);
            }
        })
        .settings_visible(false)
        .open_pane_length(240.0)
        .grid_row(1)
        .grid_column(0)
        .into();
    let base: Element = grid((titlebar, shell_nav))
        .rows([GridLength::Pixel(header::HEADER_HEIGHT), GridLength::STAR])
        .columns([GridLength::STAR])
        .into();
    // 覆盖层与基础层同 cell 重叠渲染（P-6 预留模式），盖住 titlebar + 内容区。
    // 注意：`splash_visible=false` 时的空 `grid(())` 依赖 WinUI"无背景元素不参与
    // 命中测试"的平台行为实现点击穿透——切勿给空 grid 添加背景。
    let splash: Element = if splash_visible {
        grid((
            ProgressRing::default().width(48.0).height(48.0),
            text_block("正在连接 QAQ-Harness…")
                .font_size(14.0)
                .foreground(ThemeRef::SecondaryText)
                .grid_row(1),
        ))
        .rows([GridLength::Pixel(64.0), GridLength::Auto])
        .background(ThemeRef::LayerFill)
        .into()
    } else {
        grid(()).into()
    };
    // 交互模态覆盖层（P-6 同模式）：kind="none" 时内部空 grid 穿透；
    // 有交互时半透明遮罩 + 卡片（permission/ask 模板）。置于最上层。
    let interaction: Element = component(interaction_component, ()).with_key("shell-interaction");
    // 图表放大覆盖层（P-6 同模式）：chat_view 点击写入 DIAGRAM_ZOOM，
    // 本组件轮询弹开全窗大图。无请求时空 grid 穿透。
    let diagram_zoom: Element =
        component(diagram_zoom_component, ()).with_key("shell-diagram-zoom");
    // diff 抽屉覆盖层（V4）：turn 末尾「查看详情」→ 右侧滑入面板。
    // 无请求时空 grid 穿透（同 interaction 模式）。
    let diff_drawer: Element = component(diff_drawer_component, ()).with_key("shell-diff-drawer");
    // 远端文件选择器覆盖层（临时跨端模式）：header 工作区按钮在远端模式下打开。
    let remote_picker: Element =
        component(remote_picker_component, ()).with_key("shell-remote-picker");
    // 首次启动引导（P-6 同模式）：无完成标志时盖住整个壳（含 titlebar，
    // 内部自带全屏 LayerFill 背景）；完成/跳过写标志后组件卸载，空 grid
    // 穿透（同 splash 注释：切勿给空 grid 添加背景）。
    let oobe: Element = if oobe_visible {
        component(oobe_component, set_oobe_visible.clone()).with_key("shell-oobe")
    } else {
        grid(()).into()
    };
    // 覆盖层 grid 必须显式 STAR 行/列：Grid 默认 Auto 会让 Stretch 子元素只
    // 覆盖内容大小（scrim 不全窗，遮罩不生效）——demo 已实证，同 interaction
    // 内部 grid 的修复。五个子元素重叠同一 cell（P-6 覆盖层模式）。
    // ⚠ 顺序 = z-order（后声明在上层）：interaction（agent 阻塞交互）必须在
    // diagram_zoom / diff_drawer **之后**——否则 diff 面板/图表放大打开时其
    // 全屏遮罩会盖住 ask 面板（「ask 弹不出来」根因，2026-08-12 实测定位）。
    // 模态互斥时 agent 交互优先：即使 diff 面板开着，ask 也应弹在最上层。
    let history_dialog_el: Element = if history_open {
        component(history_dialog_component, set_history_open.clone()).with_key("shell-history")
    } else {
        grid(()).into()
    };
    grid((
        base,
        splash,
        diagram_zoom,
        diff_drawer,
        history_dialog_el,
        remote_picker,
        interaction,
        oobe,
    ))
    .rows([GridLength::STAR])
    .columns([GridLength::STAR])
    .into()
}

/// Minimal file logger for headless diagnosis (GUI subsystem has no console).
fn log_diag(msg: &str) {
    crate::app_log::write("app", msg);
}

fn main() -> windows_reactor::Result<()> {
    // 数据根迁移（DeepX → QAQ-Harness）：必须在任何 connect_client / daemon
    // spawn 之前执行——旧品牌 marker 会让 daemon 拒绝启动（校验不通过即
    // "did not publish live discovery in time"，首屏会话/工作区/config 全挂）。
    bridge::migrate_legacy_data_root_marker();
    // 壳侧性能诊断：恢复上次采集模式（默认 ZDR）；必须在 render 前注册
    // render observer（thread_local，UI 线程 = main 线程）。
    diagnostics::init();
    windows_reactor::set_render_observer(Some(Box::new(|info| {
        diagnostics::record_render(
            info.tree_build_ms,
            info.reconcile_ms,
            info.effects_ms,
            info.elements_diffed,
            info.elements_skipped,
            info.elements_created,
        );
    })));
    App::new()
        .title("QAQ Harness")
        .inner_size(1200.0, 800.0)
        .backdrop(Backdrop::Mica)
        // 退出诊断（reactor #4787 on_exit）：窗口全关后、进程退出前执行。
        // 日志里出现此行 = 正常退出路径；闪退（崩溃/强杀）不会执行到这里，
        // 用于区分「正常关闭」与「异常终止」，辅助闪退调查。
        .on_exit(|| log_diag("app exit: all windows closed (normal path)"))
        // panic 诊断（reactor #4829 起 fatal）：on_fault API 已删除，callback panic
        // 不再转发给应用；reactor 会在 abort 前自行 emit
        // 「windows_reactor: {context} panicked: {msg}; aborting」，
        // 闪退调查可从该行（OutputDebugString/stderr）取源头证据。
        .render(app)
}
