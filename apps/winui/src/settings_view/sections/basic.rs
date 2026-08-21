use super::super::*;
use crate::fonts;
use crate::oobe_view;
use crate::shell_store::normalize_effort;
use qaqh_fluent::tokens;

pub(crate) fn models_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let bridge = ctx.bridge.clone();
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("模型提供方"));

    // ── 加载态：daemon 未响应时显示「加载中」而非错误的「无 provider 目录」 ──
    if !d.loaded {
        rows.push(
            text_block("正在加载配置…")
                .font_size(13.0)
                .foreground(ThemeRef::SecondaryText)
                .into(),
        );
        // 跳过其余字段渲染（避免显示 0 默认值）；保存按钮仍在底部可用。
    } else {
        let providers = d.providers.clone();
        let provider_names: Vec<String> = providers.iter().map(|p| p.display.clone()).collect();
        let pidx = providers
            .iter()
            .position(|p| p.id == d.provider_id)
            .unwrap_or(0) as i32;
        let provider_combo = if provider_names.is_empty() {
            // 已加载但 providers 为空 = daemon 异常（不应发生，registry 硬编码 10 个）。
            text_block("（未配置任何 provider，请检查 daemon）")
                .foreground(ThemeRef::SecondaryText)
                .into()
        } else {
            qaqh_fluent::solid_combo_box(provider_names)
                .selected_index(pidx)
                .header("Provider")
                .background(ThemeRef::SolidBackground)
                .on_selection_changed({
                    let providers = providers.clone();
                    let draft = draft.clone();
                    let dirty = dirty.clone();
                    let rendered_pidx = pidx;
                    move |i: i32| {
                        // 防程序化同步误触发：渲染期设置 selected_index 会触发 ValueChanged，
                        // 若值等于渲染时的索引则视为同步事件跳过（与 permission 滑杆同理）。
                        if i == rendered_pidx {
                            return;
                        }
                        let Some(p) = providers.get(i as usize) else {
                            return;
                        };
                        let mut d = draft.borrow_mut();
                        d.provider_id = p.id.clone();
                        // 仅当新 provider 不支持当前 endpoint 时才取首条 endpoint
                        // （保留用户已选的 Responses API 等偏好）。
                        let has_current = p.endpoints.iter().any(|e| e.id == d.endpoint);
                        if !has_current {
                            if let Some(ep) = p.endpoints.first() {
                                d.endpoint = ep.id.clone();
                                d.base_url = ep.base_url.clone();
                                if !ep.default_model.is_empty() {
                                    d.model = ep.default_model.clone();
                                }
                            }
                        } else {
                            // 同步 base_url 到当前 endpoint 在新 provider 下的预设。
                            // 注意：此处仍会覆盖自定义 base_url（用户显式切换 provider 视为意图跟随预设）；
                            // 但通过上面的 rendered_pidx 守卫，已避免因重渲染导致的意外覆盖。
                            if let Some(ep) = p.endpoints.iter().find(|e| e.id == d.endpoint) {
                                d.base_url = ep.base_url.clone();
                                if !ep.default_model.is_empty() && d.model.is_empty() {
                                    d.model = ep.default_model.clone();
                                }
                            }
                        }
                        *dirty.borrow_mut() = true;
                    }
                })
                .into()
        };
        rows.push(field_row("提供方", provider_combo));
        let endpoints = providers
            .iter()
            .find(|p| p.id == d.provider_id)
            .map(|p| p.endpoints.clone())
            .unwrap_or_default();
        // 使用 ui_label 显示协议 + Beta 标记，让用户能直观区分
        // Chat Completions API 与 Responses API (Beta)。
        let endpoint_labels: Vec<String> = endpoints.iter().map(|e| e.ui_label()).collect();
        let eidx = endpoints
            .iter()
            .position(|e| e.id == d.endpoint)
            .unwrap_or(0) as i32;
        let endpoint_combo = if endpoint_labels.is_empty() {
            text_block("—").foreground(ThemeRef::SecondaryText).into()
        } else {
            qaqh_fluent::solid_combo_box(endpoint_labels)
                .selected_index(eidx)
                .header("Endpoint")
                .background(ThemeRef::SolidBackground)
                .on_selection_changed({
                    let endpoints = endpoints.clone();
                    let draft = draft.clone();
                    let dirty = dirty.clone();
                    let rendered_eidx = eidx;
                    move |i: i32| {
                        if i == rendered_eidx {
                            return;
                        }
                        let Some(ep) = endpoints.get(i as usize) else {
                            return;
                        };
                        let mut d = draft.borrow_mut();
                        d.endpoint = ep.id.clone();
                        d.base_url = ep.base_url.clone();
                        // 仅当 model 为空时才回填 default_model，保留用户自定义模型
                        // （修复：此前无条件覆盖导致 opencode-go responses 等端点
                        //  任何渲染都会把自定义模型重置为 grok-4.5）。
                        if !ep.default_model.is_empty() && d.model.is_empty() {
                            d.model = ep.default_model.clone();
                        }
                        *dirty.borrow_mut() = true;
                    }
                })
                .into()
        };
        rows.push(field_row("接口", endpoint_combo));
        rows.push(field_row(
            "Base URL",
            text_box(d.base_url.clone())
                .on_text_changed({
                    let draft = draft.clone();
                    let dirty = dirty.clone();
                    move |v| {
                        draft.borrow_mut().base_url = v;
                        *dirty.borrow_mut() = true;
                    }
                })
                .into(),
        ));
        let model_names = endpoints
            .iter()
            .find(|e| e.id == d.endpoint)
            .map(|e| e.models.clone())
            .unwrap_or_default();
        // 模型：可编辑文本框（models 列表仅作提示，不强制选择）。
        rows.push(field_row(
            "模型",
            text_box(d.model.clone())
                .placeholder_text(if model_names.is_empty() {
                    "e.g. deepseek-chat"
                } else {
                    ""
                })
                .on_text_changed({
                    let draft = draft.clone();
                    let dirty = dirty.clone();
                    move |v| {
                        draft.borrow_mut().model = v;
                        *dirty.borrow_mut() = true;
                    }
                })
                .into(),
        ));
        // 下界 16（小模型可用），上界 1_000_000（主流长输出模型已支持 128K+）。
        const MAX_TOKENS_MIN: f64 = 16.0;
        const MAX_TOKENS_MAX: f64 = 1_000_000.0;
        // 守卫对比"渲染值钳到合法区间后的结果"：草稿值为 0/越界时 WinUI 会把
        // 控件值钳到区间端点并回发 ValueChanged（如 0→16），若对比原始 0 会误判
        // 为用户输入而把区间端点写回草稿（根因 3 同类）。
        let rendered_max_tokens = (d.max_tokens as f64).clamp(MAX_TOKENS_MIN, MAX_TOKENS_MAX);
        rows.push(field_row(
            "最大 Tokens",
            NumberBox::new(d.max_tokens as f64)
                .range(MAX_TOKENS_MIN, MAX_TOKENS_MAX)
                .header("")
                .on_value_changed({
                    let draft = draft.clone();
                    let dirty = dirty.clone();
move |v: f64| {
                    if (v - rendered_max_tokens).abs() < f64::EPSILON {
                            return;
                        }
                        draft.borrow_mut().max_tokens = v as u64;
                        *dirty.borrow_mut() = true;
                    }
                })
                .into(),
        ));
    }

    // ── Profile 切换器（在 models 区块底部展示并允许快速切换/管理） ──
    if d.loaded && !d.profiles.is_empty() {
        rows.push(section_title("预设"));
        let profile_names = d.profiles.clone();
        let pidx = profile_names
            .iter()
            .position(|n| n == &d.active_profile)
            .unwrap_or(0) as i32;
        rows.push(field_row(
            "当前预设",
            qaqh_fluent::solid_combo_box(profile_names.clone())
                .selected_index(pidx)
                .header("Profile")
                .background(ThemeRef::SolidBackground)
                .on_selection_changed({
                    let bridge = bridge.clone();
                    let profile_names = profile_names.clone();
                    let rendered_pidx = pidx;
                    move |i: i32| {
                        if i == rendered_pidx {
                            return;
                        }
                        if let Some(name) = profile_names.get(i as usize) {
                            // apply_profile 经 daemon 触发 config reload；
                            // 前端下次轮询会拿到新预设的字段。
                            bridge.spawn_apply_profile(name);
                        }
                    }
                })
                .into(),
        ));
        rows.push(
            text_block("切换预设会请求 daemon 应用并刷新配置（保存按钮不会触发切换）")
                .font_size(11.0)
                .foreground(ThemeRef::SecondaryText)
                .into(),
        );
        // ── 另存为 / 删除（active 非 default 才允许删除） ──
        let active = d.active_profile.clone();
        let can_delete = active != "default";
        rows.push(field_row(
            "",
            hstack((
                button("另存为").subtle().on_click({
                    let bridge = bridge.clone();
                    let profiles = profile_names.clone();
                    move || {
                        // 自动命名：profile_<N>，N = 现有 profile 数量（不与已存在冲突）。
                        let mut n = profiles.len();
                        let mut name = format!("profile_{n}");
                        while profiles.contains(&name) {
                            n += 1;
                            name = format!("profile_{n}");
                        }
                        bridge.spawn_save_profile(&name);
                    }
                }),
                button("删除当前预设")
                    .subtle()
                    .enabled(can_delete)
                    .on_click({
                        let bridge = bridge.clone();
                        let active = active.clone();
                        move || {
                            if active != "default" {
                                bridge.spawn_delete_profile(&active);
                            }
                        }
                    }),
            ))
            .spacing(8.0)
            .into(),
        ));
    }
}

pub(crate) fn api_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("API 密钥"));
    let key_row: Element = {
        let mut badge = Vec::new();
        if d.api_key_configured {
            badge.push(
                text_block("已配置")
                    .font_size(11.0)
                    .foreground(ThemeRef::AccentText)
                    .into(),
            );
        }
        let input = PasswordBox::new()
            .value(d.api_key.clone())
            .placeholder_text(if d.api_key_configured {
                "输入新值以替换"
            } else {
                "sk-…"
            })
            .on_password_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().api_key = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into();
        let mut els: Vec<Element> = vec![input];
        els.extend(badge);
        hstack(els).spacing(8.0).into()
    };
    rows.push(field_row("主 API Key", key_row));
    rows.push(
        text_block("留空 = 保持不变；输入新值 = 替换（密钥已加密存储，界面不回显）")
            .font_size(11.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    );
    rows.push(field_row(
        "重新运行引导",
        button("重开首次引导")
            .subtle()
            .on_click(|| oobe_view::request_rerun())
            .into(),
    ));
    rows.push(field_row(
        "子代理 API Key",
        PasswordBox::new()
            .value(d.sub_api_key.clone())
            .placeholder_text(if d.sub_api_key_configured {
                "输入新值以替换"
            } else {
                "sk-…"
            })
            .on_password_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().sub_api_key = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(
        text_block(
            "留空 = 保持不变/继承主 Key；输入新值 = 替换（密钥已加密存储，界面不回显）",
        )
        .font_size(11.0)
        .foreground(ThemeRef::SecondaryText)
        .into(),
    );
}

pub(crate) fn context_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("上下文窗口"));
    const CONTEXT_MIN: f64 = 10000.0;
    const CONTEXT_MAX: f64 = 10_000_000.0;
    // 同上：守卫对比"渲染值钳到合法区间后的结果"，防止草稿值越界（0）时被
    // WinUI 钳到 10000 并回发、误写回草稿（根因 3 的上下文重置源头）。
    let rendered_context = (d.context_limit as f64).clamp(CONTEXT_MIN, CONTEXT_MAX);
    rows.push(field_row(
        "上下文限制",
        NumberBox::new(d.context_limit as f64)
            .range(CONTEXT_MIN, CONTEXT_MAX)
            .header("")
            .on_value_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v: f64| {
                    // 防程序化同步误触发：与 permission 滑杆同理，渲染值回写时跳过
                    if (v - rendered_context).abs() < f64::EPSILON {
                        return;
                    }
                    draft.borrow_mut().context_limit = v as u64;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    let rendered_effort_idx = EFFORT_LADDER
        .iter()
        .position(|e| *e == normalize_effort(&d.reasoning_effort))
        .unwrap_or(2) as i32;
    rows.push(field_row(
        "推理强度",
        qaqh_fluent::solid_combo_box(
            EFFORT_LADDER
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .selected_index(rendered_effort_idx)
        .header("")
                .background(ThemeRef::SolidBackground)
        .on_selection_changed({
            let draft = draft.clone();
            let dirty = dirty.clone();
            move |i: i32| {
                if i == rendered_effort_idx {
                    return;
                }
                if let Some(e) = EFFORT_LADDER.get(i as usize) {
                    draft.borrow_mut().reasoning_effort = e.to_string();
                    *dirty.borrow_mut() = true;
                }
            }
        })
        .into(),
    ));
    let rendered_compact = d.auto_compact_threshold > 0.0;
    rows.push(field_row(
        "自动压缩",
        ToggleSwitch::new(rendered_compact)
            .header("")
            .on_toggled({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |on: bool| {
                    if on == rendered_compact {
                        return;
                    }
                    let mut d = draft.borrow_mut();
                    if on {
                        d.auto_compact_threshold = 0.75;
                    } else {
                        d.auto_compact_threshold = 0.0;
                    }
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    // 压缩禁用（阈值 0）时渲染 0.75：控件回发的值需与"渲染时的值"比对，
    // 否则禁用态会被静默改回启用（根因 3 同类：程序化同步写回）。
    let threshold = if d.auto_compact_threshold > 0.0 {
        d.auto_compact_threshold.clamp(0.3, 0.95)
    } else {
        0.75
    };
    rows.push(field_row(
        "压缩阈值",
        Slider::new(threshold)
            .range(0.3, 0.95)
            .step(0.05)
            .header("")
            .on_value_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                let rendered_threshold = threshold;
                move |v: f64| {
                    if (v - rendered_threshold).abs() < f64::EPSILON {
                        return;
                    }
                    draft.borrow_mut().auto_compact_threshold = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    let rendered_compliance = d.compliance_enabled;
    rows.push(field_row(
        "合规模式",
        ToggleSwitch::new(rendered_compliance)
            .header("")
            .on_toggled({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |on: bool| {
                    if on == rendered_compliance {
                        return;
                    }
                    draft.borrow_mut().compliance_enabled = on;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
}

pub(crate) fn subagent_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("子代理"));
    rows.push(field_row(
        "子代理模型",
        text_box(d.sub_model.clone())
            .placeholder_text("留空 = 继承主模型")
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().sub_model = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(field_row(
        "子代理 Base URL",
        text_box(d.sub_base_url.clone())
            .placeholder_text("留空 = 继承主配置")
            .on_text_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v| {
                    draft.borrow_mut().sub_base_url = v;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    let rendered_sub_max = (d.sub_max_tokens as f64).clamp(16.0, 1_000_000.0);
    rows.push(field_row(
        "最大 Tokens",
        NumberBox::new(d.sub_max_tokens as f64)
            .range(16.0, 1_000_000.0)
            .header("")
            .on_value_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v: f64| {
                    if (v - rendered_sub_max).abs() < f64::EPSILON {
                        return;
                    }
                    draft.borrow_mut().sub_max_tokens = v as u64;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    let rendered_sub_timeout = (d.sub_timeout_secs as f64).clamp(10.0, 3600.0);
    rows.push(field_row(
        "超时（秒）",
        NumberBox::new(d.sub_timeout_secs as f64)
            .range(10.0, 3600.0)
            .header("")
            .on_value_changed({
                let draft = draft.clone();
                let dirty = dirty.clone();
                move |v: f64| {
                    if (v - rendered_sub_timeout).abs() < f64::EPSILON {
                        return;
                    }
                    draft.borrow_mut().sub_timeout_secs = v as u64;
                    *dirty.borrow_mut() = true;
                }
            })
            .into(),
    ));
    rows.push(section_title("默认工具"));
    if d.tools.is_empty() {
        rows.push(
            text_block("（暂无可用工具）")
                .foreground(ThemeRef::SecondaryText)
                .into(),
        );
    } else {
        let tools = d.tools.clone();
        let selected = d.sub_tools.clone();
        for t in &tools {
            let checked = selected.contains(t);
            let rendered_checked = checked;
            rows.push(
                check_box(checked)
                    .content(t.clone())
                    .on_checked({
                        let draft = draft.clone();
                        let dirty = dirty.clone();
                        let t = t.clone();
                        move |on: bool| {
                            if on == rendered_checked {
                                return;
                            }
                            let mut d = draft.borrow_mut();
                            if on {
                                if !d.sub_tools.contains(&t) {
                                    d.sub_tools.push(t.clone());
                                }
                            } else {
                                d.sub_tools.retain(|x| x != &t);
                            }
                            *dirty.borrow_mut() = true;
                        }
                    })
                    .into(),
            );
        }
    }
}

pub(crate) fn workspace_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let bridge = ctx.bridge.clone();
    let draft = ctx.draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    rows.push(section_title("工具套件运行环境"));
    let rendered_ws_idx = WORKSPACE_MODES
        .iter()
        .position(|m| *m == d.workspace_mode)
        .unwrap_or(0) as i32;
    rows.push(field_row(
        "运行模式",
        qaqh_fluent::solid_combo_box(
            WORKSPACE_MODES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .selected_index(rendered_ws_idx)
        .header("")
                .background(ThemeRef::SolidBackground)
        .on_selection_changed({
            let draft = draft.clone();
            let dirty = dirty.clone();
            move |i: i32| {
                if i == rendered_ws_idx {
                    return;
                }
                if let Some(m) = WORKSPACE_MODES.get(i as usize) {
                    draft.borrow_mut().workspace_mode = m.to_string();
                    *dirty.borrow_mut() = true;
                }
            }
        })
        .into(),
    ));
    let status_text = if d.workspace_active_mode.is_empty() {
        "（未查询到运行状态）".to_string()
    } else {
        format!(
            "已配置 {} · 当前 {} · {}",
            d.workspace_configured_mode, d.workspace_active_mode, d.workspace_endpoint
        )
    };
    rows.push(
        text_block(status_text)
            .font_size(12.0)
            .foreground(ThemeRef::SecondaryText)
            .into(),
    );
    rows.push(
        text_block("⚠ 切换模式需重启后端（backend.restart 尚未迁移，保存后下次启动生效）")
            .font_size(12.0)
            .foreground(ThemeRef::SystemAttention)
            .into(),
    );
    rows.push(field_row(
        "",
        hstack((
            button("应用模式").subtle().on_click({
                let bridge = bridge.clone();
                let draft = draft.clone();
                let dirty = dirty.clone();
                move || {
                    let mode = draft.borrow().workspace_mode.clone();
                    bridge.spawn_workspace_set_mode(&mode);
                    bridge.spawn_workspace_status();
                    *dirty.borrow_mut() = false;
                }
            }),
            button("刷新状态").subtle().on_click({
                let bridge = bridge.clone();
                move || bridge.spawn_workspace_status()
            }),
            button("WSL 诊断").subtle().on_click({
                let bridge = bridge.clone();
                move || bridge.spawn_workspace_diagnose()
            }),
            button("安装 WSL").subtle().on_click({
                let bridge = bridge.clone();
                move || bridge.spawn_workspace_install_wsl()
            }),
        ))
        .spacing(8.0)
        .into(),
    ));
}

pub(crate) fn appearance_section(ctx: &SettingsCtx, rows: &mut Vec<Element>) {
    let bridge = ctx.bridge.clone();
    let draft = ctx.draft.clone();
    let proj_draft = ctx.proj_draft.clone();
    let dirty = ctx.dirty.clone();
    let d = ctx.d.clone();
    let pd = ctx.pd.clone();
    rows.push(section_title("界面"));
    let rendered_theme_idx = match pd.theme.as_str() {
        "light" => 1,
        "dark" => 2,
        "dark-gray" => 3,
        _ => 0,
    };
    rows.push(field_row(
        "主题",
        qaqh_fluent::solid_combo_box(vec![
            "system".to_string(),
            "light".to_string(),
            "dark".to_string(),
            "dark-gray".to_string(),
        ])
        .selected_index(rendered_theme_idx)
        .header("")
                .background(ThemeRef::SolidBackground)
        .on_selection_changed({
            let proj_draft = proj_draft.clone();
            let dirty = dirty.clone();
            move |i: i32| {
                if i == rendered_theme_idx {
                    return;
                }
                let mode = match i {
                    1 => "light",
                    2 => "dark",
                    3 => "dark-gray",
                    _ => "system",
                };
                proj_draft.borrow_mut().theme = mode.to_string();
                *dirty.borrow_mut() = true;
                // WebView 移除：主题壳本地立即应用（三态映射同
                // handle_message shell.setTheme 逻辑）。
                let theme = match mode {
                    "light" => windows_reactor::RequestedTheme::Light,
                    "dark" | "dark-gray" => windows_reactor::RequestedTheme::Dark,
                    _ => windows_reactor::RequestedTheme::Default,
                };
                windows_reactor::set_requested_theme(theme);
            }
        })
        .into(),
    ));
    let rendered_lang_idx = if pd.lang == "en" { 1 } else { 0 };
    rows.push(field_row(
        "语言",
        qaqh_fluent::solid_combo_box(vec!["中文".to_string(), "English".to_string()])
            .selected_index(rendered_lang_idx)
            .header("")
                .background(ThemeRef::SolidBackground)
            .on_selection_changed({
                let proj_draft = proj_draft.clone();
                let dirty = dirty.clone();
                move |i: i32| {
                    if i == rendered_lang_idx {
                        return;
                    }
                    let lang = if i == 1 { "en" } else { "zh" };
                    proj_draft.borrow_mut().lang = lang.to_string();
                    *dirty.borrow_mut() = true;
                    // WebView 移除：语言随保存按钮统一 config.save。
                }
            })
            .into(),
    ));
    // ── 字体：首项是随应用分发的默认字体；第二项显式选择 Windows
    // UI 字体，之后才是注册表枚举出的系统字体。显示名和值分离，避免
    // 把“内置默认”文案误写入 config。──
    let font_options: Vec<(String, String)> = {
        let mut v = vec![
            ("内置默认（HarmonyOS Sans SC）".to_string(), String::new()),
            (
                "Windows 系统界面字体".to_string(),
                fonts::WINDOWS_UI_FONT_FAMILY.to_string(),
            ),
        ];
        v.extend(
            fonts::system_fonts_cached()
                .iter()
                .filter(|f| !f.starts_with("Segoe UI") && !f.starts_with("Microsoft YaHei"))
                .cloned()
                .map(|f| (f.clone(), f)),
        );
        v
    };
    let font_idx = font_options
        .iter()
        .position(|(_, value)| *value == d.font_family)
        .unwrap_or(0) as i32;
    rows.push(field_row(
        "字体",
        qaqh_fluent::solid_combo_box(
            font_options
                .iter()
                .map(|(label, _)| label.clone())
                .collect::<Vec<_>>(),
        )
        .selected_index(font_idx)
        .header("")
                .background(ThemeRef::SolidBackground)
        .on_selection_changed({
            let draft = draft.clone();
            let dirty = dirty.clone();
            let font_options = font_options.clone();
            let rendered_font_idx = font_idx;
            move |i: i32| {
                if i == rendered_font_idx {
                    return;
                }
                let font = font_options
                    .get(i as usize)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                draft.borrow_mut().font_family = font.clone();
                *dirty.borrow_mut() = true;
                // 空值 = 应用内置默认；旧配置无需迁移即可获得新字体。
                windows_reactor::set_font_family(Some(fonts::effective_ui_font(&font)));
            }
        })
        .into(),
    ));
    rows.push(
        text_block("QAQ-Harness 内置并使用 HarmonyOS Sans SC；代码与数字使用 Cascadia Mono。字体均未修改，完整许可随应用分发。")
            .font_size(tokens::TYPE_CAPTION)
            .foreground(ThemeRef::SecondaryText)
            .wrap()
            .into(),
    );
    if let Some(notices) = fonts::notices_path() {
        rows.push(
            button("查看字体许可")
                .subtle()
                .tooltip("打开随应用分发的第三方字体许可")
                .automation_name("查看字体许可")
                .on_click({
                    let bridge = bridge.clone();
                    move || {
                        let _ = bridge.open_path(&notices);
                    }
                })
                .into(),
        );
    }
}
