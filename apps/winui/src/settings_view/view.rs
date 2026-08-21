use std::sync::Arc;

use qaqh_fluent::{motion, tokens};
use serde_json::json;
use windows_reactor::*;

use crate::bridge::{Bridge, SettingsProjection};
use crate::shell_store::{SettingsSnapshot, normalize_effort};

use super::sections::{
    advanced_section, api_section, appearance_section, context_section, models_section,
    multimodal_section, remote_section, subagent_section, workspace_section,
};
use super::*;

pub fn settings_view(cx: &mut RenderCx, bridge: Arc<Bridge>) -> Element {
    let (_snapshot, set_snapshot) = cx.use_state::<Option<SettingsSnapshot>>(None);
    let (_proj, set_proj) = cx.use_state::<SettingsProjection>(SettingsProjection::default());
    let (category, set_category) = cx.use_state::<String>("models".to_string());
    let (pane_open, set_pane_open) = cx.use_state::<bool>(true);
    let (saved_at, set_saved_at) = cx.use_state::<Option<std::time::Instant>>(None);
    let (save_error, set_save_error) = cx.use_state::<Option<String>>(None);
    // 诊断区块：模式切换/导出后自增驱动状态行重渲染。
    let (diag_rev, set_diag_rev) = cx.use_state::<u32>(0);
    // 权限滑杆当前档位显示（0 = 尚未滑动，渲染期回退到 config 值）；
    // on_value_changed 里 set 它驱动档位说明文字重渲染。
    let (perm_desc, set_perm_desc) = cx.use_state::<u8>(0);
    let (export_path, set_export_path) = cx.use_state::<Option<String>>(None);
    // 远端 daemon 档案草稿（设置页「远端连接」分类）。
    let (remote_url, set_remote_url) = cx.use_state::<String>(
        bridge
            .core()
            .remote_profile_snapshot()
            .map(|p| p.base_url)
            .unwrap_or_default(),
    );
    let (remote_token, set_remote_token) = cx.use_state::<String>(String::new());
    let (remote_status, set_remote_status) = cx.use_state::<String>(String::new());
    let timer = cx.use_ref::<Option<DispatcherTimer>>(None);
    let last_rev = cx.use_ref::<u64>(0);
    let last_proj_rev = cx.use_ref::<u64>(0);
    // 未保存修改标记（rev 刷新草稿的闸门）。
    let dirty = cx.use_ref::<bool>(false);

    // ── 草稿字段（渲染期读；rev 变化且 !dirty 时整体刷新）────────────
    let draft = cx.use_ref::<SettingsSnapshot>(SettingsSnapshot::default());
    let proj_draft = cx.use_ref::<SettingsProjection>(SettingsProjection::default());
    // theme 本地即时应用 + 随保存统一 config.save 持久化（2026-08 后端新增
    // `theme` 契约字段，收敛 BUG-001 残留）；permission 变更即发；
    // lang/fontFamily 随保存统一 config.save 提交。

    // ── 轮询：rev 比对刷新快照 + 草稿 ──────────────────────────────
    cx.use_effect((), {
        let bridge = bridge.clone();
        let set_snapshot = set_snapshot.clone();
        let set_proj = set_proj.clone();
        let timer = timer.clone();
        let last_rev = last_rev.clone();
        let last_proj_rev = last_proj_rev.clone();
        let draft = draft.clone();
        let proj_draft = proj_draft.clone();
        let dirty = dirty.clone();
        move || {
            let core = bridge.core();
            // 首次进入：先以当前快照/投影初始化草稿——组件在切走时会被卸载，
            // 再次进入时草稿是全默认值（空/0），且 spawn_config_load(false) 在
            // 缓存命中时直接跳过，轮询又因 rev 未变化永不刷新 → 页面显示全默认
            // 值（根因 0）。这里先灌入缓存快照兜住首帧显示，再 force 重拉权威
            // 快照（保存后缓存是陈旧的），轮询随后用最新值刷新。
            if let Some(snap) = core.settings_snapshot().0 {
                if !*dirty.borrow() {
                    *draft.borrow_mut() = snap.clone();
                }
            }
            let (proj, _) = core.settings_projection();
            *proj_draft.borrow_mut() = proj;
            bridge.spawn_config_load(true);
            let (_, rev) = core.settings_snapshot();
            *last_rev.borrow_mut() = rev;
            let (_, prev) = core.settings_projection();
            *last_proj_rev.borrow_mut() = prev;
            if let Ok(t) = DispatcherTimer::new(POLL_INTERVAL, {
                let core = core.clone();
                let set_snapshot = set_snapshot.clone();
                let set_proj = set_proj.clone();
                let last_rev = last_rev.clone();
                let last_proj_rev = last_proj_rev.clone();
                let draft = draft.clone();
                let proj_draft = proj_draft.clone();
                let dirty = dirty.clone();
                move || {
                    // settings 投影（config.load 结果）rev 变化且无未保存修改 → 刷新草稿。
                    let (snap, srev) = core.settings_snapshot();
                    if srev != *last_rev.borrow() {
                        *last_rev.borrow_mut() = srev;
                        if let Some(snap) = &snap {
                            if !*dirty.borrow() {
                                *draft.borrow_mut() = snap.clone();
                            }
                        }
                        set_snapshot.call(snap);
                    }
                    // 设置投影（theme/lang/permission）rev 变化 → 刷新。
                    let (p, prev) = core.settings_projection();
                    if prev != *last_proj_rev.borrow() {
                        *last_proj_rev.borrow_mut() = prev;
                        if !*dirty.borrow() {
                            *proj_draft.borrow_mut() = p.clone();
                        }
                        set_proj.call(p);
                    }
                }
            }) {
                *timer.borrow_mut() = Some(t);
            }
        }
    });

    let d = draft.borrow().clone();
    let pd = proj_draft.borrow().clone();

    // ── 保存：config.save 全字段（camelCase，对齐 Web save()）────────
    let on_save = {
        let bridge = bridge.clone();
        let draft = draft.clone();
        let proj_draft = proj_draft.clone();
        let dirty = dirty.clone();
        let set_saved_at = set_saved_at.clone();
        let set_save_error = set_save_error.clone();
        move || {
            let d = draft.borrow().clone();
            let pd = proj_draft.borrow().clone();
            // theme：`system` 映射为空串 → daemon 存 None（跟随系统），
            // 与后端 `config.save` 的 theme 契约一致（2026-08 新增字段，
            // 主题持久化收敛 BUG-001 残留）。
            let theme = if pd.theme.is_empty() || pd.theme == "system" {
                String::new()
            } else {
                pd.theme.clone()
            };
            // apiKey：空 = 保持不变（与 Web save() 一致：仅非空且非 "****" 时发送），
            // 避免未编辑 apiKey 时因 draft 为 ""（掩码占位）而误触发后端“空串删除”语义
            // 导致任何保存都清空密钥（Bug 根因 1）。
            let mut fields = json!({
                "model": d.model,
                "baseUrl": d.base_url,
                "providerId": d.provider_id,
                "endpoint": d.endpoint,
                "maxTokens": d.max_tokens,
                "contextLimit": d.context_limit,
                "reasoningEffort": normalize_effort(&d.reasoning_effort).to_string(),
                "autoCompactThreshold": if d.auto_compact_threshold > 0.0 { d.auto_compact_threshold } else { 0.0 },
                "complianceEnabled": d.compliance_enabled,
                "lang": pd.lang,
                "fontFamily": d.font_family,
                "theme": theme,
                "subagentModel": d.sub_model,
                "subagentBaseUrl": d.sub_base_url,
                "subagentMaxTokens": d.sub_max_tokens,
                "subagentTimeoutSecs": d.sub_timeout_secs,
                "subagentDefaultTools": d.sub_tools,
                "tokenizerPath": d.tokenizer_path,
                "multimodalProviderType": d.mm_provider_type,
                "multimodalEnabled": d.mm_enabled,
                "multimodalBaseUrl": d.mm_base_url,
                "multimodalModel": d.mm_model,
                "multimodalMaxTokens": d.mm_max_tokens,
            });
            // 主密钥：仅当用户显式输入新值时发送（非空且非掩码），否则省略以保持现值
            if !d.api_key.is_empty() && d.api_key != "****" {
                fields["apiKey"] = json!(d.api_key);
            }
            // 子代理/多模态密钥同理：空 = 保持（后端 update_string 忽略空串，但显式省略更清晰）
            if !d.sub_api_key.is_empty() && d.sub_api_key != "****" {
                fields["subagentApiKey"] = json!(d.sub_api_key);
            }
            if !d.mm_api_key.is_empty() && d.mm_api_key != "****" {
                fields["multimodalApiKey"] = json!(d.mm_api_key);
            }
            *dirty.borrow_mut() = false;
            bridge.spawn_config_save(fields);
            set_saved_at.call(Some(std::time::Instant::now()));
            set_save_error.call(None);
        }
    };

    // ── 字段 setter（写草稿 + 置 dirty）─────────────────────────────
    // 每个闭包捕获 bridge/draft/dirty；通用 helper 不便（借用冲突），逐字段生成。

    // ── 左侧分类导航（原生 NavigationView 菜单项）──────────────────
    // 选中条、hover/pressed、键盘焦点与系统动效由 WinUI 控件模板负责。
    // 内容页仍在下方使用稳定 keyed host，避免早期手写 Border↔TextBlock
    // 类型跳变造成的 reconciler 控件树复用错误。
    let nav_items: Vec<NavViewItem> = CATEGORIES
        .iter()
        .map(|(id, label, symbol)| {
            NavViewItem::new(*label)
                .tag(*id)
                .icon(Icon::symbol(*symbol))
        })
        .collect();

    // ── 右侧表单区（按分类）────────────────────────────────────────
    let mut rows: Vec<Element> = Vec::new();

    // models：provider / endpoint / baseUrl / model

    let ctx = SettingsCtx {
        bridge: bridge.clone(),
        draft: draft.clone(),
        proj_draft: proj_draft.clone(),
        dirty: dirty.clone(),
        d: d.clone(),
        pd: pd.clone(),
        set_diag_rev: set_diag_rev.clone(),
        set_perm_desc: set_perm_desc.clone(),
        set_export_path: set_export_path.clone(),
        diag_rev,
        perm_desc,
        export_path: export_path.clone(),
        remote_url: remote_url.clone(),
        remote_token: remote_token.clone(),
        remote_status: remote_status.clone(),
        set_remote_url: set_remote_url.clone(),
        set_remote_token: set_remote_token.clone(),
        set_remote_status: set_remote_status.clone(),
    };

    if category == "models" {
        models_section(&ctx, &mut rows);
    }
    if category == "api" {
        api_section(&ctx, &mut rows);
    }
    if category == "context" {
        context_section(&ctx, &mut rows);
    }
    if category == "subagent" {
        subagent_section(&ctx, &mut rows);
    }
    if category == "workspace" {
        workspace_section(&ctx, &mut rows);
    }
    if category == "appearance" {
        appearance_section(&ctx, &mut rows);
    }
    if category == "multimodal" {
        multimodal_section(&ctx, &mut rows);
    }
    if category == "advanced" {
        advanced_section(&ctx, &mut rows);
    }
    if category == "remote" {
        remote_section(&ctx, &mut rows);
    }
    // ── 底部：保存按钮 + 状态 ───────────────────────────────────────    // ── 底部：保存按钮 + 状态 ───────────────────────────────────────
    let footer: Element = {
        let saved_text: Element = match saved_at {
            Some(t) if t.elapsed() < Duration::from_secs(3) => text_block("已保存 ✓")
                .font_size(12.0)
                .foreground(ThemeRef::SystemSuccess)
                .into(),
            _ => text_block("").into(),
        };
        let error_text: Element = match save_error.clone() {
            Some(e) => text_block(e)
                .font_size(12.0)
                .foreground(ThemeRef::SystemCritical)
                .into(),
            None => text_block("").into(),
        };
        hstack((
            button("保存设置").accent().on_click({
                let on_save = on_save.clone();
                move || on_save()
            }),
            saved_text,
            error_text,
        ))
        .spacing(12.0)
        .into()
    };

    // ── 表单区（rows 每行带 key：`{category}-{idx}`）────────────────
    // keyed reconcile：跨分类 key 全不同 → 切换分类时整行干净重建（杜绝
    // 同 index 类型跳变（grid↔TextBlock）导致的控件复用错位）；同分类内
    // 重渲染 key 相同 → 原地更新（表单输入状态保持）。
    // 跨分类 key 全不同；动画挂在整页而非每一行，避免密集表单产生瀑布闪烁。
    let rows: Vec<Element> = rows
        .into_iter()
        .enumerate()
        .map(|(i, el)| el.with_key(format!("{category}-{i}")))
        .collect();
    let form: Element = vstack(rows).spacing(tokens::SPACE_2).into();
    let body: Element = vstack((form, footer)).spacing(tokens::SPACE_4).into();
    let page: Element = grid((scroll_viewer(body),))
        .with_key(format!("settings-page-{category}"))
        .rows([GridLength::STAR])
        .columns([GridLength::STAR])
        .padding(Thickness::xy(tokens::SPACE_6, tokens::SPACE_6))
        .transition(motion::navigation_enter(), motion::content_exit())
        .into();
    let content_host: Element = grid((page,))
        .rows([GridLength::STAR])
        .columns([GridLength::STAR])
        .into();

    // ── 根：固定展开的标准 NavigationView ─────────────────────────
    // `Left` 明确禁止 Auto 模式因窗口宽度自行折叠；这正是最初弃用标准
    // 控件时漏掉的配置。NavigationView 的 content host 保持同构，只有带
    // category key 的页面子树替换，因此既有稳定性修复仍然成立。
    NavigationView::new(nav_items, content_host)
        .selected_tag(category.clone())
        .on_selection_changed(set_category)
        .pane_display_mode(NavigationViewPaneDisplayMode::Left)
        .pane_open(pane_open)
        .on_pane_open_changed(set_pane_open)
        .pane_toggle_button_visible(true)
        .back_button_visible(false)
        .settings_visible(false)
        .open_pane_length(280.0)
        .into()
}
