//! BridgeCore methods: sessions.

use std::sync::atomic::Ordering;

use qaqh_client::{ActionRequest, QueryRequest};

use crate::shell_store::parse_workspaces;

use super::*;

impl super::BridgeCore {
    /// 当前选中 workspace id（None = 未分组视图）。
    pub(crate) fn current_workspace(&self) -> Option<String> {
        self.current_workspace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 设置当前选中 workspace（sidebar 点击 / tabs 新建归属）。
    /// 合并后顶部显示亦派生于此，需触发 header 刷新保证两处同显。
    pub(crate) fn set_current_workspace(&self, id: Option<String>) {
        *self
            .current_workspace
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = id;
        self.refresh_header();
    }

    /// 当前选中 workspace 的 path（新建会话 cwd 归属用；未选中返回 None）。
    pub(crate) fn current_workspace_path(&self) -> Option<String> {
        let id = self.current_workspace()?;
        self.workspaces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.path.clone())
    }

    /// 后台刷新 `workspace.list` → 投影进缓存 → rev++。
    pub(crate) fn spawn_refresh_workspaces(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            core.refresh_workspaces_inner().await;
        });
    }

    pub(crate) async fn refresh_workspaces_inner(&self) {
        let client = match self.ensure_client().await {
            Ok(client) => client,
            Err(err) => {
                log_diag(&format!("refresh_workspaces: connect failed: {err}"));
                return;
            }
        };
        let v = match client.query(QueryRequest::WorkspaceList).await {
            Ok(v) => v,
            Err(err) => {
                log_diag(&format!("refresh_workspaces: workspace.list failed: {err}"));
                return;
            }
        };
        let items = parse_workspaces(&v);
        *self.workspaces.lock().unwrap_or_else(|e| e.into_inner()) = items;
        self.workspace_rev.fetch_add(1, Ordering::Relaxed);
    }

    /// 注册目录为 workspace（`workspace.create`）→ 刷新列表并自动选中。
    /// 合并方案：左侧为唯一入口，创建后立即 `set_current_workspace`，
    /// 后续 `spawn_new_session` 的 `cwd=current_workspace_path` 自然归属，
    /// 解决“顶部与左侧不通、左侧恒显未分组”。
    pub(crate) fn spawn_workspace_create(&self, path: String) {
        let core = self.self_arc();
        let path_for_select = path.clone();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace_create: connect failed: {err}"));
                    return;
                }
            };
            let created_id: Option<String> =
                match client.action(ActionRequest::WorkspaceCreate { path }).await {
                    Ok(v) => {
                        log_diag("workspace_create: ok");
                        v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string())
                    }
                    Err(err) => {
                        log_diag(&format!("workspace_create failed: {err}"));
                        None
                    }
                };
            core.refresh_workspaces_inner().await;
            core.refresh_sessions_inner().await;
            // 自动选中：去重场景后端直接返回已存在项的 id，前端亦选中
            if let Some(id) = created_id {
                core.set_current_workspace(Some(id));
            } else {
                // 失败或未返回 id 时，按路径匹配已刷新列表兜底选中
                let norm = path_for_select.replace('/', "\\").to_ascii_lowercase();
                let norm = norm.trim_end_matches('\\').to_string();
                let found = core
                    .workspaces
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find(|w| {
                        w.path
                            .replace('/', "\\")
                            .to_ascii_lowercase()
                            .trim_end_matches('\\')
                            == norm
                    })
                    .map(|w| w.id.clone());
                if let Some(id) = found {
                    core.set_current_workspace(Some(id));
                }
            }
            // 头部工作区显示由 refresh_header 派生，需递增 rev 触发轮询
            core.refresh_header();
        });
    }

    /// 重命名 workspace（`workspace.rename`）→ 刷新列表。
    pub(crate) fn spawn_workspace_rename(&self, id: String, title: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace_rename: connect failed: {err}"));
                    return;
                }
            };
            match client
                .action(ActionRequest::WorkspaceRename { id, title })
                .await
            {
                Ok(_) => log_diag("workspace_rename: ok"),
                Err(err) => log_diag(&format!("workspace_rename failed: {err}")),
            }
            core.refresh_workspaces_inner().await;
        });
    }

    /// 删除 workspace 注册（`workspace.delete`，不删会话）→ 刷新列表。
    /// 若删除的是当前选中项，回落未分组视图。
    pub(crate) fn spawn_workspace_delete(&self, id: String) {
        let core = self.self_arc();
        let was_current = core.current_workspace().as_deref() == Some(id.as_str());
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace_delete: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::WorkspaceDelete { id }).await {
                Ok(_) => log_diag("workspace_delete: ok"),
                Err(err) => log_diag(&format!("workspace_delete failed: {err}")),
            }
            if was_current {
                core.set_current_workspace(None);
            }
            core.refresh_workspaces_inner().await;
            core.refresh_sessions_inner().await;
        });
    }

    /// 把会话移入指定 workspace（菜单移动，`workspace.move_session`）。
    pub(crate) fn spawn_workspace_move_session(&self, seed: String, workspace_id: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace_move: connect failed: {err}"));
                    return;
                }
            };
            match client
                .action(ActionRequest::WorkspaceMoveSession { seed, workspace_id })
                .await
            {
                Ok(_) => log_diag("workspace_move: ok"),
                Err(err) => log_diag(&format!("workspace_move failed: {err}")),
            }
            core.refresh_workspaces_inner().await;
            core.refresh_sessions_inner().await;
        });
    }

    /// 把会话移出 workspace → 未分组（`workspace.detach`）。
    pub(crate) fn spawn_workspace_detach(&self, seed: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("workspace_detach: connect failed: {err}"));
                    return;
                }
            };
            match client.action(ActionRequest::WorkspaceDetach { seed }).await {
                Ok(_) => log_diag("workspace_detach: ok"),
                Err(err) => log_diag(&format!("workspace_detach failed: {err}")),
            }
            core.refresh_workspaces_inner().await;
            core.refresh_sessions_inner().await;
        });
    }

    // ── 桌面通知（Phase 1：TurnCompleted 预览 + 点击回前台）───────────

    /// 通知开关当前值（启动时从 ui-preferences.json 载入；默认开启）。
    pub(crate) fn notif_enabled(&self) -> bool {
        self.notif_enabled.load(Ordering::Relaxed)
    }

    /// 设置通知开关：写后端 `config.save`（`notificationsEnabled`，单一权威源）、
    /// 本地偏好镜像与内存更新；开启时惰性初始化通知器。
    ///
    /// 2026-08 后端新增 `notifications_enabled` 契约字段；此前纯本地偏好文件
    /// 无法跨设备迁移，现收敛到后端 config。
    pub(crate) fn spawn_set_notif_pref(&self, enabled: bool) {
        self.notif_enabled.store(enabled, Ordering::Relaxed);
        write_notif_pref(enabled);
        if enabled {
            self.ensure_notifier();
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("set_notif_pref: connect failed: {err}"));
                    return;
                }
            };
            let fields = serde_json::json!({ "notificationsEnabled": enabled });
            match client.action(ActionRequest::ConfigSave { fields }).await {
                Ok(_) => log_diag(&format!("set_notif_pref {enabled}: config saved")),
                Err(err) => log_diag(&format!("set_notif_pref {enabled}: failed: {err}")),
            }
        });
    }
}
