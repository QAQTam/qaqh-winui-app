//! BridgeCore methods: notifications.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use qaqh_app_notifications::{InitOutcome, Notifier};

use crate::shell_store::SessionDetail;

use super::*;

impl super::BridgeCore {
    /// 启动初始化：读偏好 → 开启时初始化通知器（幂等）。
    pub(crate) fn init_notifications(&self) {
        let enabled = std::fs::read_to_string(notif_prefs_path())
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("notificationsEnabled").and_then(|x| x.as_bool()))
            .unwrap_or(true); // 默认开启（对齐旧后端 after-turn 通知行为）
        self.notif_enabled.store(enabled, Ordering::Relaxed);
        if enabled {
            self.ensure_notifier();
        }
    }

    /// 惰性创建通知器（幂等）。直连失败自动走 Bootstrap 重试
    /// （见 qaqh_app_notifications::Notifier::init）。
    pub(crate) fn ensure_notifier(&self) {
        let mut guard = self.notifier.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        *guard = match Notifier::init(|_arg| activate_main_window()) {
            InitOutcome::Supported(n) => {
                log_diag("desktop notifications: initialized");
                Some(Arc::new(n))
            }
            InitOutcome::Unsupported => {
                log_diag("desktop notifications: unsupported (IsSupported=false)");
                None
            }
            InitOutcome::Failed(err) => {
                log_diag(&format!("desktop notifications: init failed: {err}"));
                None
            }
        };
    }

    /// TurnCompleted 时弹通知：预览该会话最后一条助手回复（≤200 字符）。
    pub(crate) fn maybe_notify_turn_completed(&self, seed: &str) {
        let seed = seed.to_string();
        if !self.notif_enabled.load(Ordering::Relaxed) {
            log_diag(&format!("[notify] turn completed {seed}: disabled"));
            return;
        }
        let notifier = self
            .notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(notifier) = notifier else {
            log_diag(&format!("[notify] turn completed {seed}: notifier missing"));
            return; // 未初始化（不支持/失败）；不重复尝试
        };
        let text = self
            .chat_timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(s, _)| *s == seed)
            .and_then(|(_, snap)| {
                // 通知预览：最后一个 turn 的最后一个 text 块（block 模型）。
                snap.turns
                    .iter()
                    .rev()
                    .flat_map(|t| t.rounds.iter().rev().flat_map(|r| r.blocks.iter().rev()))
                    .find(|b| b.kind == qaqh_client::TimelineBlockKind::Text)
                    .filter(|b| !b.text.trim().is_empty())
                    .map(|b| b.text.clone())
            });
        let Some(text) = text else {
            log_diag(&format!("[notify] turn completed {seed}: no preview text"));
            return;
        };
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let body = if flat.chars().count() > 200 {
            let cut: String = flat.chars().take(200).collect();
            format!("{cut}…")
        } else {
            flat
        };
        if body.is_empty() {
            log_diag(&format!("[notify] turn completed {seed}: empty preview"));
            return;
        }
        let body_len = body.chars().count();
        // show 在独立线程执行：WinRT Show 调用不进 UI 线程消息泵
        // （COM 消息泵重入 reconciler 会触发 render fault）。
        std::thread::Builder::new()
            .name("qaqh-notif-show".into())
            .spawn(move || {
                let ok = notifier.show("QAQ-Harness", &body);
                log_diag(&format!(
                    "[notify] turn completed {seed}: show={ok} len={body_len}"
                ));
            })
            .expect("spawn notif show thread");
    }

    // ── XAML Info 面板（bootstrap conversation.state 投影）─────────────

    /// (detail, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新面板。
    pub(crate) fn info_snapshot(&self) -> (Option<SessionDetail>, u64) {
        let detail = self.info.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let rev = self.info_rev.load(Ordering::Relaxed);
        (detail, rev)
    }

    /// 后台拉取指定会话的用量详情：`client.bootstrap` → `conversation.state`
    /// 投影 → 缓存 + rev++（对齐 conversation_snapshot.rs:29-39 形状）。
    /// 快照为 None（会话无持久状态）时保留旧缓存。
    pub(crate) fn spawn_refresh_info(&self, seed: String) {
        log_diag(&format!("spawn_refresh_info({seed})"));
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            core.refresh_info_inner(&seed).await;
        });
    }
}
