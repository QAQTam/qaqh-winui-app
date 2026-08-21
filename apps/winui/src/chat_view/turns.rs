use std::rc::Rc;

use markdown_winui::BlockTurnView;
use qaqh_fluent::{StatusTone, tokens};
use windows_reactor::*;

use super::blocks::{BlockProps, block_memo};
use super::tools::{collect_diff_drawer, diff_summary_row};
use crate::bridge::Bridge;

// ── turn / block 渲染（timeline 单源，Phase 2）────────────────────

/// turn 级 memo 的 props。等价 = 同一份 BlockTurnView 数据（Rc 指针命中，或
/// turn_id + mutation_rev 相同）+ 主题。memo 命中时 reconciler
/// 复用上次渲染输出（不执行组件函数），行 diff 也直接走 props 比较。
#[derive(Clone)]
pub(super) struct TurnProps {
    pub(super) turn: Rc<BlockTurnView>,
    pub(super) color_scheme: ColorScheme,
}

impl PartialEq for TurnProps {
    fn eq(&self, other: &Self) -> bool {
        self.color_scheme == other.color_scheme
            && (Rc::ptr_eq(&self.turn, &other.turn)
                || (self.turn.turn_id == other.turn.turn_id
                    && self.turn.mutation_rev == other.turn.mutation_rev))
    }
}

/// 外层 memo 先截断未变化 turn；只有 props 变化时才重建 turn 内容。
/// 注：reactor #4828 已移除 error_boundary（#4829 起 callback panic fatal）——
/// turn 渲染 panic 不再降级为 fallback，而是传播到 render 边界后 abort。
pub(super) fn turn_memo(props: &TurnProps, _cx: &mut RenderCx) -> Element {
    component(turn_body, props.clone()).with_key(format!("{}-turn", props.turn.turn_id))
}

fn turn_body(props: &TurnProps, cx: &mut RenderCx) -> Element {
    // 右键撤回菜单（局部状态：memo 命中时保留，随历史释放）。
    let (menu_open, set_menu_open) = cx.use_state::<bool>(false);
    let (confirm_open, set_confirm_open) = cx.use_state::<bool>(false);
    let turn_id = props.turn.turn_id.clone();
    let on_undo = {
        let set_menu_open = set_menu_open.clone();
        let set_confirm_open = set_confirm_open.clone();
        move || {
            set_menu_open.call(false);
            set_confirm_open.call(true);
        }
    };
    // 确认对话框（modal，不在 visual tree）：确认后按 turn_id 撤回
    // （删除该 turn 及之后全部内容）；关闭总是复位 is_open。
    let confirm_dialog: Element = ContentDialog::new("撤回此消息？")
        .content("将删除该消息及其后的所有内容，此操作不可恢复。")
        .primary_button_text("撤回")
        .close_button_text("取消")
        .is_open(confirm_open)
        .on_closed({
            let turn_id = turn_id.clone();
            let set_confirm_open = set_confirm_open.clone();
            move |result| {
                set_confirm_open.call(false);
                if result == ContentDialogResult::Primary {
                    Bridge::shared().spawn_undo_turn(turn_id.clone());
                }
            }
        })
        .into();
    grid((
        turn_view(
            props.turn.as_ref(),
            props.color_scheme.clone(),
            menu_open,
            on_undo,
            set_menu_open.clone(),
        ),
        confirm_dialog,
    ))
    .rows([GridLength::STAR])
    .columns([GridLength::STAR])
    .into()
}

/// 撤回菜单浮层（右键弹出；图标 + 文字，仿 MenuFlyout 视觉）。
fn undo_menu_float(on_undo: impl Fn() + 'static) -> Element {
    border(
        button("撤回此消息（及之后内容）")
            .icon(Icon::symbol(Symbol::Undo))
            .subtle()
            .on_click(on_undo),
    )
    .background(ThemeRef::LayerFill)
    .corner_radius(8.0)
    .padding(Thickness::uniform(tokens::SPACE_1))
    .horizontal_alignment(HorizontalAlignment::Right)
    .with_key("turn-undo-menu")
    .into()
}

/// turn 级视图：用户气泡 + 状态徽标 + 失败提示 + blocks 按**到达序**渲染
/// （跨 round 交错保序——修复“思考-工具-回复”被三段式压平的缺陷 D1）。
fn turn_view(
    turn: &BlockTurnView,
    color_scheme: ColorScheme,
    menu_open: bool,
    on_undo: impl Fn() + 'static,
    set_menu_open: SetState<bool>,
) -> Element {
    let (status, tone) = if turn.failed {
        ("失败", StatusTone::Critical)
    } else if turn.sealed {
        ("已完成", StatusTone::Success)
    } else {
        ("正在处理", StatusTone::Running)
    };
    let mut items: Vec<Element> = vec![if turn.user_text.trim_start().starts_with("[SUBAGENT ") {
        // 子代理注入回合：不渲染用户气泡（注入正文/标签不得以 user 身份
        // 泄露到聊天流——后端已只发标签行，这里对历史数据/旧 timeline
        // 全文兜底过滤）。注入内容的模型回复仍按 blocks 正常渲染。
        text_block(&turn.user_text)
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .padding(Thickness::uniform(4.0))
            .into()
    } else {
        qaqh_fluent::user_message(
            text_block(&turn.user_text)
                .font_size(tokens::TYPE_BODY)
                .line_height(tokens::TYPE_BODY_LINE_HEIGHT)
                .wrap()
                .selectable(),
            qaqh_fluent::status_badge(status, tone),
        )
    }];
    // 右键菜单浮层：插在消息上方（右对齐），点消息任意处关闭。
    if menu_open {
        items.insert(0, undo_menu_float(on_undo));
    }
    if turn.failed {
        if let Some(error) = &turn.failure {
            items.push(
                text_block(format!("回合失败：{error}"))
                    .font_size(tokens::TYPE_CAPTION)
                    .foreground(ThemeRef::SystemCritical)
                    .wrap()
                    .selectable()
                    .automation_name("回合失败详情")
                    .with_key(format!("{}-failed-error", turn.turn_id))
                    .into(),
            );
        }
    }
    // blocks 按到达序渲染（每块独立 memo：block_id + mutation_rev）。
    // Tool 块**交错渲染在消息流中**（思考 → 工具窄行 → 答案，V4-E：
    // 尊重模型输出顺序）——不再收集到 turn 底部。完成态工具行自动
    // 压缩（只读/修改类完成即回收空间），最终答案位于流末尾（底部），
    // 用户视线跟随最新内容无需滚动（对齐 Claude Code 交互模式）。
    for block in &turn.blocks {
        items.push(
            memo(
                block_memo,
                BlockProps {
                    turn_id: Rc::from(turn.turn_id.as_str()),
                    block: block.clone(),
                    color_scheme: color_scheme.clone(),
                },
            )
            .with_key(format!("{}-block-{}", turn.turn_id, block.block_id)),
        );
    }
    // 含 diff 的工具块 → 流末尾总结行「已修改 N 个文件 · 查看详情」（V4）。
    let drawer_req = collect_diff_drawer(&turn.turn_id, &turn.blocks);
    if let Some(req) = drawer_req {
        items.push(diff_summary_row(&turn.turn_id, req));
    }
    vstack(items)
        .spacing(tokens::SPACE_3)
        .padding(Thickness {
            left: tokens::SPACE_6,
            top: tokens::SPACE_3,
            right: tokens::SPACE_6,
            bottom: tokens::SPACE_3,
        })
        .max_width(tokens::CONVERSATION_MAX_WIDTH)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .on_right_tapped({
            let set_menu_open = set_menu_open.clone();
            move || set_menu_open.call(true)
        })
        .on_tapped({
            let set_menu_open = set_menu_open.clone();
            move || set_menu_open.call(false)
        })
        .with_key(turn.turn_id.clone())
        .into()
}
