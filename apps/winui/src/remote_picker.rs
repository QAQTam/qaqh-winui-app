//! 远端文件选择器（临时跨端模式）：覆盖层 + 目录浏览 + 文件预览 + 选择目录。
//!
//! - 数据源：`bridge.core().fs_listing_snapshot()` / `fs_preview_snapshot()`，
//!   由 bridge 侧 `spawn_fs_list` / `spawn_fs_read` 经 `fs.list`/`fs.read` 拉取；
//! - 显示层：路径统一走 `display_remote_path` → `//ip/...`；点选返回的仍是
//!   daemon 侧路径（由 header 直接 `workspace.set`，不在壳侧做本地 FS 操作）；
//! - 与 diff_drawer 同款「静态槽 + 轮询 + 全屏覆盖层」模式。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_reactor::*;

use crate::bridge::{Bridge, RemoteFsListing, RemoteFsPreview};

/// 轮询间隔：静态槽检测 + 列表/preview rev 比对（轻量，80ms 足够）。
const POLL_INTERVAL: Duration = Duration::from_millis(80);
const SCRIM_ALPHA: u8 = 140;
const PICKER_W: f64 = 720.0;
const PICKER_H: f64 = 560.0;
const LIST_WIDTH: f64 = 320.0;

/// 静态槽：Some(初始路径) = 打开选择器（header 工作区按钮在远端模式下写入）。
pub static PICKER_SLOT: Mutex<Option<String>> = Mutex::new(None);

/// 写端：打开远端选择器（初始目录为空时从根开始）。
pub fn open_remote_picker(initial_path: String) {
    if let Ok(mut slot) = PICKER_SLOT.lock() {
        *slot = Some(if initial_path.is_empty() {
            "/".to_string()
        } else {
            initial_path
        });
    }
}

fn close_remote_picker() {
    if let Ok(mut slot) = PICKER_SLOT.lock() {
        *slot = None;
    }
}

/// daemon 侧路径取父目录（兼容 Unix `/home/u` 与 Windows `F:/a/b`）。
fn parent_dir(path: &str) -> String {
    let norm = path.trim_end_matches(['/', '\\']).replace('\\', "/");
    if norm.is_empty() || norm == "/" {
        return norm;
    }
    if let Some((drive, rest)) = norm.split_once(":/") {
        let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return format!("{drive}:/");
        }
        let parent = segments[..segments.len() - 1].join("/");
        return if parent.is_empty() {
            format!("{drive}:/")
        } else {
            format!("{drive}:/{parent}")
        };
    }
    match norm.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => norm.get(..idx).unwrap_or("/").to_string(),
        None => "/".to_string(),
    }
}

/// 读端覆盖层：无请求/未打开时空 grid 穿透。
pub fn remote_picker_overlay(cx: &mut RenderCx, bridge: Arc<Bridge>) -> Element {
    let (open, set_open) = cx.use_state::<bool>(false);
    let (current_path, set_current_path) = cx.use_state::<String>("/".to_string());
    let (listing, set_listing) = cx.use_state::<RemoteFsListing>(RemoteFsListing::default());
    let (preview, set_preview) = cx.use_state::<Option<RemoteFsPreview>>(None);
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let list_timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_listing_rev = cx.use_ref::<u64>(0);
    let last_preview_rev = cx.use_ref::<u64>(0);

    // 静态槽轮询：请求出现 → 打开 + 载入初始路径。
    cx.use_effect((), {
        let set_open = set_open.clone();
        let set_current_path = set_current_path.clone();
        let timer = timer.clone();
        move || {
            if timer.borrow().is_some() {
                return;
            }
            if let Ok(t) = DispatcherTimer::new(POLL_INTERVAL, move || {
                if let Ok(mut slot) = PICKER_SLOT.lock()
                    && let Some(path) = slot.as_ref()
                {
                    let path = path.clone();
                    *slot = None;
                    set_current_path.call(path);
                    set_open.call(true);
                }
            }) {
                *timer.borrow_mut() = Some(t);
            }
        }
    });

    // 列表/preview rev 轮询：bridge 侧异步任务写回后驱动刷新。
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_listing = set_listing.clone();
        let set_preview = set_preview.clone();
        let last_listing_rev = last_listing_rev.clone();
        let last_preview_rev = last_preview_rev.clone();
        let list_timer = list_timer.clone();
        move || {
            if list_timer.borrow().is_some() {
                return;
            }
            if let Ok(t) = DispatcherTimer::new(POLL_INTERVAL, move || {
                let (listing, rev) = bridge.core().fs_listing_snapshot();
                if rev != *last_listing_rev.borrow() {
                    *last_listing_rev.borrow_mut() = rev;
                    set_listing.call(listing);
                }
                let (preview, prev) = bridge.core().fs_preview_snapshot();
                if prev != *last_preview_rev.borrow() {
                    *last_preview_rev.borrow_mut() = prev;
                    set_preview.call(preview);
                }
            }) {
                *list_timer.borrow_mut() = Some(t);
            }
        }
    });

    // 打开/切目录 → 拉取该目录的 fs.list。
    cx.use_effect((open, current_path.clone()), {
        let bridge = bridge.clone();
        let path = current_path.clone();
        move || {
            if open {
                bridge.core().spawn_fs_list(path.clone());
            }
        }
    });

    // ⚠ hooks 全部在条件分支之前。
    if !open {
        return grid(()).into();
    }

    let on_close = Callback::new({
        let set_open = set_open.clone();
        let set_preview = set_preview.clone();
        move |_: ()| {
            set_open.call(false);
            close_remote_picker();
            set_preview.call(None);
        }
    });

    // 目录行：目录 → 进入；文件 → 预览。
    let rows: Vec<Element> = listing
        .entries
        .iter()
        .map(|entry| {
            let is_dir = entry.is_dir;
            let name = entry.name.clone();
            let path = entry.path.clone();
            let sub = if is_dir {
                "目录".to_string()
            } else {
                format_size(entry.size)
            };
            border(
                hstack((
                    text_block(if is_dir { "📁" } else { "📄" }).font_size(14.0),
                    text_block(&name)
                        .font_size(12.0)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .max_width(LIST_WIDTH - 110.0),
                    text_block(sub)
                        .font_size(11.0)
                        .foreground(ThemeRef::SecondaryText),
                ))
                .spacing(8.0)
                .padding(Thickness::xy(10.0, 7.0)),
            )
            .background(ThemeRef::LayerFill)
            .corner_radius(4.0)
            .on_tapped({
                let bridge = bridge.clone();
                let set_current_path = set_current_path.clone();
                move || {
                    if is_dir {
                        set_current_path.call(path.clone());
                    } else {
                        bridge.core().spawn_fs_read(path.clone());
                    }
                }
            })
            .into()
        })
        .collect();

    let list_body: Element = if listing.loading {
        text_block("加载中…")
            .font_size(12.0)
            .foreground(ThemeRef::SecondaryText)
            .into()
    } else if let Some(error) = &listing.error {
        text_block(error)
            .font_size(12.0)
            .foreground(ThemeRef::SystemCritical)
            .into()
    } else if rows.is_empty() {
        text_block("空目录")
            .font_size(12.0)
            .foreground(ThemeRef::SecondaryText)
            .into()
    } else {
        scroll_viewer(vstack(rows).spacing(2.0)).into()
    };

    // 预览区（文件点击后显示 fs.read 文本）。
    let preview_body: Element = match &preview {
        Some(p) => scroll_viewer(
            vstack((
                text_block(p.path.clone())
                    .font_size(11.0)
                    .foreground(ThemeRef::SecondaryText)
                    .text_trimming(TextTrimming::CharacterEllipsis),
                if p.truncated {
                    text_block("（内容已截断，仅预览前 64 KiB）")
                        .font_size(11.0)
                        .foreground(ThemeRef::SecondaryText)
                } else {
                    text_block("")
                },
                text_block(p.content.clone()).font_size(12.0),
            ))
            .spacing(4.0),
        )
        .into(),
        None => text_block("点击左侧文件预览内容；点击目录进入。")
            .font_size(12.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    };

    let display_path = bridge.core().display_remote_path(&current_path);
    let esc = KeyboardAccelerator::new(VirtualKey::Escape, VirtualKeyModifiers::None, {
        let on_close = on_close.clone();
        move || on_close.invoke(())
    });

    let card: Element = border(
        grid((
            // 头部
            hstack((
                text_block("远端目录选择").font_size(14.0).semibold(),
                text_block(&display_path)
                    .font_size(12.0)
                    .foreground(ThemeRef::SecondaryText)
                    .text_trimming(TextTrimming::CharacterEllipsis),
                button("✕ 关闭")
                    .on_click(on_close.clone())
                    .horizontal_alignment(HorizontalAlignment::Right),
            ))
            .spacing(12.0)
            .padding(Thickness::xy(16.0, 12.0))
            .grid_row(0),
            // 主体：左目录列表 / 右预览
            grid((
                border(list_body)
                    .background(ThemeRef::LayerFill)
                    .grid_column(0),
                border(preview_body)
                    .padding(Thickness::xy(12.0, 8.0))
                    .grid_column(1),
            ))
            .columns([GridLength::Pixel(LIST_WIDTH), GridLength::STAR])
            .rows([GridLength::STAR])
            .grid_row(1),
            // 底部：上级 / 选择当前目录
            hstack((
                button("上级目录").subtle().on_click({
                    let set_current_path = set_current_path.clone();
                    let path = current_path.clone();
                    move || set_current_path.call(parent_dir(&path))
                }),
                text_block("选择此目录后写入当前会话 workspace")
                    .font_size(11.0)
                    .foreground(ThemeRef::SecondaryText),
                button("选择此目录").accent().on_click({
                    let bridge = bridge.clone();
                    let path = current_path.clone();
                    let on_close = on_close.clone();
                    move || {
                        bridge.core().spawn_workspace_set(path.clone());
                        on_close.invoke(());
                    }
                }),
            ))
            .spacing(12.0)
            .padding(Thickness::xy(16.0, 10.0))
            .grid_row(2),
        ))
        .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .width(PICKER_W)
        .height(PICKER_H),
    )
    .background(ThemeRef::SolidBackground)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(8.0)
    .keyboard_accelerator(esc)
    .horizontal_alignment(HorizontalAlignment::Center)
    .vertical_alignment(VerticalAlignment::Center)
    .with_key("remote-picker-card")
    .into();

    grid((card,))
        .rows([GridLength::STAR])
        .columns([GridLength::STAR])
        .background(Color {
            a: SCRIM_ALPHA,
            r: 0,
            g: 0,
            b: 0,
        })
        .with_key("remote-picker-overlay")
        .into()
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
