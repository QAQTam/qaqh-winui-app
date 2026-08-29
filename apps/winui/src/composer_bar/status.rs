use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_reactor::*;

use qaqh_fluent::{motion, shimmer_runs, tokens};

use crate::bridge::{ComposerState, SubagentItem, SubagentState, WorkPhase};

/// 队列行："n 条后续任务已排队" + items + 删除。
// ── 工作状态栏（输入框之上；Codex loading-shimmer 气质复刻）───────
// shimmer 实现已提取到 qaqh_fluent::{shimmer_runs}（chat_view 的
// 加载覆盖层复用同一光带风格），本文件仅引用。

/// 工作状态栏：转圈 + 俏皮文案（shimmer 流动）。空闲时零高占位。
///
/// **独立组件边界**：tick 的 use_async_state 在组件内部——90ms 步进只
/// 重渲染本组件子树，不再连带重建整个 composer（此前挂在 composer
/// render 链上，每 tick 全量 diff 输入框/footer/context 条 → UI 线程
/// 卡顿，ChatView 16ms 泵被拖）。props=WorkPhase（Clone+PartialEq），
/// 阶段变化才触发组件重渲染。
/// 工作状态区 props：父代理阶段 + 子代理胶囊（任一变化才重渲染组件）。
#[derive(Debug, Clone, PartialEq)]
struct WorkStatusProps {
    phase: WorkPhase,
    subagents: Vec<SubagentItem>,
}

/// 工作状态区：父代理状态行（转圈 + 俏皮文案 shimmer，现有单行）+
/// 子代理胶囊行（WrapLayout 语义：hstack + 上限折叠）。空闲/无胶囊时零高占位。
///
/// **独立组件边界**：父行 shimmer 90ms tick 与胶囊 1s 时钟都在组件内部——
/// 只重渲染本组件子树；props=WorkStatusProps（Clone+PartialEq），
/// phase 或 subagents 变化才触发重渲染。胶囊**不逐卡开线程**（共用 1s 时钟）。
pub(crate) fn work_status_bar(cx: &mut RenderCx, state: &ComposerState) -> Element {
    let _ = cx;
    component(
        work_status_component,
        WorkStatusProps {
            phase: state.phase.clone(),
            subagents: state.subagents.clone(),
        },
    )
}

/// 组件内部实现：父行 label 推导 + shimmer tick + 子代理胶囊 + 渲染。
fn work_status_component(props: &WorkStatusProps, cx: &mut RenderCx) -> Element {
    let label: String = match &props.phase {
        WorkPhase::Idle => String::new(),
        WorkPhase::Thinking => "飞速思考中…".into(),
        WorkPhase::Answering => "奋力回答中…".into(),
        WorkPhase::Tool(name) => format!("探索中…正在调用「{name}」"),
        WorkPhase::WaitingUser => "等待你确认…".into(),
    };
    let active = !label.is_empty();
    let has_pills = !props.subagents.is_empty();

    // 父行 shimmer 阶梯步进（90ms/步，仿 Codex steps(120) 的离散跳动感）；
    // 仅父行活动时启动线程（deps=props，阶段变化重启）。
    let (tick, set_tick) = cx.use_async_state::<u32>(0);
    let label_len = label.chars().count() as u32;
    cx.use_effect_with_cleanup((props.clone(),), {
        let set_tick = set_tick.clone();
        move || -> Option<Box<dyn FnOnce()>> {
            if !active {
                return None;
            }
            let period = label_len + 8;
            let stop = Arc::new(AtomicBool::new(false));
            let s2 = stop.clone();
            std::thread::spawn(move || {
                let mut t = 0u32;
                while !s2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(90));
                    t = (t + 1) % period;
                    set_tick.call(t);
                }
            });
            Some(Box::new(move || stop.store(true, Ordering::Relaxed)))
        }
    });

    // 胶囊共用 1s 时钟：仅存在胶囊时启动（刷新耗时文本，不逐卡开线程）。
    let (sec_tick, set_sec_tick) = cx.use_async_state::<u32>(0);
    cx.use_effect_with_cleanup((has_pills,), {
        let set_sec_tick = set_sec_tick.clone();
        move || -> Option<Box<dyn FnOnce()>> {
            if !has_pills {
                return None;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let s2 = stop.clone();
            std::thread::spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(1));
                    set_sec_tick.call(0);
                }
            });
            Some(Box::new(move || stop.store(true, Ordering::Relaxed)))
        }
    });
    let _ = sec_tick;
    let now = unix_ms();

    // 空闲整行不挂载（零高 grid(()) 会在 vstack spacing 两侧产生幻影空隙；
    // 视图层另有空闲短路，这里是 idle+有胶囊路径的兜底）。
    let parent_row: Element = if !active {
        Element::Empty
    } else {
        let runs = shimmer_runs(&label, tick as f64 - 4.0);
        hstack((
            ProgressRing::indeterminate().width(14.0).height(14.0),
            RichTextBlock::single_paragraph(runs).font_size(tokens::TYPE_CAPTION),
        ))
        .spacing(tokens::SPACE_2)
        .padding(Thickness {
            left: tokens::SPACE_2,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        })
        .horizontal_alignment(HorizontalAlignment::Left)
        .with_key("work-status-bar")
        .into()
    };

    // 子代理胶囊行（第二行；空 = 不挂载，与幻影空隙消除口径一致）。
    let pills_row: Element = if !has_pills {
        Element::Empty
    } else {
        let mut items: Vec<Element> = props
            .subagents
            .iter()
            .map(|item| subagent_pill(item, now))
            .collect();
        const PILL_CAP: usize = 8;
        let extra = items.len().saturating_sub(PILL_CAP);
        items.truncate(PILL_CAP);
        if extra > 0 {
            items.push(
                text_block(&format!("+{extra}"))
                    .font_size(tokens::TYPE_CAPTION)
                    .into(),
            );
        }
        hstack(items)
            .spacing(tokens::SPACE_2)
            .horizontal_alignment(HorizontalAlignment::Left)
            .with_key("subagent-pill-row")
            .into()
    };

    vstack((parent_row, pills_row))
        .spacing(tokens::SPACE_1)
        .into()
}

/// 单个子代理胶囊：状态图标 + 名称 + 耗时 mm:ss；done 降透明。
/// 点击打开子代理运行记录面板（seed 可用时；拉 timeline 按行渲染）。
fn subagent_pill(item: &SubagentItem, now: u64) -> Element {
    let glyph: Element = match item.state {
        SubagentState::Working => ProgressRing::indeterminate()
            .width(10.0)
            .height(10.0)
            .into(),
        SubagentState::Done => text_block("✓").font_size(tokens::TYPE_CAPTION).into(),
        SubagentState::Error => text_block("✗").font_size(tokens::TYPE_CAPTION).into(),
        SubagentState::Timeout => text_block("⏱").font_size(tokens::TYPE_CAPTION).into(),
        SubagentState::Cancelled => text_block("⊘").font_size(tokens::TYPE_CAPTION).into(),
        SubagentState::Lost => text_block("⚠").font_size(tokens::TYPE_CAPTION).into(),
    };
    let label = format!("{} · {}", item.name, fmt_elapsed(item, now));
    let pill = border(
        hstack((glyph, text_block(&label).font_size(tokens::TYPE_CAPTION)))
            .spacing(tokens::SPACE_1)
            .padding(Thickness {
                left: tokens::SPACE_2,
                top: 2.0,
                right: tokens::SPACE_2,
                bottom: 2.0,
            }),
    )
    .corner_radius(10.0)
    .background(ThemeRef::CardBackground)
    .with_key(format!("subagent-pill-{}", item.call_id));
    // 有 seed 才可点开记录面板（seed 未解析时保持纯展示）。
    if item.seed.is_empty() {
        return pill.into();
    }
    pill.on_tapped({
        let name = item.name.clone();
        let seed = item.seed.clone();
        move || {
            crate::diff_drawer::open_diff_drawer(crate::diff_drawer::DrawerRequest::Subagent {
                seed: seed.clone(),
                name: name.clone(),
            })
        }
    })
    .into()
}

/// 耗时 mm:ss：运行中 = started_at → 现在；终态 = started_at → finished_at。
fn fmt_elapsed(item: &SubagentItem, now: u64) -> String {
    let end = if item.finished_at > 0 {
        item.finished_at
    } else {
        now
    };
    let secs = end.saturating_sub(item.started_at) / 1000;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// 当前 unix 时间（epoch ms；同 bridge::unix_ms 语义）。
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn queue_row(
    state: &ComposerState,
    on_queue_remove: Arc<dyn Fn(String) + 'static>,
) -> Element {
    let mut items: Vec<Element> = Vec::new();
    for (i, item) in state.queue_items.iter().enumerate() {
        let remove_name = format!("移除后续任务 {}", item.text);
        let row: Element = hstack((
            text_block(&item.text)
                .font_size(tokens::TYPE_CAPTION)
                .foreground(ThemeRef::SecondaryText)
                .wrap(),
            button("")
                .icon(Symbol::Cancel)
                .subtle()
                .tooltip(remove_name.clone())
                .automation_name(remove_name)
                .automation_id(format!("composer-queue-remove-{}", item.id))
                .on_click({
                    let on_queue_remove = on_queue_remove.clone();
                    let id = item.id.clone();
                    move || on_queue_remove(id.clone())
                }),
        ))
        .spacing(8.0)
        .into();
        items.push(row.with_key(format!("q-{i}-{}", item.id)));
    }
    border(
        vstack((
            text_block(format!("{} 条后续任务已排队", state.queue_count))
                .font_size(tokens::TYPE_CAPTION)
                .semibold(),
            vstack(items).spacing(4.0),
        ))
        .spacing(6.0)
        .padding(10.0),
    )
    .corner_radius(6.0)
    .background(ThemeRef::CardBackground)
    .transition(motion::reveal(), motion::content_exit())
    .with_key("composer-queue")
    .into()
}

/// 图片 MIME 猜测（对齐 Web 端 Blob type 语义；对话框无 mime 信息）。
pub(crate) fn guess_image_mime(file_name: &str) -> String {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if lower.ends_with(".gif") {
        "image/gif".to_string()
    } else if lower.ends_with(".webp") {
        "image/webp".to_string()
    } else if lower.ends_with(".bmp") {
        "image/bmp".to_string()
    } else {
        "image/*".to_string()
    }
}
