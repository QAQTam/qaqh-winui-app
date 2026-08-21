use super::super::*;
use crate::diagnostics::{self, Mode};

pub(crate) fn multimodal_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("多模态"));
    let rendered_mm_enabled = d.mm_enabled;
    rows.push(field_row(
        "启用",
        ToggleSwitch::new(rendered_mm_enabled)
            .header("")
            .on_toggled({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |on: bool| {
                    if on == rendered_mm_enabled {
                        return;
                    }
                    draft.borrow_mut().mm_enabled = on;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(field_row(
        "提供方类型",
        text_box(d.mm_provider_type.clone())
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().mm_provider_type = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(field_row(
        "API Key",
        PasswordBox::new()
            .value(d.mm_api_key.clone())
            .placeholder_text(if d.mm_api_key_configured {
                "输入新值以替换"
            } else {
                "sk-…"
            })
            .on_password_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().mm_api_key = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(
        text_block("留空 = 保持不变；输入新值 = 替换（密钥已加密存储，界面不回显）")
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    );
    rows.push(field_row(
        "Base URL",
        text_box(d.mm_base_url.clone())
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().mm_base_url = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(field_row(
        "模型",
        text_box(d.mm_model.clone())
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().mm_model = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    let rendered_mm_max = (d.mm_max_tokens as f64).clamp(16.0, 1_000_000.0);
    rows.push(field_row(
        "最大 Tokens",
        NumberBox::new(d.mm_max_tokens as f64)
            .range(16.0, 1_000_000.0)
            .header("")
            .on_value_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v: f64| {
                    if (v - rendered_mm_max).abs() < f64::EPSILON {
                        return;
                    }
                    draft.borrow_mut().mm_max_tokens = v as u64;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
}

pub(crate) fn advanced_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let bridge = ctx.bridge.clone();
    let draft = ctx.draft.clone();
    let proj_draft = ctx.proj_draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    let pd = ctx.pd.clone();
    let set_diag_rev = ctx.set_diag_rev.clone();
    let set_perm_desc = ctx.set_perm_desc.clone();
    let set_export_path = ctx.set_export_path.clone();
    let diag_rev = ctx.diag_rev;
    let perm_desc = ctx.perm_desc;
    let export_path = ctx.export_path.clone();
    rows.push(section_title("通知"));
    let rendered_notif = bridge.core().notif_enabled();
    rows.push(field_row(
        "桌面通知",
        ToggleSwitch::new(rendered_notif)
            .header("")
            .on_toggled({
                let bridge = bridge.clone();
                move |on: bool| {
                    if on == rendered_notif {
                        return;
                    }
                    bridge.spawn_set_notif_pref(on);
                }
            })
            .into(),
    ));
    rows.push(
        text_block("回合完成后弹出系统通知；点击通知可回到窗口")
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    );
    rows.push(section_title("权限控制"));
    // UAC 安全设置范式：4 档滑杆（离散步进）+ 档位标签 + 当前档说明。
    // permission_level==0（config 未加载/失败）：不渲染滑杆——clamp(1,4)
    // 会把 0 显示成 L1，误导用户以为权限被重置。
    let raw_level = pd.permission_level;
    if raw_level == 0 {
        rows.push(
            text_block("权限配置加载中…")
                .font_size(11.0)
                .foreground(ThemeRef::SecondaryText)
                .into(),
        );
    } else {
        let level = raw_level.clamp(1, 4) as u8;
        let desc_idx = if (1..=4).contains(&perm_desc) {
            (perm_desc - 1) as usize
        } else {
            (level - 1) as usize
        };
        let tick_row: Element = hstack(
            PERMISSION_TICKS
                .iter()
                .map(|t| text_block(*t).width(76.0).font_size(11.0).into())
                .collect::<Vec<Element>>(),
        )
        .spacing(0.0)
        .into();
        let desc_title: Element = text_block(PERMISSION_LADDER[desc_idx].0)
            .font_size(13.0)
            .foreground(ThemeRef::AccentText)
            .into();
        let desc_body: Element = text_block(PERMISSION_LADDER[desc_idx].1)
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into();
        let desc_row: Element = vstack((desc_title, desc_body)).spacing(2.0).into();
        let perm_slider: Element = Slider::new(level as f64)
            .range(1.0, 4.0)
            .step(1.0)
            .header("")
            .on_value_changed({
                let bridge = bridge.clone();
                let proj_draft = proj_draft.clone();
                let dirty = dirty.clone();
                let set_perm = set_perm_desc.clone();
                // 防程序化同步误触发：渲染应用 Slider value 会触发 ValueChanged，
                // 值等于渲染时的实际权限则视为同步事件跳过（不写回、不标 dirty）。
                let rendered_level = level as u64;
                move |v: f64| {
                    let lvl = v.round().clamp(1.0, 4.0) as u64;
                    if lvl == rendered_level {
                        return;
                    }
                    proj_draft.borrow_mut().permission_level = lvl;
                    *dirty.borrow_mut() = true;
                    bridge.spawn_set_permission(lvl);
                    set_perm.call(lvl as u8);
                }
            })
            .into();
        rows.push(field_row(
            "权限等级",
            vstack((perm_slider, tick_row, desc_row))
                .spacing(6.0)
                .into(),
        ));
    }
    rows.push(section_title("性能"));
    let tokenizer_row: Element = {
        let input: Element = text_box(d.tokenizer_path.clone())
            .placeholder_text("path/to/tokenizer.json")
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().tokenizer_path = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into();
        let browse = button("浏览…").subtle().on_click({
            let bridge = bridge.clone();
            let draft = draft.clone();
            let dirty = dirty.clone();
            move || {
                if let Ok(serde_json::Value::String(path)) = bridge.pick_file() {
                    draft.borrow_mut().tokenizer_path = path;
                    *dirty.borrow_mut() = true;
                }
            }
        });
        hstack((input, browse)).spacing(8.0).into()
    };
    rows.push(field_row("Tokenizer 路径", tokenizer_row));

    // ── 诊断（纯本地 · 白名单 · 三档采集）────────────────────────
    // 隐私合规是结构性保证：白名单字段、仅性能侧、无系统指纹、默认 ZDR。
    rows.push(section_title("诊断"));
    let cur_mode = diagnostics::mode();
    let mode_idx = match cur_mode {
        Mode::Full => 0,
        Mode::Minimal => 1,
        Mode::Zero => 2,
    };
    let rendered_mode_idx = mode_idx;
    rows.push(field_row(
        "数据采集",
        RadioButtons::new(vec![
            "完整数据采集".to_string(),
            "最小数据采集".to_string(),
            "ZDR（零诊断数据记录）".to_string(),
        ])
        .selected_index(rendered_mode_idx)
        .header("")
        .on_selection_changed({
            let set_diag_rev = set_diag_rev.clone();
            move |i: i32| {
                if i == rendered_mode_idx {
                    return;
                }
                let m = match i.max(0) {
                    0 => Mode::Full,
                    1 => Mode::Minimal,
                    _ => Mode::Zero,
                };
                diagnostics::set_mode(m);
                set_diag_rev.call(diag_rev + 1);
            }
        })
        .into(),
    ));
    rows.push(
        text_block(
            "仅采集运行时性能数据（渲染耗时 / 帧间隔 / 事件吞吐），不含对话内容、路径或系统指纹；唯一系统信息为 OS 版本号。数据仅保存在本机，导出后可审查。默认 ZDR，不主动记录任何数据。",
        )
        .font_size(11.0)
        .foreground(ThemeRef::SecondaryText)
        .into(),
    );
    let status_text = format!(
        "当前：{} · 缓冲 {} 条事件",
        cur_mode.label(),
        diagnostics::buffered_events()
    );
    let export_btn = button("导出 JSON")
        .subtle()
        .enabled(cur_mode != Mode::Zero)
        .on_click({
            let set_export_path = set_export_path.clone();
            move || {
                if let Some(p) = diagnostics::export_to_file() {
                    set_export_path.call(Some(p));
                }
            }
        });
    let export_status: Element = match export_path.clone() {
        Some(p) => text_block(format!("已导出：{p}"))
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
        None => text_block(status_text)
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    };
    rows.push(field_row(
        "导出诊断包",
        vstack((export_btn, export_status)).spacing(4.0).into(),
    ));
}

pub(crate) fn remote_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let bridge = ctx.bridge.clone();
    let remote_url = ctx.remote_url.clone();
    let remote_token = ctx.remote_token.clone();
    let remote_status = ctx.remote_status.clone();
    let set_remote_url = ctx.set_remote_url.clone();
    let set_remote_token = ctx.set_remote_token.clone();
    let set_remote_status = ctx.set_remote_status.clone();
    rows.push(section_title("远端 daemon（临时跨端模式）"));
    let current = bridge.core().remote_profile_snapshot();
    let default_status = match &current {
        Some(p) => format!("当前远端：{}（路径显示为 //ip/…）", p.base_url),
        None => "当前：本地 daemon（默认）".to_string(),
    };
    rows.push(field_row(
        "远端地址",
        text_box(remote_url.clone())
            .placeholder_text("http://192.168.1.10:64413")
            .on_text_changed({
                let set_remote_url = set_remote_url.clone();
                move |v| set_remote_url.call(v)
            })
            .into(),
    ));
    rows.push(field_row(
        "访问令牌",
        PasswordBox::new()
            .value(remote_token.clone())
            .placeholder_text("qaqh-daemon server --token 的值")
            .on_password_changed({
                let set_remote_token = set_remote_token.clone();
                move |v| set_remote_token.call(v)
            })
            .into(),
    ));
    let connect_btn = button("连接并切换").accent().on_click({
        let bridge = bridge.clone();
        let url = remote_url.clone();
        let token = remote_token.clone();
        let set_remote_status = set_remote_status.clone();
        move || {
            if url.trim().is_empty() {
                set_remote_status.call("请先填写 http://ip:port".to_string());
                return;
            }
            bridge
                .core()
                .apply_remote_profile(url.clone(), token.clone());
            set_remote_status.call("已保存档案，正在切换并重连…".to_string());
        }
    });
    let disconnect_btn = button("恢复本地 daemon")
        .subtle()
        .enabled(current.is_some())
        .on_click({
            let bridge = bridge.clone();
            let set_remote_status = set_remote_status.clone();
            move || {
                bridge.core().clear_remote_profile();
                set_remote_status.call("已清除远端档案，正在切回本地…".to_string());
            }
        });
    let status_text = if remote_status.is_empty() {
        default_status
    } else {
        remote_status.clone()
    };
    rows.push(field_row(
        "操作",
        vstack((
            hstack((connect_btn, disconnect_btn)).spacing(8.0),
            text_block(status_text)
                .font_size(11.0)
                .foreground(ThemeRef::SecondaryText),
        ))
        .spacing(6.0)
        .into(),
    ));
    rows.push(
        text_block(
            "远端模式下：文件与工具操作都发生在 daemon 所在机器；路径仅按 //ip/路径 显示，不当作本机路径使用。",
        )
        .font_size(11.0)
        .foreground(ThemeRef::SecondaryText)
        .into(),
    );
}
