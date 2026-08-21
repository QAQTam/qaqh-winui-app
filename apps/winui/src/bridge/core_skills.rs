//! BridgeCore methods: skills.

use std::sync::atomic::Ordering;

use qaqh_client::ActionRequest;

use crate::shell_store::{SettingsSnapshot, parse_skills_payload};

use super::*;

impl super::BridgeCore {
    /// 后端是否已连接（daemon 就绪且 client 建立）。开屏覆盖层显隐依据。
    pub(crate) fn backend_connected(&self) -> bool {
        self.client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// 无缓存时向 daemon 拉一次权威快照（进入技能页首次渲染兜底）。
    ///
    /// 正常路径下 `skills_updated` 事件持续推送（事件即完整快照），无需
    /// 主动拉取；兜底覆盖“事件在页面挂载前已推送”的窗口。
    pub(crate) fn ensure_skills(&self) {
        if self
            .skills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return;
        }
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("ensure_skills: connect failed: {err}"));
                    return;
                }
            };
            let seed = core.active_seed();
            if seed.is_empty() {
                return;
            }
            match client.bootstrap(&seed).await {
                Ok(snapshot) => {
                    if let Some(skills) = snapshot.control.state.get("skills") {
                        let mut snap = parse_skills_payload(skills);
                        snap.seed = seed;
                        core.skills
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .replace(snap);
                        core.skills_rev.fetch_add(1, Ordering::Relaxed);
                        log_diag("ensure_skills: bootstrap snapshot cached");
                    } else {
                        log_diag("ensure_skills: no control.skills in bootstrap snapshot");
                    }
                }
                Err(err) => log_diag(&format!("ensure_skills: bootstrap failed: {err}")),
            }
        });
    }

    /// 技能动作（对应 `skills.operation` 协议：request/release/retain）。
    ///
    /// seed 取当前激活会话；operation_id 用壳内序号（daemon 无 UUID 强校验，
    /// 仅透传去重）；expected_revision 取快照 operation_revision（幂等）。
    pub(crate) fn spawn_skill_operation(&self, action: &str, name: &str) {
        let core = self.self_arc();
        let action = action.to_string();
        let name = name.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("skill {action} {name}: connect failed: {err}"));
                    return;
                }
            };
            let seed = core.active_seed();
            if seed.is_empty() {
                log_diag(&format!("skill {action} {name}: no active session"));
                return;
            }
            let revision = core
                .skills
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|s| s.operation_revision)
                .unwrap_or(0);
            match client
                .action(ActionRequest::SkillsOperation {
                    seed,
                    operation_id: core.next_command_id(),
                    action: action.clone(),
                    name: name.clone(),
                    expected_revision: revision,
                })
                .await
            {
                Ok(_) => log_diag(&format!("skill operation {action} {name}: ok")),
                Err(err) => log_diag(&format!("skill operation {action} {name}: failed: {err}")),
            }
        });
    }

    /// 技能目录重载（对应 `skills.reload` 协议）。
    pub(crate) fn spawn_skill_reload(&self) {
        let core = self.self_arc();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("skill reload: connect failed: {err}"));
                    return;
                }
            };
            let seed = core.active_seed();
            if seed.is_empty() {
                log_diag("skill reload: no active session");
                return;
            }
            match client.action(ActionRequest::SkillsReload { seed }).await {
                Ok(_) => log_diag("skill reload: ok"),
                Err(err) => log_diag(&format!("skill reload: failed: {err}")),
            }
        });
    }

    // ── XAML 设置页（config.load 投影 + 壳直连命令，D-2 原则）───────

    /// (snapshot, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新。
    pub(crate) fn settings_snapshot(&self) -> (Option<SettingsSnapshot>, u64) {
        let snap = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.settings_rev.load(Ordering::Relaxed);
        (snap, rev)
    }

    /// (projection, rev)：Web `shell.setSettings` 初始投影（theme/lang/…）。
    pub(crate) fn settings_projection(&self) -> (SettingsProjection, u64) {
        let proj = self
            .settings_proj
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.settings_proj_rev.load(Ordering::Relaxed);
        (proj, rev)
    }
}
