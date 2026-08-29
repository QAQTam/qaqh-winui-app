use std::rc::Rc;

use markdown_winui::{
    AnswerView, BlockView, LiveSegment, RichTextOutput, TableHover, TimelineBlockKind, ToolCardView,
};
use qaqh_fluent::{motion, tokens};
use windows_reactor::*;

use super::tools::{tool_row, tool_row_visible};
use super::zoom::zoom_request_callback;

/// block 级 memo：block_id + mutation_rev 等价判据（live delta 只重建
/// 变化的块，不重建整个 turn）。
#[derive(Clone)]
pub(super) struct BlockProps {
    pub(super) turn_id: Rc<str>,
    pub(super) block: Rc<BlockView>,
    pub(super) color_scheme: ColorScheme,
}

impl PartialEq for BlockProps {
    fn eq(&self, other: &Self) -> bool {
        self.color_scheme == other.color_scheme
            && self.turn_id == other.turn_id
            && (Rc::ptr_eq(&self.block, &other.block)
                || (self.block.block_id == other.block.block_id
                    && self.block.mutation_rev == other.block.mutation_rev))
    }
}

pub(super) fn block_memo(props: &BlockProps, _cx: &mut RenderCx) -> Element {
    component(block_view, props.clone())
        .with_key(format!("{}-block-{}", props.turn_id, props.block.block_id))
}

/// 块渲染分派（按 kind；顺序由 turn.blocks 到达序决定）。
fn block_view(props: &BlockProps, cx: &mut RenderCx) -> Element {
    let block = props.block.as_ref();
    match block.kind {
        TimelineBlockKind::Reasoning => component(reasoning_view, props.clone()).with_key(format!(
            "{}-block-reasoning-{}",
            props.turn_id, block.block_id
        )),
        TimelineBlockKind::Tool => {
            let card = block.tool.clone().unwrap_or_else(|| ToolCardView {
                id: block.block_id.clone(),
                name: None,
                args_display: String::new(),
                args_json: None,
                body: markdown_winui::ToolBody::Empty,
                changes: None,
                done: false,
                failed: false,
                failure: None,
                provider: false,
                started: true,
            });
            // 工具行可见性：Prepared 预览（LLM 刚吐出、未真正执行）不渲染，
            // 其余状态一律保留——V4-E「完成即回收」策略废除（F-N6，用户
            // 决定 2026-08-24）：read/grep/write/edit 完成态也显示 ✓ 行；
            // 文件修改类的「已修改 N 个文件」diff 汇总卡照旧叠加。
            if !tool_row_visible(&card) {
                return Element::Empty;
            }
            tool_row(
                props.turn_id.as_ref(),
                block.block_order,
                &card,
                block.duration_ms,
                props.color_scheme.clone(),
            )
        }
        TimelineBlockKind::Text => {
            if answer_has_visible_content(&block.answer) {
                // 表格行悬停状态：按块隔离，组件销毁即随历史释放。
                let table_hover = TableHover::new(cx);
                qaqh_fluent::assistant_message(answer_view(
                    props.turn_id.as_ref(),
                    block.block_order,
                    &block.answer,
                    &table_hover,
                    props.color_scheme.clone(),
                ))
                .with_key(format!("{}-block-answer-{}", props.turn_id, block.block_id))
            } else {
                Element::Empty
            }
        }
        TimelineBlockKind::Notice => notice_block(props.turn_id.as_ref(), block),
    }
}

/// reasoning 块（V4+）：可折叠"过程摘要"。
///
/// - 头部：状态点（思考中=spinner / 完成=✓ 绿）+ 标题 + meta + chevron；
/// - 内容：ScrollViewer 包完整文本（max 176px），流式思考中自动追底
///   （text 增长 → generation 递增 → `scroll_to_bottom` diff）；
///   思考完成（sealed）后停止追底，用户可自由滚动回溯完整过程；
/// - 交互：思考中默认展开，sealed 且用户未手动操作 → 自动折叠为一行。
fn reasoning_view(props: &BlockProps, cx: &mut RenderCx) -> Element {
    let block = props.block.as_ref();
    if block.text.trim().is_empty() {
        return Element::Empty;
    }
    let (open, set_open) = cx.use_state::<bool>(true);
    let user_touched = cx.use_ref::<bool>(false);
    let done = block.sealed;
    // 自动折叠：sealed 且用户从未手动开合过。
    cx.use_effect((done,), {
        let set_open = set_open.clone();
        let user_touched = user_touched.clone();
        move || {
            if done && !*user_touched.borrow() {
                set_open.call(false);
            }
        }
    });
    // 流式追底：文本增长 → generation 递增 → `scroll_to_bottom` diff 触发
    // backend ChangeView。sealed 后 text 不再变化，effect 不再触发，
    // 最后一次滚动请求在 sealed 帧执行（layout 通常已跟上）。
    const REASONING_TAIL_HEIGHT: f64 = 176.0;
    let (scroll_gen, set_scroll_gen) = cx.use_state::<i32>(0);
    cx.use_effect((block.text.clone(),), {
        let set_scroll_gen = set_scroll_gen.clone();
        // 闭包捕获本次渲染的 scroll_gen 值；text 变化 → effect 执行 → 递增。
        move || {
            set_scroll_gen.call(scroll_gen + 1);
        }
    });
    let mut run = RichTextRun::plain(&block.text);
    run.is_italic = true;
    let mut summary = RichTextBlock::new();
    summary.paragraphs = vec![RichTextParagraph::new(vec![RichTextInline::Run(run)])];
    summary.font_size = Some(tokens::TYPE_BODY);
    summary.line_height = Some(tokens::TYPE_BODY_LINE_HEIGHT);
    summary.text_wrapping = TextWrapping::Wrap;
    summary.is_text_selection_enabled = true;
    summary.modifiers.font_family = Some(tokens::DEFAULT_UI_FONT_FAMILY.to_string());
    let _ = props.color_scheme.clone();
    // 头部：状态点 + 标题 + meta + chevron，整行可点开合。
    // 不用原生 Expander：F-N15 §1.2 定案 Expander::OnApplyTemplate → VSM
    // storyboard → Binding 重连 → GetActivationFactory 80040111 冷路径
    // 崩溃（resume 大会话必现），全 app 禁用 Expander，折叠交互统一改为
    // tap header + 条件渲染。
    let dot: Element = if done {
        text_block("✓")
            .font_size(10.0)
            .foreground(ThemeRef::SystemSuccess)
            .into()
    } else {
        ProgressRing::indeterminate()
            .width(10.0)
            .height(10.0)
            .into()
    };
    let head = hstack((
        dot,
        text_block(if done {
            "过程摘要"
        } else {
            "正在整理过程摘要"
        })
        .font_size(tokens::TYPE_CAPTION)
        .semibold(),
        text_block(if done { "已折叠" } else { "思考中…" })
            .font_size(tokens::TYPE_CAPTION)
            .foreground(ThemeRef::SecondaryText),
        text_block(if open { "▾" } else { "▸" })
            .font_size(tokens::TYPE_CAPTION)
            .foreground(ThemeRef::SecondaryText),
    ))
    .spacing(tokens::SPACE_2)
    .padding(Thickness::xy(tokens::SPACE_2, 6.0))
    .on_tapped({
        let user_touched = user_touched.clone();
        let set_open = set_open.clone();
        move || {
            *user_touched.borrow_mut() = true;
            set_open.call(!open);
        }
    });
    let body: Element = if open {
        ScrollViewer::new(
            vstack((Element::from(summary),))
                .padding(Thickness::xy(tokens::SPACE_3, tokens::SPACE_2)),
        )
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .max_height(REASONING_TAIL_HEIGHT)
        .scroll_to_bottom(scroll_gen)
        .into()
    } else {
        Element::Empty
    };
    vstack((head, body))
        .spacing(1.0)
        .min_height(34.0)
        .automation_name(if done {
            "过程摘要"
        } else {
            "正在整理过程摘要"
        })
        .automation_id(format!("chat-reasoning-{}", block.block_id))
        .with_key(format!(
            "{}-block-reasoning-{}",
            props.turn_id, block.block_id
        ))
        .into()
}

/// notice 块：系统/服务端通知文本（遥测平面，不参与正文排序语义）。
fn notice_block(turn_id: &str, block: &BlockView) -> Element {
    if block.text.trim().is_empty() {
        return Element::Empty;
    }
    text_block(&block.text)
        .font_size(tokens::TYPE_CAPTION)
        .foreground(ThemeRef::SecondaryText)
        .wrap()
        .selectable()
        .automation_name("系统通知")
        .with_key(format!("{turn_id}-block-notice-{}", block.block_id))
        .into()
}

pub(super) fn answer_has_visible_content(answer: &AnswerView) -> bool {
    match answer {
        AnswerView::Streaming { raw, .. } => !raw.trim().is_empty(),
        AnswerView::Final { blocks, rich } => {
            !blocks.is_empty()
                || !rich.paragraphs.is_empty()
                || !rich.code_blocks.is_empty()
                || !rich.diagrams.is_empty()
                || !rich.tables.is_empty()
        }
    }
}

fn answer_view(
    turn_id: &str,
    round_num: u32,
    answer: &AnswerView,
    table_hover: &TableHover,
    color_scheme: ColorScheme,
) -> Element {
    match answer {
        AnswerView::Streaming { segments, .. } => {
            live_view(turn_id, round_num, segments, table_hover)
        }
        AnswerView::Final { rich, .. } => {
            final_view(turn_id, round_num, rich, table_hover, color_scheme)
        }
    }
}

/// 流式答案视图：字面/表格交错序列按序渲染
/// （协议表格渐进长出；残行逐字生长在网格末行）。
fn live_view(
    turn_id: &str,
    round_num: u32,
    segments: &[LiveSegment],
    table_hover: &TableHover,
) -> Element {
    let mut items: Vec<Element> = Vec::new();
    for (si, seg) in segments.iter().enumerate() {
        match seg {
            LiveSegment::Text(t) if !t.is_empty() => items.push(
                text_block(t)
                    .font_size(tokens::TYPE_BODY)
                    .line_height(tokens::TYPE_BODY_LINE_HEIGHT)
                    .wrap()
                    .selectable()
                    .with_key(format!("{turn_id}-r{round_num}-live-t{si}"))
                    .into(),
            ),
            LiveSegment::Table(td) => items.push(markdown_winui::table_view(
                td,
                &format!("{turn_id}-r{round_num}-live-table-{si}"),
                Some(table_hover),
            )),
            LiveSegment::Text(_) => {}
        }
    }
    if items.is_empty() {
        // 空内容：占位保持 key 稳定
        items.push(
            text_block("")
                .with_key(format!("{turn_id}-r{round_num}-live-empty"))
                .into(),
        );
    }
    vstack(items)
        .spacing(tokens::SPACE_2)
        .with_key(format!("{turn_id}-r{round_num}-live"))
        .into()
}

/// 权威终态视图：按文档块顺序渲染（正文/表格/代码块交错），
/// 连续段落合并进同一 RichTextBlock，遇表格/代码块/图表断开。
fn final_view(
    turn_id: &str,
    round_num: u32,
    rich: &RichTextOutput,
    table_hover: &TableHover,
    color_scheme: ColorScheme,
) -> Element {
    let mut items: Vec<Element> = Vec::new();
    // 段落级渲染：每段独立 RichTextBlock，间距统一走 vstack（web 式等距，
    // 对齐 marked→HTML→CSS 的均匀 margin 模型）。此前多段合入单个
    // RichTextBlock 时 XAML Paragraph 无 Margin 通道，段间距恒为 0。
    let make_block = |paragraphs: Vec<RichTextParagraph>| {
        let mut rt = RichTextBlock::new();
        rt.paragraphs = paragraphs;
        rt.font_size = Some(tokens::TYPE_BODY);
        rt.line_height = Some(tokens::TYPE_BODY_LINE_HEIGHT);
        rt.text_wrapping = TextWrapping::Wrap;
        rt.is_text_selection_enabled = true;
        // RichTextBlock 不参与 XAML 字体属性继承（host 全局字体只覆盖
        // TextBlock 系），必须显式指定 UI 字体，否则 fallback 系统字体。
        rt.modifiers.font_family = Some(tokens::DEFAULT_UI_FONT_FAMILY.to_string());
        rt
    };
    if !rich.blocks.is_empty() {
        for b in &rich.blocks {
            match b {
                markdown_winui::FinalBlock::Paragraph(p) => {
                    items.push(make_block(vec![p.clone()]).into());
                }
                markdown_winui::FinalBlock::Heading { level, paragraph } => {
                    // 论文式层级：段前 > 段后。vstack 基础间距 SPACE_2(8) 充当
                    // 段后距，标题顶边距叠加出段前层次：h1 8+8=16 / h2 6+8=14 /
                    // h3+ 4+8=12（正文段间 8）。
                    let top = match level {
                        1 => tokens::SPACE_2,
                        2 => 6.0,
                        _ => 4.0,
                    };
                    let el: Element = make_block(vec![paragraph.clone()]).into();
                    items.push(el.margin(Thickness {
                        left: 0.0,
                        top,
                        right: 0.0,
                        bottom: 0.0,
                    }));
                }
                markdown_winui::FinalBlock::Table(td) => {
                    items.push(markdown_winui::table_view(
                        td,
                        &format!("{turn_id}-r{round_num}-table-{n}", n = items.len()),
                        Some(table_hover),
                    ));
                }
                markdown_winui::FinalBlock::Code(code) => {
                    let highlighted = markdown_winui::highlighted_code_block(
                        code,
                        color_scheme,
                        tokens::CODE_FONT_FAMILY,
                    );
                    items.push(qaqh_fluent::code_surface_content(
                        code.lang.as_deref().unwrap_or(""),
                        highlighted,
                        format!("{turn_id}-r{round_num}-code-{n}", n = items.len()),
                    ));
                }
                markdown_winui::FinalBlock::Diagram(diagram) => {
                    let key = format!("{turn_id}-r{round_num}-diagram-{n}", n = items.len());
                    items.push(markdown_winui::diagram_view(
                        diagram,
                        color_scheme,
                        &key,
                        Some(zoom_request_callback(diagram, color_scheme, &key)),
                    ));
                }
            }
        }
    } else {
        // 降级路径（blocks 为空的历史数据）：按通道渲染，保底不空白。
        let mut rt = RichTextBlock::new();
        rt.paragraphs = rich.paragraphs.clone();
        rt.font_size = Some(tokens::TYPE_BODY);
        rt.line_height = Some(tokens::TYPE_BODY_LINE_HEIGHT);
        rt.text_wrapping = TextWrapping::Wrap;
        rt.is_text_selection_enabled = true;
        // RichTextBlock 不参与 XAML 字体属性继承（host 全局字体只覆盖
        // TextBlock 系），必须显式指定 UI 字体，否则 fallback 系统字体。
        rt.modifiers.font_family = Some(tokens::DEFAULT_UI_FONT_FAMILY.to_string());
        items.push(rt.into());
        for (ti, table) in rich.tables.iter().enumerate() {
            items.push(markdown_winui::table_view(
                table,
                &format!("{turn_id}-r{round_num}-table-{ti}"),
                Some(table_hover),
            ));
        }
        for (ci, code) in rich.code_blocks.iter().enumerate() {
            let highlighted = markdown_winui::highlighted_code_block(
                code,
                color_scheme,
                tokens::CODE_FONT_FAMILY,
            );
            items.push(qaqh_fluent::code_surface_content(
                code.lang.as_deref().unwrap_or(""),
                highlighted,
                format!("{turn_id}-r{round_num}-code-{ci}"),
            ));
        }
        for (di, diagram) in rich.diagrams.iter().enumerate() {
            let key = format!("{turn_id}-r{round_num}-diagram-{di}");
            items.push(markdown_winui::diagram_view(
                diagram,
                color_scheme,
                &key,
                Some(zoom_request_callback(diagram, color_scheme, &key)),
            ));
        }
    }
    vstack(items)
        .spacing(tokens::SPACE_2)
        .transition(motion::content_enter(), None)
        .with_key(format!("{turn_id}-r{round_num}-final"))
        .into()
}
