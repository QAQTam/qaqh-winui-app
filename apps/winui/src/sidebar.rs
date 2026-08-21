//! XAML 侧栏 — 简化版工作区导航（WinUI/FD2 原生语义）。
//!
//! 设计决策（用户确认简化）：
//! - **左侧只做工作区筛选**，不再重复渲染会话列表（会话切换统一走
//!   顶部 `TabView session_tabs` + 启动页 `home_view`）。此举消除
//!   `sidebar` 与 `tabs` 双 `list_view` + 双 `500ms` 轮询的同步抖动与
//!   `ScrollViewer(vstack(N*ListView))` 非虚拟化重排（卡顿主因）。
//! - 布局：`NavigationView` 语义的分组头 + `ListView` 轻量工作区行
//!   + `未分组` + `归档` 折叠（`Expander`），未采用手写 `Grid` 假列表。
//! - 交互：工作区行点击 = `set_current_workspace`（标签页随之过滤）；
//!   新建任务 `spawn_new_session` 自动取 `current_workspace_path` 归属。
//! - 宽度可拖拽（`capture_pointer_on_press` + `window_x` 差分，复用原
//!   `splitter` 逻辑）。

use std::sync::Arc;
use std::time::Duration;

use windows_reactor::*;

use crate::bridge::Bridge;
use crate::shell_store::{SessionItem, WorkspaceItem};

fn log_diag(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("qaqh-winui.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "[{}] {msg}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
    }
}

pub const SIDEBAR_MIN_WIDTH: f64 = 180.0;
pub const SIDEBAR_MAX_WIDTH: f64 = 400.0;
pub const SIDEBAR_DEFAULT_WIDTH: f64 = 260.0;

fn build_id_label() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let info = dir.join("build-info.json");
            if let Ok(text) = std::fs::read_to_string(&info) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(id) = v.get("build_id").and_then(|x| x.as_str()) {
                        return format!("v{id}");
                    }
                }
            }
        }
    }
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn icon_button(icon: Icon, on_click: impl Fn() + 'static) -> Element {
    button("").icon(icon).subtle().on_click(on_click).into()
}

/// 会话活动状态 → Fluent 语义色令牌
pub(crate) fn state_color(state: crate::shell_store::ActivityState) -> ThemeRef {
    match state {
        crate::shell_store::ActivityState::Working => ThemeRef::SystemSuccess,
        crate::shell_store::ActivityState::WaitingUser => ThemeRef::Accent,
        crate::shell_store::ActivityState::Starting => ThemeRef::SystemAttention,
        crate::shell_store::ActivityState::Disconnected => ThemeRef::SystemCritical,
        crate::shell_store::ActivityState::Idle => ThemeRef::SystemNeutral,
    }
}

/// 状态圆点：8px 圆形（Border + 4px 圆角），供 session_tabs 复用
pub(crate) fn state_dot(state: crate::shell_store::ActivityState) -> Element {
    border(text_block(""))
        .width(8.0)
        .height(8.0)
        .corner_radius(4.0)
        .background(state_color(state))
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

/// 一个 workspace 行：文件夹图标 + 标题 + 会话数徽标 + 重命名/删除。
/// 选中态 = `SubtleFill` 药丸（Win11 NavigationView 语言），FD2 圆角 8。
fn workspace_row(
    ws: &WorkspaceItem,
    selected: bool,
    count: usize,
    bridge: Arc<Bridge>,
    set_ws_confirm: SetState<Option<String>>,
    set_ws_rename: SetState<Option<(String, String)>>,
) -> Element {
    let id = ws.id.clone();
    let title = ws.title.clone();
    let icon = icon_button(Icon::symbol(Symbol::Folder), {
        let id = id.clone();
        let bridge = bridge.clone();
        move || bridge.set_current_workspace(Some(id.clone()))
    })
    .vertical_alignment(VerticalAlignment::Center);
    let title_el: Element = text_block(title.clone())
        .max_lines(1)
        .text_trimming(TextTrimming::CharacterEllipsis)
        .foreground(ThemeRef::PrimaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .on_pointer_pressed({
            let id = id.clone();
            let bridge = bridge.clone();
            move |_| bridge.set_current_workspace(Some(id.clone()))
        })
        .into();
    let badge: Element = text_block(count.to_string())
        .font_size(11.0)
        .foreground(ThemeRef::SecondaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .into();
    let rename = icon_button(Icon::symbol(Symbol::Edit), {
        let id = id.clone();
        let title = title.clone();
        let set_ws_rename = set_ws_rename.clone();
        move || set_ws_rename.call(Some((id.clone(), title.clone())))
    })
    .vertical_alignment(VerticalAlignment::Center);
    let delete = icon_button(Icon::symbol(Symbol::Delete), {
        let id = id.clone();
        let set_ws_confirm = set_ws_confirm.clone();
        move || set_ws_confirm.call(Some(id.clone()))
    })
    .vertical_alignment(VerticalAlignment::Center);
    let row: Element = grid((
        icon.grid_column(0),
        title_el.grid_column(1),
        badge.grid_column(2),
        rename.grid_column(3),
        delete.grid_column(4),
    ))
    .columns([
        GridLength::Auto,
        GridLength::STAR,
        GridLength::Auto,
        GridLength::Auto,
        GridLength::Auto,
    ])
    .column_spacing(8.0)
    .padding(Thickness::xy(10.0, 6.0))
    .into();
    let item_el = border(row).corner_radius(8.0);
    let item_el = if selected {
        item_el.background(ThemeRef::SubtleFill)
    } else {
        item_el
    };
    item_el.into()
}

fn ungrouped_row(count: usize, selected: bool, bridge: Arc<Bridge>) -> Element {
    let icon = icon_button(Icon::symbol(Symbol::Folder), {
        let bridge = bridge.clone();
        move || bridge.set_current_workspace(None)
    })
    .vertical_alignment(VerticalAlignment::Center);
    let title_el: Element = text_block("未分组")
        .max_lines(1)
        .text_trimming(TextTrimming::CharacterEllipsis)
        .foreground(ThemeRef::PrimaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .on_pointer_pressed({
            let bridge = bridge.clone();
            move |_| bridge.set_current_workspace(None)
        })
        .into();
    let badge: Element = text_block(count.to_string())
        .font_size(11.0)
        .foreground(ThemeRef::SecondaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .into();
    let row: Element = grid((
        icon.grid_column(0),
        title_el.grid_column(1),
        badge.grid_column(2),
    ))
    .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
    .column_spacing(8.0)
    .padding(Thickness::xy(10.0, 6.0))
    .into();
    let item_el = border(row).corner_radius(8.0);
    let item_el = if selected {
        item_el.background(ThemeRef::SubtleFill)
    } else {
        item_el
    };
    item_el.into()
}

/// 归档行（精简，仅标题 + 恢复/删除；归档列表折叠在 Expander 内）
fn archive_row(
    item: &SessionItem,
    bridge: Arc<Bridge>,
    set_confirm: SetState<Option<String>>,
) -> Element {
    let seed = item.seed.clone();
    let title_el: Element = text_block(item.title.clone())
        .max_lines(1)
        .text_trimming(TextTrimming::CharacterEllipsis)
        .foreground(ThemeRef::SecondaryText)
        .vertical_alignment(VerticalAlignment::Center)
        .on_pointer_pressed({
            let seed = seed.clone();
            let bridge = bridge.clone();
            move |_| bridge.spawn_unarchive(&seed)
        })
        .into();
    let delete = icon_button(Icon::symbol(Symbol::Delete), {
        let seed = seed.clone();
        let set_confirm = set_confirm.clone();
        move || set_confirm.call(Some(seed.clone()))
    })
    .vertical_alignment(VerticalAlignment::Center);
    let row: Element = grid((title_el.grid_column(0), delete.grid_column(1)))
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(8.0)
        .padding(Thickness::xy(10.0, 6.0))
        .into();
    border(row).corner_radius(8.0).into()
}

pub fn sidebar(
    cx: &mut RenderCx,
    bridge: Arc<Bridge>,
    width: f64,
    set_width: SetState<f64>,
) -> Element {
    let (items, set_items) = cx.use_state::<Vec<SessionItem>>(Vec::new());
    let (workspaces, set_workspaces) = cx.use_state::<Vec<WorkspaceItem>>(Vec::new());
    let (current_ws, set_current_ws) = cx.use_state::<Option<String>>(None);
    let (confirm_seed, set_confirm_seed) = cx.use_state::<Option<String>>(None);
    let (ws_confirm, set_ws_confirm) = cx.use_state::<Option<String>>(None);
    let (ws_rename, set_ws_rename) = cx.use_state::<Option<(String, String)>>(None);
    let ws_rename_input = cx.use_ref::<String>(String::new());
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_rev = cx.use_ref::<u64>(0);
    let last_ws_rev = cx.use_ref::<u64>(0);
    let last_cur_ws = cx.use_ref::<Option<String>>(None);
    let drag_start = cx.use_ref::<Option<(f64, f64)>>(None);
    let build_label = cx.use_ref::<Option<String>>(None);
    let (splitter_hover, set_splitter_hover) = cx.use_state::<bool>(false);

    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_items = set_items.clone();
        let set_workspaces = set_workspaces.clone();
        let set_current_ws = set_current_ws.clone();
        let timer = timer.clone();
        let last_rev = last_rev.clone();
        let last_ws_rev = last_ws_rev.clone();
        move || {
            let core = bridge.core();
            bridge.spawn_refresh_sessions();
            bridge.spawn_refresh_workspaces();
            *last_rev.borrow_mut() = core.session_snapshot().1;
            *last_ws_rev.borrow_mut() = core.workspace_snapshot().1;
            if let Ok(t) = DispatcherTimer::new(Duration::from_millis(500), {
                let core = core.clone();
                let set_items = set_items.clone();
                let set_workspaces = set_workspaces.clone();
                let set_current_ws = set_current_ws.clone();
                let last_rev = last_rev.clone();
                let last_ws_rev = last_ws_rev.clone();
                move || {
                    let (items, rev) = core.session_snapshot();
                    if rev != *last_rev.borrow() {
                        *last_rev.borrow_mut() = rev;
                        set_items.call(items);
                    }
                    let (workspaces, ws_rev) = core.workspace_snapshot();
                    if ws_rev != *last_ws_rev.borrow() {
                        *last_ws_rev.borrow_mut() = ws_rev;
                        set_workspaces.call(workspaces);
                    }
                    let cur = core.current_workspace();
                    let prev = last_cur_ws.borrow().clone();
                    if cur != prev {
                        *last_cur_ws.borrow_mut() = cur.clone();
                        set_current_ws.call(cur);
                    }
                }
            }) {
                *timer.borrow_mut() = Some(t);
            }
        }
    });

    let brand: Element = {
        let el: Element = text_block("QAQ-Harness").semibold().font_size(18.0).into();
        el.on_pointer_pressed({
            let bridge = bridge.clone();
            move |_| bridge.navigate("home", None)
        })
        .margin(12.0)
    };

    // 新建：自动归属当前选中工作区（`current_workspace_path`）
    let actions: Element = {
        let sp: Element = hstack((button("新建任务")
            .icon(Icon::symbol(Symbol::Add))
            .subtle()
            .on_click({
                let bridge = bridge.clone();
                move || bridge.spawn_new_session()
            }),))
        .into();
        sp.margin(12.0)
    };

    let group_label: Element = {
        let label: Element = text_block("工作区")
            .font_size(12.0)
            .foreground(ThemeRef::SecondaryText)
            .into();
        let add = icon_button(Icon::symbol(Symbol::Add), {
            let bridge = bridge.clone();
            move || match bridge.pick_workspace_directory() {
                Ok(serde_json::Value::String(path)) => {
                    bridge.spawn_workspace_create(path);
                }
                Ok(_) => {}
                Err(err) => log_diag(&format!("pick workspace directory failed: {err}")),
            }
        })
        .vertical_alignment(VerticalAlignment::Center);
        grid((label.grid_column(0), add.grid_column(1)))
            .columns([GridLength::STAR, GridLength::Auto])
            .margin(Thickness::xy(8.0, 2.0))
            .into()
    };

    // 计数（轻量，不再展开会话列表；会话切 TabView/启动页）
    let (active_items, archived_items): (Vec<SessionItem>, Vec<SessionItem>) =
        items.iter().cloned().partition(|s| !s.archived);
    let ws_count = |ws_id: &str| -> usize {
        active_items
            .iter()
            .filter(|s| s.workspace_id.as_deref() == Some(ws_id))
            .count()
    };
    let ungrouped_count = active_items
        .iter()
        .filter(|s| s.workspace_id.is_none())
        .count();

    // 仅渲染工作区导航 + 未分组 + 归档折叠（无会话展开列表，卡顿主因已移除）
    let mut nav_rows: Vec<Element> = Vec::new();
    for ws in &workspaces {
        nav_rows.push(workspace_row(
            ws,
            current_ws.as_deref() == Some(ws.id.as_str()),
            ws_count(&ws.id),
            bridge.clone(),
            set_ws_confirm.clone(),
            set_ws_rename.clone(),
        ));
    }
    nav_rows.push(ungrouped_row(
        ungrouped_count,
        current_ws.is_none(),
        bridge.clone(),
    ));

    // 提示：当前筛选说明（FD2 Caption）
    let hint: Element = {
        let txt = if let Some(id) = current_ws.as_deref() {
            workspaces
                .iter()
                .find(|w| w.id == id)
                .map(|w| format!("当前：{} · 标签页已过滤", w.title))
                .unwrap_or_else(|| "当前：未分组 · 标签页已过滤".into())
        } else {
            format!("当前：未分组 · {} 个会话", ungrouped_count)
        };
        text_block(txt)
            .font_size(11.0)
            .foreground(ThemeRef::TertiaryText)
            .margin(Thickness::xy(12.0, 4.0))
            .into()
    };

    // 归档：折叠 Expander，内单 ListView（量小，不影响主列表虚拟化）
    let archived_section: Element = if archived_items.is_empty() {
        grid(()).into()
    } else {
        let list_archived = list_view(archived_items.clone(), {
            let bridge = bridge.clone();
            let set_confirm = set_confirm_seed.clone();
            move |item, _| archive_row(item, bridge.clone(), set_confirm.clone())
        })
        .with_key_selector(|item| item.seed.clone())
        .selection_mode(SelectionMode::None)
        .build();
        Expander::new(list_archived)
            .header_content(
                hstack((
                    text_block("归档").font_size(12.0),
                    text_block(format!("{}", archived_items.len()))
                        .font_size(11.0)
                        .foreground(ThemeRef::SecondaryText),
                ))
                .spacing(6.0),
            )
            .margin(Thickness::xy(8.0, 4.0))
            .into()
    };

    let nav_list: Element = scroll_viewer(
        vstack({
            let mut v = Vec::new();
            v.push(group_label);
            v.extend(nav_rows);
            v.push(hint);
            v.push(archived_section);
            v
        })
        .spacing(2.0),
    )
    .into();

    let footer: Element = {
        let sp: Element = hstack((
            button("技能")
                .icon(Icon::symbol(Symbol::Library))
                .subtle()
                .on_click({
                    let bridge = bridge.clone();
                    move || bridge.navigate("skills", None)
                }),
            button("设置")
                .icon(Icon::symbol(Symbol::Setting))
                .subtle()
                .on_click({
                    let bridge = bridge.clone();
                    move || bridge.navigate("settings", None)
                }),
        ))
        .into();
        let cached_label = build_label.borrow().clone();
        let label = match cached_label {
            Some(s) => s,
            None => {
                let s = build_id_label();
                *build_label.borrow_mut() = Some(s.clone());
                s
            }
        };
        let version: Element = text_block(&label)
            .font_size(10.0)
            .foreground(ThemeRef::TertiaryText)
            .max_lines(1)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .tooltip(format!(
                "构建标识：{label}\n格式：<版本>-<commit12>-<UTC yyyyMMddHHmmss>"
            ))
            .automation_name("构建版本")
            .into();
        vstack((sp, version)).spacing(4.0).margin(12.0).into()
    };

    let content: Element = grid((
        brand.grid_row(0).grid_column(0),
        actions.grid_row(1).grid_column(0),
        nav_list.grid_row(2).grid_column(0),
        footer.grid_row(3).grid_column(0),
    ))
    .rows([
        GridLength::Auto,
        GridLength::Auto,
        GridLength::STAR,
        GridLength::Auto,
    ])
    .into();

    let bar: Element = border(text_block(""))
        .width(if splitter_hover { 2.0 } else { 1.0 })
        .background(if splitter_hover {
            ThemeRef::AccentSecondary
        } else {
            ThemeRef::DividerStroke
        })
        .horizontal_alignment(HorizontalAlignment::Center)
        .into();
    let splitter: Element = border(bar)
        .width(12.0)
        .capture_pointer_on_press()
        .on_pointer_pressed({
            let drag_start = drag_start.clone();
            move |info: PointerEventInfo| {
                *drag_start.borrow_mut() = Some((info.window_x, width));
            }
        })
        .on_pointer_moved({
            let drag_start = drag_start.clone();
            let set_width = set_width.clone();
            move |info: PointerEventInfo| {
                if !info.is_left_button_pressed {
                    return;
                }
                let Some((sx, sw)) = *drag_start.borrow() else {
                    return;
                };
                let new_w = (sw + (info.window_x - sx)).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                if (new_w - sw).abs() >= 2.0 {
                    set_width.call(new_w);
                }
            }
        })
        .on_pointer_released({
            let drag_start = drag_start.clone();
            move |_| *drag_start.borrow_mut() = None
        })
        .on_pointer_capture_lost({
            let drag_start = drag_start.clone();
            move || *drag_start.borrow_mut() = None
        })
        .on_pointer_entered({
            let set_hover = set_splitter_hover.clone();
            move |_| set_hover.call(true)
        })
        .on_pointer_exited({
            let set_hover = set_splitter_hover.clone();
            move || set_hover.call(false)
        })
        .on_tapped({
            let set_width = set_width.clone();
            move || set_width.call(SIDEBAR_DEFAULT_WIDTH)
        })
        .into();

    let dialog: Element = match confirm_seed.clone() {
        Some(seed) => {
            let bridge = bridge.clone();
            let set_confirm = set_confirm_seed.clone();
            ContentDialog::new("彻底删除会话")
                .content("将删除该会话及其全部消息文件，不可恢复。\n\n归档会话请使用标签页的关闭按钮（×）。")
                .primary_button_text("彻底删除")
                .close_button_text("取消")
                .is_open(true)
                .on_closed(move |result: ContentDialogResult| {
                    if result == ContentDialogResult::Primary {
                        bridge.spawn_delete(&seed);
                    }
                    set_confirm.call(None);
                })
                .into()
        }
        None => grid(()).into(),
    };

    let ws_delete_dialog: Element = match ws_confirm.clone() {
        Some(id) => {
            let bridge = bridge.clone();
            let set_ws_confirm = set_ws_confirm.clone();
            ContentDialog::new("删除工作区")
                .content("删除该工作区分组。其下会话不会被删除，将移入「未分组」。")
                .primary_button_text("删除")
                .close_button_text("取消")
                .is_open(true)
                .on_closed(move |result: ContentDialogResult| {
                    if result == ContentDialogResult::Primary {
                        bridge.spawn_workspace_delete(id.clone());
                    }
                    set_ws_confirm.call(None);
                })
                .into()
        }
        None => grid(()).into(),
    };

    let ws_rename_dialog: Element = match ws_rename.clone() {
        Some((id, current_title)) => {
            let bridge = bridge.clone();
            let set_ws_rename = set_ws_rename.clone();
            let input_ref = ws_rename_input.clone();
            *input_ref.borrow_mut() = current_title.clone();
            let input: Element = text_box(input_ref.borrow().clone())
                .on_text_changed({
                    let input_ref = input_ref.clone();
                    move |v| *input_ref.borrow_mut() = v
                })
                .width(280.0)
                .into();
            let confirm = button("确定").on_click({
                let bridge = bridge.clone();
                let set_ws_rename = set_ws_rename.clone();
                let input_ref = input_ref.clone();
                move || {
                    let title = input_ref.borrow().trim().to_string();
                    if !title.is_empty() {
                        bridge.spawn_workspace_rename(id.clone(), title);
                    }
                    set_ws_rename.call(None);
                }
            });
            let cancel = button("取消").on_click({
                let set_ws_rename = set_ws_rename.clone();
                move || set_ws_rename.call(None)
            });
            let card: Element = {
                let title_el: Element = text_block("重命名工作区")
                    .semibold()
                    .font_size(14.0)
                    .into();
                let buttons: Element = hstack((cancel, confirm))
                    .spacing(8.0)
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .into();
                border(vstack((title_el, input, buttons)).spacing(12.0))
                    .corner_radius(8.0)
                    .background(ThemeRef::CardBackground)
                    .border_brush(ThemeRef::CardStroke)
                    .border_thickness(Thickness::xy(1.0, 1.0))
                    .padding(Thickness::xy(16.0, 16.0))
                    .into()
            };
            grid((card,))
                .columns([GridLength::STAR])
                .rows([GridLength::STAR])
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .into()
        }
        None => grid(()).into(),
    };

    grid((
        grid((content.grid_column(0), splitter.grid_column(1)))
            .columns([GridLength::STAR, GridLength::Pixel(12.0)])
            .rows([GridLength::STAR])
            .grid_row(0)
            .grid_column(0),
        dialog.grid_row(0).grid_column(0),
        ws_delete_dialog.grid_row(0).grid_column(0),
        ws_rename_dialog.grid_row(0).grid_column(0),
    ))
    .rows([GridLength::STAR])
    .into()
}
