//! BridgeCore methods: lifecycle.

use std::sync::atomic::Ordering;

use qaqh_client::{CommandOptions, ControlCommand, RingingCommand};
use serde_json::Value;

use crate::shell_store::{SkillsSnapshot, parse_conversation_state};

use super::*;

impl super::BridgeCore {
    pub(crate) fn apply_bootstrap_conversation_state(&self, seed: &str, state: &Value) {
        let detail = parse_conversation_state(state);
        let status = detail.compact_status.clone();
        let mut compact_statuses = self
            .compact_statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if status.is_empty() {
            compact_statuses.remove(seed);
        } else {
            compact_statuses.insert(seed.to_string(), status);
        }
        drop(compact_statuses);
        if self.active_seed() == seed {
            *self.info.lock().unwrap_or_else(|e| e.into_inner()) = Some(detail);
            self.info_rev.fetch_add(1, Ordering::Relaxed);
            self.refresh_header();
        }
    }

    pub(crate) async fn refresh_info_inner(&self, seed: &str) {
        if seed.is_empty() {
            return;
        }
        let client = match self.ensure_client().await {
            Ok(client) => client,
            Err(err) => {
                log_diag(&format!("refresh_info: connect failed: {err}"));
                return;
            }
        };
        let bootstrap = match client.bootstrap(seed).await {
            Ok(v) => v,
            Err(err) => {
                log_diag(&format!("refresh_info: bootstrap failed: {err}"));
                return;
            }
        };
        self.apply_bootstrap_conversation_state(seed, &bootstrap.conversation.state);
        if self.active_seed() != seed {
            log_diag(&format!(
                "refresh_info: cached compact state but discarded stale info projection for {seed}"
            ));
            return;
        }
        log_diag(&format!("refresh_info: {seed} refreshed"));
    }

    /// 新建会话：`session_create`（control）+ 轮询发现新 seed（对齐前端
    /// `waitForSessionCreated` 的 15s 超时）→ navigate chat。
    /// 若当前选中 workspace（sidebar 上下文），SessionCreate 携带其 path →
    /// daemon 记录 cwd 并自动归属。
    pub(crate) fn spawn_new_session(&self) {
        let core = self.self_arc();
        let cwd = core.current_workspace_path();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("new_session: connect failed: {err}"));
                    return;
                }
            };
            // 先刷新拿基线，避免"空列表时把旧会话当新会话"。
            core.refresh_sessions_inner().await;
            let before = core.seed_set();
            match client
                .send_command(
                    None,
                    RingingCommand::Control(ControlCommand::SessionCreate {
                        close_current: false,
                        cwd: cwd.clone(),
                        tool_mode: None,
                        custom_tools: Vec::new(),
                    }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    for _ in 0..30 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        core.refresh_sessions_inner().await;
                        let now = core.seed_set();
                        if let Some(new_seed) = now.iter().find(|s| !before.contains(*s)) {
                            let new_seed = new_seed.clone();
                            core.set_active_seed(&new_seed);
                            log_diag(&format!("new_session: created {new_seed}"));
                            core.navigate("chat", Some(&new_seed));
                            return;
                        }
                    }
                    log_diag("new_session: no new seed within 15s");
                }
                Err(err) => log_diag(&format!("new_session: command failed: {err}")),
            }
        });
    }

    /// 恢复会话：`attach(seed)`（session_resume 语义）+ 显式激活 timeline 流
    /// （快照 restore 历史）+ navigate chat。
    ///
    /// 幂等：seed 已是 active 时跳过 attach（挡重复 attach 的网络往返），
    /// 但仍 navigate 回 chat——壳的 current_view 可能已离开 chat（用户点过
    /// 技能/设置，或 resume 失败回 home），否则"点同一会话无反应"。
    ///
    /// BUG-003/004：resume_generation 意图代次。每次调用递增作废所有在途
    /// 任务；异步任务在 attach 后、set_active_seed 前、activate_timeline
    /// 前、navigate 前四处校验——只有最新点击能切换会话，过期任务不会
    /// 乱序覆盖 active、不会踢掉当前 stream（BUG-004 根因 2）。
    pub(crate) fn spawn_resume(&self, seed: &str) {
        let generation = self.resume_generation.fetch_add(1, Ordering::Relaxed) + 1;
        if self.active_seed() == seed {
            log_diag(&format!("resume {seed}: already active, re-navigate only"));
            self.navigate("chat", Some(seed));
            return;
        }
        let core = self.self_arc();
        let seed = seed.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("resume {seed}: connect failed: {err}"));
                    return;
                }
            };
            if let Err(err) = client.attach(&seed).await {
                log_diag(&format!("resume {seed}: attach failed: {err}"));
                return;
            }
            // 检查点 1：attach 完成后再确认（attach 网络往返期间可能被新点击
            // supersede）；不过期才允许 set_active_seed 副作用。
            if core.resume_generation.load(Ordering::Relaxed) != generation {
                log_diag(&format!("resume {seed}: superseded before set_active_seed"));
                return;
            }
            // 原生 ChatView 数据源：显式激活 timeline 流，daemon 推送
            // `TimelineSnapshot`（权威 turns 历史）→ bridge 缓存 → restore。
            // 先记录 seed 再 activate：快照可能瞬时到达，缓存标记须就绪。
            *core
                .last_timeline_seed
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = seed.clone();
            // 先同步 active_seed 再 activate：快照瞬时到达时 seed 校验已
            // 就绪（原顺序 activate 在前，快照早到会在 chat_view 泵里被
            // 当作 stale 丢弃——即使没有 Web 竞争也存在竞态窗口）。
            core.set_active_seed(&seed);
            match client.bootstrap(&seed).await {
                Ok(snapshot) => {
                    core.apply_bootstrap_conversation_state(&seed, &snapshot.conversation.state)
                }
                Err(err) => log_diag(&format!("resume {seed}: bootstrap failed: {err}")),
            }
            // 检查点 2：activate_timeline 前（BUG-004 根因 2）——过期任务
            // 仍会停掉当前唯一 stream 并起自己的 stream，必须先挡。
            if core.resume_generation.load(Ordering::Relaxed) != generation {
                log_diag(&format!(
                    "resume {seed}: superseded before activate_timeline"
                ));
                return;
            }
            if let Err(err) = client.activate_timeline(&seed).await {
                log_diag(&format!("resume {seed}: activate_timeline failed: {err}"));
            }
            // An already-open InfoPanel must follow the selected tab. Waiting
            // here also prevents the previous seed's bootstrap from winning a
            // race against this navigation.
            let info_open = core
                .header_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .info_open;
            if info_open {
                core.refresh_info_inner(&seed).await;
            }
            // rev++ 让侧栏 timer 同步 active 高亮（selected_tag 受控刷新）。
            core.session_rev.fetch_add(1, Ordering::Relaxed);
            log_diag(&format!("resume: attached {seed}"));
            // 检查点 3：navigate 前最后一道（不再动 UI 状态）。
            if core.resume_generation.load(Ordering::Relaxed) != generation {
                log_diag(&format!("resume {seed}: superseded before navigate"));
                return;
            }
            core.navigate("chat", Some(&seed));
        });
    }

    /// 归档会话（标签 ×）：Ringing `session_archive`——daemon 侧关实例 +
    /// meta `archived=true`（磁盘保留，左侧列表归档组可见可恢复）。
    ///
    /// 归档的是活动会话时自动切邻居：列表首个非归档会话（updated_at 序），
    /// 无则清空活动态回 home（空态 + 加号引导）。
    pub(crate) fn spawn_archive(&self, seed: &str) {
        let is_active = self.active_seed() == seed;
        let neighbor = if is_active {
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|s| !s.archived && s.seed != seed)
                .map(|s| s.seed.clone())
        } else {
            None
        };
        let core = self.self_arc();
        let seed = seed.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("archive {seed}: connect failed: {err}"));
                    return;
                }
            };
            match client
                .send_command(
                    Some(&seed),
                    RingingCommand::Control(ControlCommand::SessionArchive { seed: seed.clone() }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    core.refresh_sessions_inner().await;
                    if let Some(neighbor) = neighbor {
                        core.spawn_resume(&neighbor);
                    } else {
                        core.set_active_seed("");
                        core.navigate("home", None);
                    }
                }
                Err(err) => log_diag(&format!("archive {seed}: command failed: {err}")),
            }
        });
    }

    /// 恢复归档会话：Ringing `session_unarchive`（meta `archived=false` +
    /// 重新拉起实例），成功后走 resume 链路（attach + timeline 快照）。
    pub(crate) fn spawn_unarchive(&self, seed: &str) {
        let core = self.self_arc();
        let seed = seed.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("unarchive {seed}: connect failed: {err}"));
                    return;
                }
            };
            match client
                .send_command(
                    Some(&seed),
                    RingingCommand::Control(ControlCommand::SessionUnarchive {
                        seed: seed.clone(),
                    }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    core.refresh_sessions_inner().await;
                    core.spawn_resume(&seed);
                }
                Err(err) => log_diag(&format!("unarchive {seed}: command failed: {err}")),
            }
        });
    }

    /// 彻底删除会话：Ringing `session_delete`（daemon 侧先关实例再删磁盘
    /// 目录与索引——区别于归档；原 `session_close` 只关实例不删文件）。
    /// 删除的是活动会话时清空活动态回 home。
    pub(crate) fn spawn_delete(&self, seed: &str) {
        let core = self.self_arc();
        let seed = seed.to_string();
        let _ = qaqh_client::runtime_handle().spawn(async move {
            let client = match core.ensure_client().await {
                Ok(client) => client,
                Err(err) => {
                    log_diag(&format!("delete {seed}: connect failed: {err}"));
                    return;
                }
            };
            match client
                .send_command(
                    Some(&seed),
                    RingingCommand::Control(ControlCommand::SessionDelete { seed: seed.clone() }),
                    CommandOptions::default(),
                )
                .await
            {
                Ok(_) => {
                    core.compact_statuses
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&seed);
                    if core.active_seed() == seed {
                        core.set_active_seed("");
                        core.navigate("home", None);
                    }
                    core.refresh_sessions_inner().await;
                }
                Err(err) => log_diag(&format!("delete {seed}: command failed: {err}")),
            }
        });
    }

    // ── XAML 技能页（skills_updated 投影，WORKFLOW §8）────────────

    /// (snapshot, rev) 快照：UI 侧 timer 比对 rev 决定是否刷新。
    pub(crate) fn skills_snapshot(&self) -> (Option<SkillsSnapshot>, u64) {
        let snap = self
            .skills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rev = self.skills_rev.load(Ordering::Relaxed);
        (snap, rev)
    }

    /// 壳主导的当前视图（main.rs 内容区视图切换判定）。
    pub(crate) fn current_view(&self) -> String {
        self.current_view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
