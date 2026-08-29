use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows_reactor::*;

use qaqh_fluent::{motion, tokens};
use qaqh_types::tool_mode::CUSTOM;

use crate::bridge::{Bridge, ComposerAttachment, ComposerState, ComposerTextFile, WorkPhase};
use crate::shell_store::ContextStats;

use super::status::{guess_image_mime, queue_row, work_status_bar};
use super::*;

pub fn composer_bar(cx: &mut RenderCx, bridge: Arc<Bridge>) -> Element {
    let (state, set_state) = cx.use_state::<ComposerState>(ComposerState::default());
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_rev = cx.use_ref::<u64>(0);
    // 草稿：ref 真实存储 + 版本号驱动渲染（SetState 无 get，回调从 ref 读写）。
    let draft = cx.use_ref::<Draft>(Draft::default());
    let (_draft_version, set_draft_version) = cx.use_state::<u64>(0);
    // 版本号真实存储（回调读 ref 递增，避免批处理下基于渲染值 +1 丢帧）。
    let draft_ver = cx.use_ref::<u64>(0);
    // sendAck/seed 基线（effect 比对用）。
    let last_ack = cx.use_ref::<u64>(0);
    let last_seed = cx.use_ref::<String>(String::new());
    // Desktop editor sizing: content estimates grow the editor until the user
    // takes ownership with the native pointer-captured resize handle.
    let (input_height, set_input_height) = cx.use_state::<f64>(INPUT_DEFAULT_HEIGHT);
    let (manual_height, set_manual_height) = cx.use_state::<bool>(false);
    let (immersive, set_immersive) = cx.use_state::<bool>(false);
    let resize_start = cx.use_ref::<Option<(f64, f64)>>(None);
    // 上下文构成分布（输入框下常驻堆叠条）：daemon 回合结束写入
    // context_stats.json（6 段 token 分布，同 Web ContextPanel 饼图）。
    let (ctx_stats, set_ctx_stats) = cx.use_state::<Option<ContextStats>>(None);
    let ctx_timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_ctx_rev = cx.use_ref::<u64>(0);

    // composer 投影轮询（250ms rev 比对）。
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_state = set_state.clone();
        let timer = timer.clone();
        let last_rev = last_rev.clone();
        let draft_mirror = draft.clone();
        move || {
            let fetch_bridge = bridge.clone();
            let apply_bridge = bridge.clone();
            crate::shell::poll_rev(
                "composer",
                timer,
                last_rev,
                POLL_INTERVAL,
                move || fetch_bridge.core().composer_snapshot(),
                move |s| {
                    // F-N4 写穿镜像：心跳把当前草稿同步到 bridge 持久层，
                    // 页面卸载（use_ref 销毁）最多丢最后 250ms 输入而非全部。
                    if !s.seed.is_empty() {
                        let snap = draft_mirror.borrow().clone();
                        apply_bridge.core().save_draft(&s.seed, snap);
                    }
                    set_state.call(s);
                },
            );
        }
    });

    // 上下文构成轮询（context_stats 文件 mtime rev；文件缺失 → None）。
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_ctx_stats = set_ctx_stats.clone();
        let ctx_timer = ctx_timer.clone();
        let last_ctx_rev = last_ctx_rev.clone();
        move || {
            crate::shell::poll_rev(
                "composer_ctx",
                ctx_timer,
                last_ctx_rev,
                POLL_INTERVAL,
                move || bridge.core().context_stats_snapshot(),
                move |s| set_ctx_stats.call(s),
            );
        }
    });

    // sendAck 变化 → 发送已确认 → 清空草稿（悲观清空，对齐 Web 成功路径）。
    // seed 变化 → 会话切换 → 重置草稿（对齐 Web 新会话空输入）。
    // deps 与闭包都捕获 clone 值，避免 move 闭包拿走 state 所有权。
    let ack0 = state.send_ack;
    let seed0 = state.seed.clone();
    cx.use_effect((ack0, seed0.clone()), {
        let bridge = bridge.clone();
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let last_ack = last_ack.clone();
        let last_seed = last_seed.clone();
        let set_draft_version = set_draft_version.clone();
        move || {
            let ack = ack0;
            let seed = seed0.clone();
            if ack != *last_ack.borrow() {
                *last_ack.borrow_mut() = ack;
                if ack > 0 {
                    {
                        let mut d = draft.borrow_mut();
                        for att in &d.attachments {
                            remove_preview(att.preview_path.as_deref());
                        }
                        d.text.clear();
                        d.attachments.clear();
                    }
                    // F-N4：持久层同步清空（防卸载后从存档复活旧文本）。
                    let cleared = draft.borrow().clone();
                    bridge.core().save_draft(&seed, cleared);
                    log_diag("sendAck: draft cleared");
                }
            }
            if seed != *last_seed.borrow() {
                // F-N4 存旧取新：切走前保存旧会话草稿；切入时恢复目标草稿
                // （无存档则空白，对齐 Web 新会话语义）。预览临时文件随条目
                // 存活，仅在发送清空/容量逐出时删除。
                let prev = last_seed.borrow().clone();
                if !prev.is_empty() && prev != seed {
                    let snap = draft.borrow().clone();
                    bridge.core().save_draft(&prev, snap);
                }
                let restored = bridge.core().take_draft(&seed);
                {
                    let mut d = draft.borrow_mut();
                    match restored {
                        Some(r) => *d = r,
                        None => {
                            for att in &d.attachments {
                                remove_preview(att.preview_path.as_deref());
                            }
                            d.text.clear();
                            d.attachments.clear();
                            d.selected_slash = 0;
                            d.dismissed_slash = None;
                        }
                    }
                }
                *last_seed.borrow_mut() = seed;
                log_diag("seed changed: draft swapped");
            }
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    });

    // ── 回调（Arc 共享 + ref 读写；渲染时捕获 state 快照）───────────
    let has_pending_gate = state.has_pending_gate;
    let is_streaming = state.is_streaming;

    // 文本输入：更新草稿 + 重置 slash 导航（对齐 Web updateText）。
    // 自动高度用简单换行计数（避免逐字符 O(n²) 扫描），只在行数变化时触发。
    let last_auto_height = cx.use_ref::<f64>(INPUT_DEFAULT_HEIGHT);
    let on_text_changed = {
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let set_input_height = set_input_height.clone();
        let last_auto = last_auto_height.clone();
        move |value: String| {
            if !manual_height && !immersive {
                let line_count = value.as_bytes().iter().filter(|&&b| b == b'\n').count() + 1;
                // A1：单行 = 56（新空态基准），两行起每行 +20。
                let target = (36.0 + line_count as f64 * 20.0)
                    .clamp(INPUT_MIN_HEIGHT, INPUT_AUTO_MAX_HEIGHT);
                let prev = *last_auto.borrow();
                if (target - prev).abs() > 1.0 {
                    *last_auto.borrow_mut() = target;
                    set_input_height.call(target);
                }
            }
            let mut d = draft.borrow_mut();
            d.text = value;
            d.selected_slash = 0;
            d.dismissed_slash = None;
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    };

    // 提交：校验 → emit Send（附件传路径，base64 由 Web 侧读）。
    let on_submit: Arc<dyn Fn() + 'static> = Arc::new({
        let bridge = bridge.clone();
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let has_pending_gate = has_pending_gate;
        move || {
            let d = draft.borrow();
            let text = d.text.trim().to_string();
            if (text.is_empty() && d.attachments.is_empty()) || has_pending_gate {
                return;
            }
            let mut image_paths: Vec<ComposerAttachment> = Vec::new();
            let mut text_files: Vec<ComposerTextFile> = Vec::new();
            for att in &d.attachments {
                match &att.kind {
                    AttachmentKind::Image { mime_type } => image_paths.push(ComposerAttachment {
                        file_name: att.file_name.clone(),
                        mime_type: mime_type.clone(),
                        path: att.path.clone(),
                    }),
                    AttachmentKind::Text => text_files.push(ComposerTextFile {
                        file_name: att.file_name.clone(),
                        path: att.path.clone(),
                    }),
                }
            }
            drop(d);
            log_diag("composer send emitted");
            // 直连动作：协议请求 Rust 直发（附件上传 ContentRef 后发命令）。
            bridge.spawn_send_message(text, image_paths, text_files);
            // 悲观清空（对齐 Web sendAck 语义）：提交即清空草稿/附件。
            {
                let mut d = draft.borrow_mut();
                d.text = String::new();
                d.attachments.clear();
                d.selected_slash = 0;
                d.dismissed_slash = None;
                drop(d);
                let v = *draft_ver.borrow() + 1;
                *draft_ver.borrow_mut() = v;
                set_draft_version.call(v);
            }
        }
    });

    // 附件选择（STA 直调对话框 + 读元数据；用户取消返回 null 不动作）。
    let on_pick_image = {
        let bridge = bridge.clone();
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        move || match bridge.pick_image_file() {
            Ok(serde_json::Value::String(path)) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let file_name = path
                    .split(['/', '\\'])
                    .last()
                    .unwrap_or("image")
                    .to_string();
                let mime_type = guess_image_mime(&file_name);
                let id = format!("att-{}-{}", ATT_ID.fetch_add(1, Ordering::Relaxed), size);
                // 缩略图预览：复制到 %TEMP%（WinUI Image 不支持 base64）。
                let preview_path = write_preview_copy(&path, &id);
                let mut d = draft.borrow_mut();
                d.attachments.push(AttachmentItem {
                    id,
                    kind: AttachmentKind::Image { mime_type },
                    file_name,
                    size,
                    path,
                    preview_path,
                });
                drop(d);
                let v = *draft_ver.borrow() + 1;
                *draft_ver.borrow_mut() = v;
                set_draft_version.call(v);
            }
            _ => {}
        }
    };
    let on_pick_text = {
        let bridge = bridge.clone();
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        move || match bridge.pick_text_file() {
            Ok(serde_json::Value::String(path)) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let file_name = path.split(['/', '\\']).last().unwrap_or("file").to_string();
                let mut d = draft.borrow_mut();
                d.attachments.push(AttachmentItem {
                    id: format!("att-{}-{}", ATT_ID.fetch_add(1, Ordering::Relaxed), size),
                    kind: AttachmentKind::Text,
                    file_name,
                    size,
                    path,
                    preview_path: None,
                });
                drop(d);
                let v = *draft_ver.borrow() + 1;
                *draft_ver.borrow_mut() = v;
                set_draft_version.call(v);
            }
            _ => {}
        }
    };
    // 移除附件（按 id；顺带删预览临时文件）。
    let on_remove_attach: Arc<dyn Fn(String) + 'static> = Arc::new({
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        move |id: String| {
            let mut d = draft.borrow_mut();
            if let Some(att) = d.attachments.iter().find(|a| a.id == id) {
                remove_preview(att.preview_path.as_deref());
            }
            d.attachments.retain(|a| a.id != id);
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    });
    // footer 动作（直连动作：协议请求 Rust 直发，不再回传 Web）。
    let on_mode_toggle = {
        let bridge = bridge.clone();
        let mode = state.mode.clone();
        move || {
            let next = if mode == "plan" { "code" } else { "plan" };
            bridge.spawn_set_mode(next);
        }
    };
    let on_permission: Arc<dyn Fn(u64) + 'static> = Arc::new({
        let bridge = bridge.clone();
        move |level: u64| {
            bridge.spawn_set_permission(level);
        }
    });
    // 工具模式五选一（标准/极限·8/极限·6/极限·4/创造；minimal:dsh 已移除）；创造模式用预设基底 custom_tools。
    let on_tool_mode: Arc<dyn Fn(i32) + 'static> = Arc::new({
        let bridge = bridge.clone();
        move |index: i32| {
            let mode = tool_mode_from_index(index);
            let custom_tools = if mode == CUSTOM {
                CUSTOM_MODE_DEFAULT_TOOLS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                Vec::new()
            };
            bridge.spawn_set_tool_mode(mode, custom_tools);
        }
    });
    let on_stop = {
        let bridge = bridge.clone();
        move || {
            bridge.spawn_conversation_command(
                qaqh_client::ConversationCommand::ConversationCancel { turn_id: None },
            )
        }
    };
    // 队列移除：queue 已随 B 组本地化恒空（本地无排队概念，WebView 移除），
    // 保留绑定签名兼容 queue_row，但无实际动作。
    let on_queue_remove: Arc<dyn Fn(String) + 'static> = Arc::new({
        let _bridge = bridge.clone();
        move |_id: String| {}
    });

    // ── 渲染时读取草稿（版本号变化触发本函数重跑）────────────────
    let d = draft.borrow();
    let text = d.text.clone();
    let attachments = d.attachments.clone();
    let selected_slash = d.selected_slash;
    let dismissed = d.dismissed_slash.clone();
    // slash 菜单可见性（对齐 Web visibleSlashCommands）。
    let slash_cmds = if dismissed.as_deref() == Some(text.as_str()) {
        Vec::new()
    } else {
        match_slash_commands(&text)
    };
    let slash_visible = !slash_cmds.is_empty();
    drop(d);

    // Enter 发送 accelerator（菜单可见时 Enter = 选中命令，否则发送）。
    let on_enter = {
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let slash_cmds = slash_cmds.clone();
        let selected_slash = selected_slash;
        let on_submit = on_submit.clone();
        move || {
            if !slash_cmds.is_empty() {
                let idx = selected_slash % slash_cmds.len();
                let (cmd, _, _) = &slash_cmds[idx];
                let mut d = draft.borrow_mut();
                d.text = cmd.clone();
                d.selected_slash = 0;
                d.dismissed_slash = Some(cmd.clone());
                drop(d);
                let v = *draft_ver.borrow() + 1;
                *draft_ver.borrow_mut() = v;
                set_draft_version.call(v);
            } else {
                on_submit();
            }
        }
    };
    // 菜单导航（仅菜单可见时绑定 ↓↑ Esc）。
    let on_slash_next = {
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let len = slash_cmds.len();
        move || {
            let mut d = draft.borrow_mut();
            d.selected_slash = (d.selected_slash + 1) % len.max(1);
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    };
    let on_slash_prev = {
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let len = slash_cmds.len();
        move || {
            let mut d = draft.borrow_mut();
            d.selected_slash = (d.selected_slash + len - 1) % len.max(1);
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    };
    let on_slash_dismiss = {
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        let text = text.clone();
        move || {
            let mut d = draft.borrow_mut();
            d.dismissed_slash = Some(text.clone());
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    };
    // 菜单项点击选中。
    let on_slash_pick: Arc<dyn Fn(String) + 'static> = Arc::new({
        let draft = draft.clone();
        let draft_ver = draft_ver.clone();
        let set_draft_version = set_draft_version.clone();
        move |cmd: String| {
            let mut d = draft.borrow_mut();
            d.text = cmd.clone();
            d.selected_slash = 0;
            d.dismissed_slash = Some(cmd);
            drop(d);
            let v = *draft_ver.borrow() + 1;
            *draft_ver.borrow_mut() = v;
            set_draft_version.call(v);
        }
    });

    // ── 渲染树 ───────────────────────────────────────────────────
    let placeholder = if has_pending_gate {
        "请先处理当前授权请求"
    } else {
        "向 QAQ-Harness 提问…"
    };

    // TextBox + Enter accelerator（菜单可见时附加 ↓↑ Esc）。
    // A1 去卡中卡：零边框 + 同卡底色（LayerFill），输入区直接坐进外层
    // command_surface 卡，消除双边框冗余感；高度基准 56（Fluent 2 输入）。
    let mut input: Element = text_box(text.clone())
        .multiline()
        .placeholder_text(placeholder)
        .height(input_height)
        .border_thickness(Thickness::xy(0.0, 0.0))
        .on_text_changed(on_text_changed)
        .keyboard_accelerator(KeyboardAccelerator::new(
            VirtualKey::Enter,
            VirtualKeyModifiers::None,
            on_enter,
        ))
        .automation_name("消息输入")
        .automation_id("composer-input")
        .with_key("composer-textbox")
        .background(ThemeRef::LayerFill)
        .into();
    if slash_visible {
        input = input
            .keyboard_accelerator(KeyboardAccelerator::new(
                VirtualKey::Down,
                VirtualKeyModifiers::None,
                on_slash_next,
            ))
            .keyboard_accelerator(KeyboardAccelerator::new(
                VirtualKey::Up,
                VirtualKeyModifiers::None,
                on_slash_prev,
            ))
            .keyboard_accelerator(KeyboardAccelerator::new(
                VirtualKey::Escape,
                VirtualKeyModifiers::None,
                on_slash_dismiss,
            ));
    }

    // 原生附件菜单：让 WinUI 负责弹层、焦点、键盘和无障碍语义。
    let attach_button = button("")
        .icon(Symbol::Attach)
        .subtle()
        .menu_flyout(vec![menu_item("上传图片"), menu_item("上传文本")])
        .on_item_clicked(move |label: String| match label.as_str() {
            "上传图片" => on_pick_image(),
            "上传文本" => on_pick_text(),
            _ => {}
        })
        .tooltip("添加附件")
        .automation_name("添加附件")
        .automation_id("composer-attach");

    // 附件预览行（图片：缩略图 + 文件名；文本：类型徽标 + 文件名）。
    let mut attach_rows: Vec<Element> = Vec::new();
    for (i, att) in attachments.iter().enumerate() {
        // 图片缩略图：file:// URI 加载 %TEMP% 预览副本（48x48，等比裁切）。
        let thumb: Element = match (&att.kind, &att.preview_path) {
            (AttachmentKind::Image { .. }, Some(p)) => {
                let uri = format!("file:///{}", p.replace('\\', "/"));
                border(
                    Image::new_with_uri(uri)
                        .width(48.0)
                        .height(48.0)
                        .stretch(Stretch::UniformToFill),
                )
                .corner_radius(4.0)
                .into()
            }
            (AttachmentKind::Image { .. }, None) => qaqh_fluent::metadata_badge("IMG"),
            (AttachmentKind::Text, _) => qaqh_fluent::metadata_badge("TXT"),
        };
        let remove_name = format!("移除附件 {}", att.file_name);
        let row: Element = qaqh_fluent::inset_surface(
            hstack((
                thumb,
                text_block(format!("{} ({})", att.file_name, att.size_label()))
                    .font_size(tokens::TYPE_CAPTION)
                    .foreground(ThemeRef::SecondaryText),
                button("")
                    .icon(Symbol::Cancel)
                    .subtle()
                    .tooltip(remove_name.clone())
                    .automation_name(remove_name)
                    .automation_id(format!("composer-remove-{}", att.id))
                    .on_click({
                        let on_remove_attach = on_remove_attach.clone();
                        let id = att.id.clone();
                        move || on_remove_attach(id.clone())
                    }),
            ))
            .spacing(tokens::SPACE_2),
        )
        .transition(motion::reveal(), motion::content_exit())
        .automation_name(format!("附件 {}", att.file_name));
        attach_rows.push(row.with_key(format!("att-{i}-{}", att.id)));
    }
    let attach_preview: Element = if attach_rows.is_empty() {
        Element::Empty
    } else {
        vstack(attach_rows).spacing(tokens::SPACE_1).into()
    };

    // submitError 行（Web 失败回填；壳保留草稿不清空）。
    // 空态用 Element::Empty：零高 grid(()) 占位会在 vstack spacing 两侧产生
    // 幻影空隙（A1 高度回收的一部分）。
    let error_row: Element = if state.submit_error.is_empty() {
        Element::Empty
    } else {
        text_block(&state.submit_error)
            .font_size(tokens::TYPE_CAPTION)
            .foreground(ThemeRef::SystemCritical)
            .wrap()
            .accessibility_live_setting(AutomationLiveSetting::Assertive)
            .automation_name(format!("发送失败：{}", state.submit_error))
            .automation_id("composer-submit-error")
            .into()
    };

    // 发送/停止按钮。
    let can_send = (!text.trim().is_empty() || !attachments.is_empty()) && !has_pending_gate;
    let send_stop: Element = if is_streaming {
        button("")
            .icon(Symbol::Stop)
            .subtle()
            .tooltip("停止生成")
            .automation_name("停止生成")
            .automation_id("composer-stop")
            .on_click(on_stop)
            .into()
    } else {
        button("")
            .icon(Symbol::Send)
            .accent()
            .enabled(can_send)
            .tooltip("发送消息 (Enter)")
            .automation_name("发送消息")
            .automation_id("composer-send")
            .on_click({
                let cb = on_submit.clone();
                move || cb()
            })
            .into()
    };

    // 权限语义 chip（A4）：`L1▼` ComboBox → `权限 L{n}` MenuFlyout，四项
    // 带一句话说明（黑话 L1 不再裸奔）。守卫等价迁移：menu 无程序化同步
    // 事件，但 rendered_pl==0（config 未加载）时按钮禁用 + 回调内再拦一道
    // （permission_change_allowed，Bug#2 语义）——此前该窗口未闭合导致每次
    // 启动都把权限误写成 L1 并持久化。
    let pl = state.permission_level;
    let perm_label = if pl == 0 {
        "权限 …".to_string()
    } else {
        format!("权限 L{pl}")
    };
    let mut permission_button = button(perm_label)
        .icon(Icon::symbol(Symbol::Permissions))
        .subtle()
        .menu_flyout(PERMISSION_MENU.iter().map(|(_, text)| menu_item(*text)).collect())
        .on_item_clicked({
            let on_permission = on_permission.clone();
            let rendered_pl = pl;
            move |label: String| {
                if let Some(lvl) = permission_menu_level(&label) {
                    if permission_change_allowed(rendered_pl, lvl) {
                        on_permission(lvl);
                    }
                }
            }
        })
        .tooltip("权限级别：控制哪些操作自动批准")
        .automation_name("权限级别")
        .automation_id("composer-permission-level");
    if pl == 0 {
        permission_button = permission_button.enabled(false);
    }
    let permission_picker: Element = permission_button.into();

    // 工具模式五选一（标准/极限·8/极限·6/极限·4/创造；PLAN-TOOL-MODES.md，minimal:dsh 已移除）。
    // 空态（新会话 meta.tool_mode 为空）渲染为 standard(0) 而非 -1：-1 会被 WinUI
    // 规范化触发程序化 SelectionChanged，而旧守卫把所有空态 SelectionChanged 都当
    // 同步事件丢弃，导致空态会话的工具模式选择永久失效（BUG-017）。放行/跳过判定
    // 收敛到 `tool_mode_change_is_user`（mod.rs），覆盖挂载/会话切换同步与用户点击。
    let tm = state.tool_mode.clone();
    let tool_mode_picker: Element = qaqh_fluent::solid_combo_box(TOOL_MODE_OPTIONS)
        .selected_index(tool_mode_index(&tm))
        .on_selection_changed({
            let rendered_tm = tm;
            move |index: i32| {
                if !tool_mode_change_is_user(&rendered_tm, index) {
                    log_diag(&format!(
                        "tool_mode: sync event skipped (rendered='{rendered_tm}' index={index})"
                    ));
                    return;
                }
                on_tool_mode(index);
            }
        })
        .width(88.0)
        .tooltip("工具模式")
        .automation_name("工具模式")
        .automation_id("composer-tool-mode")
        .into();

    // 执行/规划 toggle chip（A5）：图标+文；规划（非默认态）用 accent 底作
    // Fluent toggle 选中语言，执行态 subtle。点击互切（spawn_set_mode）。
    let mode_button: Element = if state.mode == "plan" {
        button("规划")
            .icon(Icon::symbol(Symbol::List))
            .accent()
            .tooltip("规划模式（点击切回执行）")
            .automation_name("工作模式")
            .automation_id("composer-mode")
            .on_click(on_mode_toggle)
            .into()
    } else {
        button("执行")
            .icon(Icon::symbol(Symbol::Play))
            .subtle()
            .tooltip("执行模式（点击切换规划）")
            .automation_name("工作模式")
            .automation_id("composer-mode")
            .on_click(on_mode_toggle)
            .into()
    };
    // ⤢ 沉浸式（A6）：移出 footer，与拖拽 grip 同行（顶部条右端），
    // footer 减负；hover 显形需 IsHitTestVisible（vendor 未投影，隐形控件
    // 会拦截输入区点击），故常显 subtle。
    let immersive_button: Element = button("")
        .icon(Icon::symbol(if immersive {
            Symbol::BackToWindow
        } else {
            Symbol::FullScreen
        }))
        .subtle()
        .height(28.0)
        .tooltip(if immersive {
            "退出沉浸式编辑"
        } else {
            "展开编辑器"
        })
        .automation_name(if immersive {
            "退出沉浸式编辑"
        } else {
            "展开编辑器"
        })
        .automation_id("composer-immersive")
        .on_click({
            let set_immersive = set_immersive.clone();
            let set_manual_height = set_manual_height.clone();
            let set_input_height = set_input_height.clone();
            move || {
                let next = !immersive;
                set_immersive.call(next);
                set_manual_height.call(false);
                set_input_height.call(if next {
                    INPUT_MANUAL_MAX_HEIGHT
                } else {
                    INPUT_DEFAULT_HEIGHT
                });
            }
        })
        .into();

    // 工作区入口（A2）：已选目录 → 标题栏 chip（header.rs footer，恢复
    // 挂账的 on_workspace 合并流）；卡上仅未选目录时保留一次性入口。
    // 点击 → 系统选目录 → 有活动会话走 workspace.set（粘性写 meta.cwd +
    // 自动归属），无会话走 workspace.create + 选中（作为下个新会话
    // SessionCreate 携带的 cwd）。
    let cwd_display = state.cwd.clone();
    let cwd_entry_visible = cwd_display.as_deref().map(str::is_empty).unwrap_or(true);
    let workspace_chip: Element = if cwd_entry_visible {
        button("选择工作目录")
            .icon(Icon::symbol(Symbol::Folder))
            .subtle()
            .tooltip("选择会话工作目录")
            .automation_name("工作区")
            .automation_id("composer-workspace")
            .on_click({
                let bridge = bridge.clone();
                move || match bridge.pick_workspace_directory() {
                    Ok(serde_json::Value::String(path)) => {
                        // 空 path 防护：永不向 daemon 发空 cwd（后端已拒绝，
                        // 这里同步拦截，避免无意义往返）。
                        if path.trim().is_empty() {
                            return;
                        }
                        if !bridge.core().active_seed().is_empty() {
                            bridge.spawn_workspace_set(path);
                        } else {
                            bridge.spawn_workspace_create(path);
                        }
                    }
                    _ => {}
                }
            })
            .into()
    } else {
        Element::Empty
    };

    // Grid provides real left/right command groups. A horizontal StackPanel
    // cannot emulate a web flex spacer because it measures children at infinity.
    // 列按需组装（A2 工作目录迁标题栏、A6 ⤢ 移顶部条后）：附件 | 工具模式 |
    // 模式 chip | [工作目录空态入口] | 弹性空白 | 权限 | 发送 —— 常态 6 项。
    let mut footer_cells: Vec<Element> = vec![
        attach_button.grid_column(0).into(),
        tool_mode_picker.grid_column(1),
        mode_button.grid_column(2),
    ];
    let mut footer_cols: Vec<GridLength> = vec![
        GridLength::Auto,
        GridLength::Auto,
        GridLength::Auto,
    ];
    let mut col = 3;
    if cwd_entry_visible {
        footer_cols.push(GridLength::Auto);
        footer_cells.push(workspace_chip.grid_column(col));
        col += 1;
    }
    footer_cols.push(GridLength::STAR);
    footer_cols.push(GridLength::Auto);
    footer_cells.push(permission_picker.grid_column(col));
    footer_cells.push(send_stop.grid_column(col + 1));
    let footer: Element = grid(footer_cells)
        .columns(footer_cols)
        .column_spacing(tokens::SPACE_2)
        .into();

    // Twelve-DIP hit target with a quiet two-DIP visual grip. Dragging upward
    // grows the editor; tapping returns ownership to automatic sizing.
    // A6：条加高到 28 与 ⤢ 沉浸式按钮同行（右端）， grip 垂直居中。
    let grip: Element = border(text_block(""))
        .width(40.0)
        .height(2.0)
        .corner_radius(1.0)
        .background(ThemeRef::DividerStroke)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .into();
    let resize_handle: Element = border(grip)
        .height(28.0)
        .capture_pointer_on_press()
        .on_pointer_pressed({
            let resize_start = resize_start.clone();
            move |info: PointerEventInfo| {
                *resize_start.borrow_mut() = Some((info.window_y, input_height));
            }
        })
        .on_pointer_moved({
            let resize_start = resize_start.clone();
            let set_input_height = set_input_height.clone();
            let set_manual_height = set_manual_height.clone();
            move |info: PointerEventInfo| {
                if !info.is_left_button_pressed {
                    return;
                }
                let Some((start_y, start_height)) = *resize_start.borrow() else {
                    return;
                };
                set_manual_height.call(true);
                set_input_height.call(
                    (start_height - (info.window_y - start_y))
                        .clamp(INPUT_MIN_HEIGHT, INPUT_MANUAL_MAX_HEIGHT),
                );
            }
        })
        .on_pointer_released({
            let resize_start = resize_start.clone();
            move |_| *resize_start.borrow_mut() = None
        })
        .on_pointer_capture_lost({
            let resize_start = resize_start.clone();
            move || *resize_start.borrow_mut() = None
        })
        .on_tapped({
            let set_manual_height = set_manual_height.clone();
            let set_input_height = set_input_height.clone();
            move || {
                set_manual_height.call(false);
                set_input_height.call(INPUT_DEFAULT_HEIGHT);
            }
        })
        .tooltip("拖动调整输入框高度；单击恢复自动高度")
        .automation_name("调整输入框高度")
        .automation_id("composer-resize-handle")
        .into();
    // 顶部条（A6）：拖拽 grip 满宽 + ⤢ 沉浸式右端；两列并排不叠压，
    // 按钮点击不会落进拖拽命中区。
    let top_strip: Element = grid((
        resize_handle.grid_column(0),
        immersive_button.grid_column(1),
    ))
    .columns([GridLength::STAR, GridLength::Auto])
    .into();

    // 上下文构成堆叠条（A3 降级为卡底贴边线）：6 段 token 分布（对齐 Web
    // ContextPanel 饼图语义：对话/思考/工具调用/工具结果/工具定义/系统提示），
    // 2px 高贴卡片下缘（卡 padding bottom=0 + WinUI Border 圆角裁切），
    // 短计数 caption 叠在线右端（线从文字下穿过），全量数字进 tooltip。
    // 悬停命中：整行容器与各段都挂 tooltip（细条难 hover，容器兜底）。
    // 文件缺失或全零（无会话/回合未结束）时隐藏。
    let ctx_bar: Element = match ctx_stats.as_ref() {
        Some(s) if s.total() > 0 => {
            let total = s.total();
            let segs: [(u64, &str, ThemeRef); 6] = [
                (s.chat_text, "对话", ThemeRef::Accent),
                (s.thinking, "思考", ThemeRef::SystemCaution),
                (s.tool_calls, "工具调用", ThemeRef::SystemAttention),
                (s.tool_results, "工具结果", ThemeRef::SystemSuccess),
                (s.tools_schema, "工具定义", ThemeRef::SystemNeutral),
                (s.system_prompt, "系统提示", ThemeRef::SystemCritical),
            ];
            let mut cells: Vec<Element> = Vec::with_capacity(6);
            let mut weights: Vec<GridLength> = Vec::with_capacity(6);
            // 容器 tooltip：完整构成（多行，ToolTip TextBlock 支持 \n）。
            let mut summary = String::from("上下文构成");
            for (i, (v, name, color)) in segs.iter().enumerate() {
                let pct = *v as f64 * 100.0 / total as f64;
                cells.push(
                    border(text_block(""))
                        .background(color.clone())
                        .tooltip(format!("{name} {pct:.1}% · {} tokens", fmt_thousands(*v)))
                        .automation_name(format!("{name} {pct:.1}%"))
                        .grid_column(i as i32)
                        .into(),
                );
                // 权重 0 的段给极小值（Star(0) 列不参与分配）。
                weights.push(GridLength::Star((*v as f64).max(0.001)));
                summary.push_str(&format!(
                    "\n{name} {pct:.1}% · {} tokens",
                    fmt_thousands(*v)
                ));
            }
            // 2px 分布线（贴底）+ caption 同格叠放：caption 后挂 → 渲染在线上，
            // 线成为其下划线；行高 = caption 行高，不新增独立行。
            let line: Element = border(grid(cells).columns(weights).column_spacing(1.0))
                .height(2.0)
                .corner_radius(1.0)
                .vertical_alignment(VerticalAlignment::Bottom)
                .tooltip(summary.clone())
                .into();
            let caption: Element = text_block(fmt_tokens_short(total))
                .font_size(10.0)
                .foreground(ThemeRef::SecondaryText)
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Bottom)
                .tooltip(summary.clone())
                .automation_name(format!("{} tokens", fmt_thousands(total)))
                .into();
            grid((line, caption))
                .tooltip(summary.clone())
                .automation_name(summary)
                .with_key("ctx-bar")
                .into()
        }
        _ => grid(()).with_key("ctx-bar-empty").into(),
    };

    // 悬浮命令卡（A1+批次 B2）：LayerFill + 圆角 8 + elevation 16（ThemeShadow
    // 落在直接父面板）；卡 padding bottom=0 让 token 线贴下缘（圆角裁切）。
    let card: Element = qaqh_fluent::elevated_command_surface(
        vstack((
            top_strip,
            input,
            error_row,
            attach_preview,
            footer,
            ctx_bar,
        ))
        .spacing(tokens::SPACE_2)
        .padding(Thickness {
            left: tokens::SPACE_3,
            top: tokens::SPACE_2,
            right: tokens::SPACE_3,
            bottom: 0.0,
        }),
        16.0,
    )
    .automation_name("消息编辑器")
    .automation_id("composer-surface");

    // 队列行（queue_count > 0 时；空态不挂载，消除 spacing 幻影空隙）。
    let queue_bar: Element = if state.queue_count > 0 {
        queue_row(&state, on_queue_remove)
    } else {
        Element::Empty
    };

    // slash 菜单（可见时；composer 卡片上方 cell）。
    let slash_menu: Element = if slash_visible {
        let mut items: Vec<Element> = Vec::new();
        for (i, (cmd, label, desc)) in slash_cmds.iter().enumerate() {
            let selected = i == selected_slash;
            let mut opt = button(format!("{cmd}  {label}  {desc}"))
                .subtle()
                .automation_name(format!("选择命令 {cmd}"))
                .automation_id(format!("composer-slash-{i}"))
                .on_click({
                    let on_slash_pick = on_slash_pick.clone();
                    let cmd = cmd.clone();
                    move || on_slash_pick(cmd.clone())
                });
            if selected {
                opt = opt.accent();
            }
            items.push(opt.into());
        }
        border(vstack(items).spacing(2.0).padding(6.0))
            .corner_radius(6.0)
            .background(ThemeRef::CardBackground)
            .transition(motion::reveal(), motion::content_exit())
            .with_key("composer-slash-menu")
            .into()
    } else {
        Element::Empty
    };

    // 工作状态栏（输入框之上最顶行）。空闲且无胶囊时整行不挂载：
    // 零高占位会在外层 vstack 两侧产生 spacing 幻影空隙（A1 高度回收）。
    let status_bar: Element =
        if matches!(state.phase, WorkPhase::Idle) && state.subagents.is_empty() {
            Element::Empty
        } else {
            work_status_bar(cx, &state)
        };

    vstack((status_bar, queue_bar, slash_menu, card))
        .spacing(tokens::SPACE_2)
        .padding(Thickness {
            left: tokens::SPACE_6,
            top: tokens::SPACE_3,
            right: tokens::SPACE_6,
            bottom: tokens::SPACE_3,
        })
        .max_width(tokens::CONVERSATION_MAX_WIDTH)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .into()
}
