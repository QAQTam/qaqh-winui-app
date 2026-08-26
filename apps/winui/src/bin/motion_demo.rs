//! F-N11 动画补丁验证：布局动画（Size/Offset 合成层隐式补间）。
//!
//! 运行：`cargo run -p qaqh-winui --bin motion_demo`（人工模式）
//! 自动验收：`MOTION_MODE=size|offset|none|both cargo run ...`
//!   —— 启动后定时器自行触发动画序列，7 秒后以退出码 0 自杀；
//!      中途崩溃（stowed exception）则进程提前非零退出。
//! 环境变量 MOTION_MODE 控制挂载哪些动画键，用于二分定位。

use windows_reactor::*;

fn main() -> windows_reactor::Result<()> {
    let mode = std::env::var("MOTION_MODE").unwrap_or_else(|_| "manual".into());
    println!("motion_demo mode={mode}");
    App::new()
        .title("F-N11 布局动画演示")
        .inner_size(560.0, 480.0)
        .render(move |cx| render(cx, &mode))
}

fn animate_size(mode: &str) -> bool {
    matches!(mode, "size" | "both" | "manual")
}

fn animate_offset(mode: &str) -> bool {
    matches!(mode, "offset" | "both" | "manual")
}

fn render(cx: &mut RenderCx, mode: &str) -> Element {
    let mode: &str = Box::leak(mode.to_string().into_boxed_str());
    // async_state：set 可从工作线程调用（自动验收序列用）。
    let (big, set_big) = cx.use_async_state::<bool>(false);
    let (flipped, set_flipped) = cx.use_async_state::<bool>(false);

    if mode != "manual" {
        // ── 自动验收序列：1s 触发 Size、2.5s 复原、4s 触发 Offset、
        //    5.5s 复原、7s 存活退出（exit 0）。任一阶段 stowed 崩溃 =
        //    进程非零死亡，退出码即判据。──
        let set_big = set_big.clone();
        let set_big2 = set_big.clone();
        let set_flipped = set_flipped.clone();
        let set_flipped2 = set_flipped.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            eprintln!("[t+1.0s] trigger SIZE");
            set_big.call(true);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            eprintln!("[t+2.5s] restore SIZE");
            set_big2.call(false);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            eprintln!("[t+4.0s] trigger OFFSET");
            set_flipped.call(true);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            eprintln!("[t+5.5s] restore OFFSET");
            set_flipped2.call(false);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            eprintln!("SURVIVED all phases");
            std::process::exit(0);
        });
    }

    let big_v = big;
    let flipped_v = flipped;
    let card_w = if big_v { 340.0 } else { 160.0 };
    let card_h = if big_v { 220.0 } else { 100.0 };

    let chip_names: [&str; 3] = if flipped_v {
        ["丙 · 3", "乙 · 2", "甲 · 1"]
    } else {
        ["甲 · 1", "乙 · 2", "丙 · 3"]
    };

    let chips: Vec<Element> = chip_names
        .iter()
        .map(|name| {
            let mut chip = border(text_block(*name).font_size(13.0))
                .background(ThemeRef::AccentSecondary)
                .corner_radius(14.0)
                .padding(Thickness::xy(16.0, 8.0));
            if animate_offset(mode) {
                chip = chip.with_layout_animation(LayoutAnimationConfig {
                    animate_offset: true,
                    animate_size: false,
                    ..LayoutAnimationConfig::default()
                });
            }
            chip.into()
        })
        .collect();

    let mut card = border(
        text_block(if big_v {
            "大卡片 340×220"
        } else {
            "小卡片 160×100"
        })
        .foreground(ThemeRef::PrimaryText),
    )
    .width(card_w)
    .height(card_h)
    .background(ThemeRef::AccentSecondary)
    .corner_radius(12.0);
    if animate_size(mode) {
        card = card.with_layout_animation(LayoutAnimationConfig {
            animate_size: true,
            animate_offset: false,
            ..LayoutAnimationConfig::default()
        });
    }
    let card_el: Element = card
        .on_tapped({
            let set_big = set_big.clone();
            move || set_big.call(!big_v)
        })
        .into();

    vstack((
        text_block(format!(
            "mode={mode}｜点击卡片测 Size，「换位」测 Offset。自动模式下 7s 自验。"
        ))
        .font_size(12.0)
        .foreground(ThemeRef::SecondaryText),
        card_el,
        button("换位").on_click({
            let set_flipped = set_flipped.clone();
            move || set_flipped.call(!flipped_v)
        }),
        hstack(chips).spacing(12.0),
    ))
    .spacing(24.0)
    .padding(Thickness::xy(32.0, 28.0))
    .into()
}
