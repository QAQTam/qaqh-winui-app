use std::rc::Rc;

use markdown_winui::BlockView;
use qaqh_fluent::tokens;
use windows_reactor::*;

use crate::diff_drawer;
use crate::diff_drawer::{DrawerFile, DrawerRequest};

/// 工具卡（流式累积，id 稳定）。折叠器承载：
/// header = 状态 + 语义动作 + 目标摘要；展开 = 参数 raw（QAQ-Harness 工具）或
/// 执行状态（provider 内建工具，如 web_search 搜索中…）。
fn tool_action(card: &markdown_winui::ToolCardView) -> String {
    let Some(name) = card.name.as_deref() else {
        return "解析工具调用".to_string();
    };
    let short_name = name
        .rsplit(|ch| matches!(ch, '.' | ':' | '/'))
        .next()
        .unwrap_or(name);
    match short_name.to_ascii_lowercase().as_str() {
        "exec" | "exec_command" | "shell" | "shell_command" => "运行命令".to_string(),
        "read" | "read_file" => "读取文件".to_string(),
        "write" | "write_file" => "写入文件".to_string(),
        "apply_patch" | "edit" | "edit_file" => "修改文件".to_string(),
        "grep" | "rg" | "search_files" | "search_text" => "搜索内容".to_string(),
        "web_search" | "search_query" => "搜索网页".to_string(),
        "view_image" | "open_image" | "read_image" => "查看图片".to_string(),
        "list_files" | "list_directory" => "列出文件".to_string(),
        _ => format!("调用 {short_name}"),
    }
}

fn compact_preview(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut preview = compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    preview.push('…');
    preview
}

fn tool_argument_hint(card: &markdown_winui::ToolCardView) -> Option<String> {
    let raw = card.args_json.as_deref()?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    const HINT_KEYS: [&str; 8] = [
        "path", "file", "command", "query", "q", "pattern", "url", "name",
    ];
    if let serde_json::Value::Object(args) = &value {
        for key in HINT_KEYS {
            if let Some(value) = args.get(key).and_then(serde_json::Value::as_str) {
                let preview = compact_preview(value, 52);
                if !preview.is_empty() {
                    return Some(preview);
                }
            }
        }
    }
    value
        .as_str()
        .map(|value| compact_preview(value, 52))
        .filter(|value| !value.is_empty())
}

/// 工具行可见性（V4-E 已废除）：仅抑制 Prepared 预览卡（LLM 刚吐出调用、
/// 尚未开始执行/等待审批的占位）。运行中转圈、完成 ✓、失败 ✕ 一律保留
/// 状态行——读/写/编辑的可审计性优先于空间密度（用户决定 2026-08-24）；
/// 文件修改类仍额外叠加「已修改 N 个文件」diff 汇总卡（turns.rs 流末总结）。
pub(super) fn tool_row_visible(card: &markdown_winui::ToolCardView) -> bool {
    card.started || card.done
}

/// 工具行（V4-D 精简）：状态图标 + 动作短语 + 参数摘要 + 统计（±N · 耗时），
/// 直接显示在对话流内（不再 Expander 折叠）；失败时错误摘要内联在行下方。
pub(super) fn tool_row(
    turn_id: &str,
    block_order: u32,
    card: &markdown_winui::ToolCardView,
    duration_ms: Option<u64>,
    _color_scheme: ColorScheme,
) -> Element {
    let action = tool_action(card);
    let hint = tool_argument_hint(card);
    let mut line = action.clone();
    if let Some(h) = &hint {
        line.push_str(" · ");
        line.push_str(h);
    }
    // 统计：±N（changes.label）+ 耗时（block 墙钟）。
    let mut stats: Vec<String> = Vec::new();
    if let Some(changes) = &card.changes {
        if !changes.is_empty() {
            stats.push(changes.label());
        }
    }
    if let Some(ms) = duration_ms {
        stats.push(format_duration(ms));
    }
    if !stats.is_empty() {
        line.push_str("   ");
        line.push_str(&stats.join("  "));
    }
    // 状态图标（文案不随状态变化；状态由图标/颜色表达）。
    let icon: Element = if !card.done {
        ProgressRing::indeterminate()
            .width(12.0)
            .height(12.0)
            .into()
    } else if card.failed {
        text_block("✕")
            .font_size(12.0)
            .foreground(ThemeRef::SystemCritical)
            .into()
    } else {
        text_block("✓")
            .font_size(12.0)
            .foreground(ThemeRef::SystemSuccess)
            .into()
    };
    let line_el: Element = hstack((
        icon,
        text_block(&line)
            .font_size(tokens::TYPE_CAPTION)
            .foreground(if card.failed {
                ThemeRef::SystemCritical
            } else {
                ThemeRef::PrimaryText
            }),
    ))
    .spacing(tokens::SPACE_2)
    .into();
    // 失败：错误摘要内联在行下方（不再需要展开折叠）。
    let el: Element = if card.failed {
        if let Some(f) = &card.failure {
            vstack((
                line_el,
                text_block(format!("错误：{f}"))
                    .font_size(tokens::TYPE_CAPTION)
                    .foreground(ThemeRef::SystemCritical)
                    .wrap()
                    .selectable(),
            ))
            .spacing(tokens::SPACE_1)
            .into()
        } else {
            line_el
        }
    } else {
        line_el
    };
    el.with_key(format!("{turn_id}-r{block_order}-tool-{}", card.id))
        .automation_name(&line)
        .automation_id(format!("chat-tool-{}", card.id))
        .into()
}

/// 耗时格式化（block 墙钟；1s 内显示毫秒，之后一位小数秒）。
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// 收集工具块的 diff 数据（ToolBody::Diff → 按文件合并）→ 抽屉请求。
/// 同路径多工具块（如 read 后 write）合并 rows + 统计；failed 取 OR。
pub(super) fn collect_diff_drawer(
    turn_id: &str,
    blocks: &[Rc<BlockView>],
) -> Option<DrawerRequest> {
    let mut by_path: Vec<(String, bool, Vec<markdown_winui::DiffFile>)> = Vec::new();
    for block in blocks {
        let Some(card) = &block.tool else { continue };
        let markdown_winui::ToolBody::Diff(doc) = &card.body else {
            continue;
        };
        let failed = card.failed;
        for f in &doc.files {
            let path = f.display_path().to_string();
            // 同文件多次编辑 → 追加为独立段（保留各自行号体系），不再硬拼。
            if let Some((_, f_failed, segs)) = by_path.iter_mut().find(|(p, _, _)| *p == path) {
                segs.push(f.clone());
                *f_failed = *f_failed || failed;
            } else {
                by_path.push((path, failed, vec![f.clone()]));
            }
        }
    }
    if by_path.is_empty() {
        return None;
    }
    let files = by_path
        .into_iter()
        .map(|(path, failed, segments)| DrawerFile {
            added: segments.iter().map(|s| s.lines_added).sum(),
            removed: segments.iter().map(|s| s.lines_removed).sum(),
            path,
            failed,
            segments,
        })
        .collect();
    Some(DrawerRequest::Diff {
        turn_id: turn_id.to_string(),
        files,
    })
}

/// turn 末尾总结行：「已修改 N 个文件（+X −Y）· 查看详情 ›」→ 打开 diff 抽屉。
pub(super) fn diff_summary_row(turn_id: &str, req: DrawerRequest) -> Element {
    let DrawerRequest::Diff { files, .. } = &req else {
        return grid(()).into();
    };
    let n = files.len();
    let added: usize = files.iter().map(|f| f.added).sum();
    let removed: usize = files.iter().map(|f| f.removed).sum();
    let failed_n = files.iter().filter(|f| f.failed).count();
    let label = if failed_n > 0 {
        format!("已修改 {n} 个文件（+{added} −{removed}）· {failed_n} 个异常")
    } else {
        format!("已修改 {n} 个文件（+{added} −{removed}）")
    };
    border(
        hstack((
            text_block(&label)
                .font_size(12.0)
                .foreground(ThemeRef::SecondaryText),
            text_block("查看详情 ›")
                .font_size(12.0)
                .semibold()
                .foreground(ThemeRef::AccentText),
        ))
        .spacing(8.0)
        .padding(Thickness::xy(10.0, 6.0)),
    )
    .background(ThemeRef::LayerFill)
    .border_brush(ThemeRef::CardStroke)
    .border_thickness(Thickness::uniform(1.0))
    .corner_radius(6.0)
    .on_tapped(move || diff_drawer::open_diff_drawer(req.clone()))
    .automation_name(&label)
    .with_key(format!("{turn_id}-diff-summary"))
    .into()
}

// （ToolSection 聚合区段已移除：V4-E 工具行交错渲染在消息流中）

#[cfg(test)]
mod activity_group_tests {
    use super::super::blocks::answer_has_visible_content;
    use super::*;
    use markdown_winui::{AnswerView, LiveSegment};

    fn card(name: &str, args_json: Option<&str>, done: bool) -> markdown_winui::ToolCardView {
        markdown_winui::ToolCardView {
            id: format!("{name}-1"),
            name: Some(name.to_string()),
            args_display: String::new(),
            args_json: args_json.map(str::to_string),
            body: markdown_winui::ToolBody::Empty,
            changes: None,
            done,
            failed: false,
            failure: None,
            provider: false,
            started: true,
        }
    }

    #[test]
    fn tool_header_uses_semantic_action_and_argument_hint() {
        let tool = card(
            "functions.exec",
            Some(r#"{"command":"cargo test -p qaqh-winui"}"#),
            true,
        );
        assert_eq!(tool_action(&tool), "运行命令");
        assert_eq!(
            tool_argument_hint(&tool).as_deref(),
            Some("cargo test -p qaqh-winui")
        );
    }

    #[test]
    fn completed_tool_rows_stay_visible_after_v4e_repeal() {
        // F-N6：完成态只读/搜索/文件修改工具不再被回收，全部保留 ✓ 行；
        // 仅 Prepared（started=false 且 done=false）预览继续抑制。
        for name in [
            "read",
            "grep",
            "list_files",
            "web_search",
            "write",
            "edit",
            "apply_patch",
        ] {
            let done_card = card(name, None, true);
            assert!(tool_row_visible(&done_card), "{name} 完成态应可见");
        }
        let running = card("read", None, false);
        assert!(running.started, "card() 助手默认 started=true");
        assert!(tool_row_visible(&running));
        let prepared = markdown_winui::ToolCardView {
            started: false,
            done: false,
            ..card("write", None, false)
        };
        assert!(!tool_row_visible(&prepared), "Prepared 预览仍应抑制");
    }

    #[test]
    fn empty_answer_does_not_create_an_assistant_shell() {
        let empty = AnswerView::default();
        assert!(!answer_has_visible_content(&empty));

        let visible = AnswerView::Streaming {
            raw: "答案".into(),
            inlines: Vec::new(),
            segments: vec![LiveSegment::Text("答案".into())],
            table_tracker: Default::default(),
            gfm_table_tracker: Default::default(),
        };
        assert!(answer_has_visible_content(&visible));
    }
}

/// 诊断日志（窗口程序无控制台：写 %TEMP%）。
pub(super) fn log_diag(msg: &str) {
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
