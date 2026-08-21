use std::sync::Mutex;

use windows_reactor::*;

use super::*;

/// 从 SVG 头部解析 `width`/`height` 属性（usvg 输出格式固定为
/// `<svg width=".." height=".."`）。解析失败回退 800x600。
fn svg_natural_size(svg: &str) -> (f64, f64) {
    let head = svg.get(..svg.len().min(300)).unwrap_or(svg);
    let mut w = 800.0;
    let mut h = 600.0;
    if let Some(i) = head.find("width=\"") {
        if let Some(v) = head
            .get(i + 7..)
            .and_then(|rest| rest.split('"').next())
            .and_then(|s| s.parse::<f64>().ok())
        {
            w = v;
        }
    }
    if let Some(i) = head.find("height=\"") {
        if let Some(v) = head
            .get(i + 8..)
            .and_then(|rest| rest.split('"').next())
            .and_then(|s| s.parse::<f64>().ok())
        {
            h = v;
        }
    }
    (w, h)
}

pub static DIAGRAM_ZOOM: Mutex<Option<DiagramZoomRequest>> = Mutex::new(None);

/// 图表放大弹窗：视口占主窗口比例（跟随窗口尺寸动态变化）与遮罩透明度。
const DIAGRAM_ZOOM_W_RATIO: f64 = 0.78;
const DIAGRAM_ZOOM_H_RATIO: f64 = 0.72;
const SCRIM_ALPHA: u8 = 140;

/// 图表点击 → 写入放大请求（覆盖层轮询消费）。
/// `Callback<()>` 的闭包签名为 `Fn(())`（reactor 约定，invoke 传 unit）。
pub(super) fn zoom_request_callback(
    diagram: &markdown_winui::DiagramBlock,
    scheme: ColorScheme,
    key: &str,
) -> windows_reactor::Callback<()> {
    let label = key.to_string();
    let svg = diagram.svg(scheme).unwrap_or_default().to_string();
    let (width, height) = svg_natural_size(&svg);
    windows_reactor::Callback::new(move |_: ()| {
        if let Ok(mut slot) = DIAGRAM_ZOOM.lock() {
            *slot = Some(DiagramZoomRequest {
                label: label.clone(),
                svg: svg.clone(),
                width,
                height,
            });
        }
    })
}

/// XAML 提交批次：事件泵由 `CompositionTarget::Rendering`（vsync 对齐）
/// 驱动——60Hz 屏 60 次/s、120Hz 屏 120 次/s，天然跟随客户显示器刷新
/// 率（DispatcherTimer 受系统时钟 15.6ms 粒度限制，最多 ~64fps，
/// 120Hz 屏只有一半帧率——"渲染慢"吐槽的根因）。队列中的 token delta
/// 先合并，再在 UI 线程的一次 retained-mode 更新中提交；不逐 token
/// 触碰控件树。
///
/// 跟随尾部滚动请求节流：live 流式期间 100ms 一次贴底请求。vsync 泵每
/// 帧都请求会让滚动与用户滚轮/滚动条抢占，并形成"滚动 → 行 realize
/// → 渲染 → 再滚动"反馈循环，UI 线程满载（表现为滚动条卡死）；100ms
/// 是经实机验证的折中（原 50ms 在长文本流式下仍会触发卡顿）。结构性
/// 变化（restore / 新 turn / round 完成）不受此限，立即滚底。

/// 图表放大覆盖层（挂 main.rs 覆盖层 grid，P-6 同 cell 模式）。
///
/// 轮询 [`DIAGRAM_ZOOM`] 静态槽（写端在 `final_view` 的图表点击回调），
/// 80ms 周期；请求出现即弹开全窗实底大图，点遮罩/关闭按钮清除。
/// 与 interaction_overlay 同款「写端组件 / 读端覆盖层」分离模式。
pub fn diagram_zoom_overlay(cx: &mut RenderCx) -> Element {
    // ⚠ hooks 必须在所有条件分支之前（React 规则：zoom=None 提前 return
    // 时顺序不能变，否则状态错位导致按钮/状态更新不生效）。
    let (zoom, set_zoom) = cx.use_state::<Option<DiagramZoomRequest>>(None);
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    // 视口跟随主窗口尺寸：宽 78%、高 72%（clamp 到合理范围，窗口极小时不崩）。
    let win = cx.use_inner_size();
    let view_w = (win.width * DIAGRAM_ZOOM_W_RATIO).clamp(420.0, 1100.0);
    let view_h = (win.height * DIAGRAM_ZOOM_H_RATIO).clamp(320.0, 760.0);

    cx.use_effect((), {
        let set_zoom = set_zoom.clone();
        let timer = timer.clone();
        move || {
            if timer.borrow().is_some() {
                return;
            }
            if let Ok(t) = DispatcherTimer::new(Duration::from_millis(80), move || {
                if let Ok(slot) = DIAGRAM_ZOOM.lock()
                    && let Some(req) = slot.as_ref()
                {
                    let req = req.clone();
                    set_zoom.call(Some(req));
                }
            }) {
                *timer.borrow_mut() = Some(t);
            }
        }
    });

    let Some(req) = zoom else {
        // 无请求：空 grid（无背景 → 不参与命中测试，点击穿透，同 splash）。
        return grid(()).into();
    };

    // 关闭：清本地 state + 清静态槽（下次点击同一图表可重新弹开）。
    let close = {
        let set_zoom = set_zoom.clone();
        move || {
            set_zoom.call(None);
            if let Ok(mut slot) = DIAGRAM_ZOOM.lock() {
                *slot = None;
            }
        }
    };

    // fit 基准：完整图缩放进视口（小图不放大），原生 ZoomMode 在此基础上
    // 放大（Ctrl+滚轮 / 触摸捏合），放大后由 ScrollViewer 滚动查看细节。
    let fit = (view_w / req.width.max(1.0))
        .min(view_h / req.height.max(1.0))
        .min(1.0);
    let img_w = (req.width * fit).max(1.0);
    let img_h = (req.height * fit).max(1.0);

    let esc =
        KeyboardAccelerator::new(VirtualKey::Escape, VirtualKeyModifiers::None, close.clone());
    let card: Element = border(
        vstack((
            hstack((
                text_block(&req.label)
                    .semibold()
                    .vertical_alignment(VerticalAlignment::Center),
                button("✕ 关闭")
                    .on_click(close.clone())
                    .vertical_alignment(VerticalAlignment::Center),
            ))
            .spacing(12.0),
            scroll_viewer(
                border(
                    Image::new(ImageSource::svg(req.svg))
                        .stretch(Stretch::Uniform)
                        .width(img_w)
                        .height(img_h),
                )
                .background(ThemeRef::CardBackground)
                .padding(12.0),
            )
            .width(view_w)
            .height(view_h)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            // 原生缩放：ScrollViewerZoomMode::Enabled（0=Disabled, 1=Enabled）
            .zoom_mode(1)
            .min_zoom_factor(1.0)
            .max_zoom_factor(4.0),
            text_block("Ctrl+滚轮 / 双指捏合缩放 · Esc 关闭")
                .font_size(11.0)
                .foreground(ThemeRef::SecondaryText)
                .horizontal_alignment(HorizontalAlignment::Center),
        ))
        .spacing(12.0)
        .padding(20.0),
    )
    .background(ThemeRef::SolidBackground)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(8.0)
    .keyboard_accelerator(esc)
    .horizontal_alignment(HorizontalAlignment::Center)
    .vertical_alignment(VerticalAlignment::Center)
    .into();

    grid((card,))
        .rows([GridLength::STAR])
        .columns([GridLength::STAR])
        // 半透明遮罩仅作背景降噪（不再拦截点击关闭——卡片内滚动会误触）。
        .background(Color {
            a: SCRIM_ALPHA,
            r: 0,
            g: 0,
            b: 0,
        })
        .with_key("diagram-zoom-overlay")
        .into()
}
