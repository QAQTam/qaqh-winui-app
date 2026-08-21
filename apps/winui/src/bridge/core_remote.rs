//! BridgeCore methods: remote.

use std::sync::atomic::Ordering;

use qaqh_client::{Client, QueryRequest};
use serde_json::Value;

use super::*;

impl super::BridgeCore {
    /// 清除远端档案，切回本地 daemon（本地 launch 逻辑由 connect_client 恢复）。
    pub(crate) fn clear_remote_profile(&self) {
        remove_remote_profile_file();
        *self
            .remote_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.switch_daemon();
    }

    /// 显示用远端路径：`//ip/...`（无远端档案时原样返回 daemon 路径）。
    /// 标题栏工作区路径展示已接入；远端文件选择器就绪后可替换/扩展。
    pub(crate) fn display_remote_path(&self, daemon_path: &str) -> String {
        match self.remote_profile_snapshot() {
            Some(profile) => {
                qaqh_client::display_path(qaqh_client::display_host(&profile.base_url), daemon_path)
            }
            None => daemon_path.to_string(),
        }
    }

    /// 显示形式/用户输入 → daemon 侧路径（纯文本转换，无 I/O）。
    /// 供后续远端文件选择器接入（当前暂未消费）。
    #[allow(dead_code)]
    pub(crate) fn daemon_path_from_display(&self, input: &str) -> Option<String> {
        qaqh_client::remote_path_from_display(input)
    }

    // ── 远端文件选择器数据流（fs.list / fs.read）──────────────────

    /// (listing, rev) 快照：picker 轮询比对 rev 刷新。
    pub(crate) fn fs_listing_snapshot(&self) -> (RemoteFsListing, u64) {
        let listing = self
            .remote_fs_listing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.remote_fs_rev.load(Ordering::Relaxed);
        (listing, rev)
    }

    /// (preview, rev) 快照：picker 轮询比对 rev 刷新。
    pub(crate) fn fs_preview_snapshot(&self) -> (Option<RemoteFsPreview>, u64) {
        let preview = self
            .remote_fs_preview
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.remote_fs_preview_rev.load(Ordering::Relaxed);
        (preview, rev)
    }

    /// 拉取 daemon 侧目录列表（`fs.list`）。加载态先行，结果/错误后写。
    pub(crate) fn spawn_fs_list(&self, path: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            {
                let mut listing = core
                    .remote_fs_listing
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                listing.path = path.clone();
                listing.entries.clear();
                listing.loading = true;
                listing.error = None;
                core.remote_fs_rev.fetch_add(1, Ordering::Relaxed);
            }
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(error) => {
                    let mut listing = core
                        .remote_fs_listing
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    listing.loading = false;
                    listing.error = Some(format!("连接失败：{error}"));
                    core.remote_fs_rev.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            match client
                .query(QueryRequest::FsList { path: path.clone() })
                .await
            {
                Ok(value) => {
                    let entries = value
                        .as_array()
                        .map(|array| array.iter().filter_map(parse_remote_fs_entry).collect())
                        .unwrap_or_default();
                    let mut listing = core
                        .remote_fs_listing
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    listing.path = path;
                    listing.entries = entries;
                    listing.loading = false;
                    listing.error = None;
                    core.remote_fs_rev.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    let mut listing = core
                        .remote_fs_listing
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    listing.loading = false;
                    listing.error = Some(format!("fs.list 失败：{error}"));
                    core.remote_fs_rev.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    /// 拉取 daemon 侧文件文本预览（`fs.read`，最多 64 KiB）。
    pub(crate) fn spawn_fs_read(&self, path: String) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let Ok(client) = core.ensure_client().await else {
                return;
            };
            match client
                .query(QueryRequest::FsRead {
                    path: path.clone(),
                    max_bytes: Some(64 * 1024),
                })
                .await
            {
                Ok(value) => {
                    let preview = RemoteFsPreview {
                        path: path.clone(),
                        content: value
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        truncated: value
                            .get("truncated")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    };
                    *core
                        .remote_fs_preview
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(preview);
                    core.remote_fs_preview_rev.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    log_diag(&format!("remote fs.read {path} failed: {error}"));
                }
            }
        });
    }
    /// 切换 daemon：停旧 client、清空会话态、回首页、重建连接。
    pub(crate) fn switch_daemon(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            if let Some(client) = core.client.lock().unwrap_or_else(|e| e.into_inner()).take() {
                client.close();
                log_diag("remote: closed previous client");
            }
            core.attached
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            *core.active_seed.lock().unwrap_or_else(|e| e.into_inner()) = String::new();
            *core
                .last_timeline_seed
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = String::new();
            core.compact_statuses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            core.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            core.session_rev.fetch_add(1, Ordering::Relaxed);
            core.workspaces
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            core.workspace_rev.fetch_add(1, Ordering::Relaxed);
            core.navigate("home", None);
            core.rebuilding.store(true, Ordering::Relaxed);
            match core.connect_client().await {
                Ok(_) => {
                    core.rebuild_failures.store(0, Ordering::Relaxed);
                    log_diag("remote: daemon switched");
                }
                Err(error) => {
                    core.rebuild_failures.fetch_add(1, Ordering::Relaxed);
                    log_diag(&format!("remote: switch failed: {error}"));
                }
            }
            core.rebuilding.store(false, Ordering::Relaxed);
            core.reset_stall_timers();
            core.spawn_refresh_sessions();
        });
    }

    /// 外部入口：重建进行中时拒绝（防双 client 竞态），否则委托内部实现。    /// 外部入口：重建进行中时拒绝（防双 client 竞态），否则委托内部实现。
    pub(crate) async fn ensure_client(&self) -> Result<Client, String> {
        // A 方案：重建进行中时拒绝新连接（rebuild_client 内部持锁协调），
        // 避免双 client 竞态（两个 connect 各建一套 SSE 流）。
        if self.rebuilding.load(Ordering::Relaxed) {
            return Err("client is rebuilding after daemon stall".into());
        }
        self.connect_client().await
    }
}
