//! Bridge data types, state machines and parsing helpers.

use std::collections::{HashMap, VecDeque};

use qaqh_client::{
    ControlEvent, ConversationEvent as DomainConversationEvent, PermissionCategory, PermissionRisk,
    ToolEvent,
};
use serde::{Deserialize, Serialize};

use super::*;
/// 直连模式的发送反馈（替代 Web setComposer 的 submitError/sendAck 投影）。
#[derive(Debug, Clone, Default)]
pub(crate) struct ComposerFeedback {
    /// 最近发送失败原因（空 = 无错误；composer_bar 显示且不清空草稿）。
    pub(crate) submit_error: String,
    /// 发送 accepted 后递增（悲观清空信号；UI 侧已本地清空，保留兼容）。
    pub(crate) send_ack: u64,
}

/// Per-session **timeline** live entry queues（Phase 2：timeline 单源）。
///
/// 与 [`ChatEventQueues`] 并存（双通道对照期）；timeline 事件带全局单调
/// `timeline_seq`，delta 可丢（BlockCheckpoint 覆盖自愈 + 快照兜底），
/// 结构性事件（打开/封存/终态）强制保留——丢封存会断 done 判定链。
#[derive(Default)]
pub(crate) struct TimelineEventQueues {
    pub(crate) by_seed: HashMap<String, VecDeque<qaqh_client::TimelineEntry>>,
    pub(crate) total_len: usize,
}

impl TimelineEventQueues {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    /// Enqueue one timeline entry. Deltas are dropped at the global fuse;
    /// structural entries force-in by evicting an older delta (same seed
    /// first, mirroring `ChatEventQueues`).
    pub(crate) fn push(
        &mut self,
        seed: String,
        entry: qaqh_client::TimelineEntry,
        is_delta: bool,
    ) -> bool {
        if self.total_len >= TIMELINE_QUEUE_CAP && (is_delta || !self.evict_one(seed.as_str())) {
            return false;
        }
        self.by_seed.entry(seed).or_default().push_back(entry);
        self.total_len += 1;
        true
    }

    pub(crate) fn evict_one(&mut self, preferred_seed: &str) -> bool {
        let mut removed = self
            .by_seed
            .get_mut(preferred_seed)
            .is_some_and(remove_oldest_timeline_delta);
        if !removed {
            removed = self.by_seed.values_mut().any(remove_oldest_timeline_delta);
        }
        if !removed {
            removed = self
                .by_seed
                .get_mut(preferred_seed)
                .is_some_and(|queue| queue.pop_front().is_some());
        }
        if !removed {
            removed = self
                .by_seed
                .values_mut()
                .any(|queue| queue.pop_front().is_some());
        }
        if removed {
            self.total_len -= 1;
            self.by_seed.retain(|_, queue| !queue.is_empty());
        }
        removed
    }

    /// Normal frame-pump path: O(limit), independent of background backlog.
    pub(crate) fn drain_seed(
        &mut self,
        seed: &str,
        limit: usize,
    ) -> Vec<qaqh_client::TimelineEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let Some(queue) = self.by_seed.get_mut(seed) else {
            return Vec::new();
        };
        let take = limit.min(queue.len());
        let events: Vec<_> = queue.drain(..take).collect();
        self.total_len -= events.len();
        if queue.is_empty() {
            self.by_seed.remove(seed);
        }
        events
    }
}

/// 移除队列中最早的 delta 条目（保险丝挤出；保留结构性条目）。
pub(crate) fn remove_oldest_timeline_delta(
    queue: &mut VecDeque<qaqh_client::TimelineEntry>,
) -> bool {
    let index = queue.iter().position(is_timeline_delta);
    index.map(|i| queue.remove(i)).is_some()
}

/// timeline 高频可丢事件（checkpoint 覆盖自愈 + 快照兜底）。
pub(crate) fn is_timeline_delta(entry: &qaqh_client::TimelineEntry) -> bool {
    matches!(
        entry.event,
        qaqh_client::TimelineEvent::TextDelta { .. }
            | qaqh_client::TimelineEvent::BlockCheckpoint { .. }
            | qaqh_client::TimelineEvent::ToolProgress { .. }
    )
}

/// XAML 标题栏状态（headerDirect：Rust 从壳导航/会话列表/conversation
/// 事件组装；Web `shell.setHeader` 投影仅在直连关闭时生效）。
///
/// 字段名对齐 Web 侧 `HeaderState`（camelCase）。`#[serde(default)]` 保证
/// 未来字段扩展向后兼容（P-2 typed struct 预埋，见 WORKFLOW §6.1）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HeaderState {
    pub view: String,
    pub title: String,
    /// 当前会话运行工作区路径（`workspace.set` 成功后写入；此前该字段
    /// 只声明从未赋值，标题栏永远显示「未选择工作区」→ 用户无任何反馈）。
    pub workspace: String,
    /// 工作区选择/设置失败文案（None = 无错误；header 500ms 轮询展示，
    /// 成功选择或新会话刷新后清除——修复「选择失败但零提示」）。
    pub workspace_error: Option<String>,
    /// 当前会话 seed（chat 视图；apply_header 同步 active_seed）。
    pub seed: String,
    pub info_open: bool,
    pub stats_open: bool,
    pub compacting: bool,
    pub compact_disabled: bool,
    pub undo_disabled: bool,
    pub pet_enabled: bool,
}

/// 标题栏本地开关（headerDirect：壳本地翻转，不回传 Web）。
#[derive(Debug, Clone, Copy)]
pub enum HeaderFlag {
    /// Info 面板开合（2026-08 移除标题栏按钮后暂未使用；保留变体，
    /// 恢复入口时即用）。
    #[allow(dead_code)]
    Info,
    /// Stats 面板开合。
    Stats,
}

/// XAML 设置页本地投影（由 `config.load` 等 daemon 数据派生）。
///
/// theme/lang/permissionLevel 是壳侧 UI 的展示投影；写操作统一走
/// `config.save` / `config.set_permission_level`，不再经 Web 回传。
/// WebView 已移除，`shell.settingsAction` 通道不存在。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsProjection {
    /// system | light | dark | dark-gray（三态进协议，P-5）。
    pub theme: String,
    /// en | zh。
    pub lang: String,
    pub permission_level: u64,
    /// local | wsl | remote（workspace 运行环境）。
    pub workspace_mode: String,
}

/// 远端 daemon 直连档案（临时跨端模式，壳本地持久化）。
///
/// 设置后 `connect_client` 跳过本地 discovery/spawn，直接连 `base_url`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteProfile {
    /// `http://<ip>:<port>`（保存前会规范化：去尾随斜杠）。
    pub base_url: String,
    /// `qaqh-daemon server --token` 的 Bearer token。
    pub token: String,
}

/// `fs.list` 单条目（远端文件选择器数据源）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteFsEntry {
    pub name: String,
    /// daemon 侧绝对路径（显示时经 `display_remote_path` 转 `//ip/...`）。
    pub path: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub size: u64,
}

/// `fs.list` 结果投影（path + entries + 加载/错误态）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteFsListing {
    pub path: String,
    pub entries: Vec<RemoteFsEntry>,
    pub loading: bool,
    pub error: Option<String>,
}

/// `fs.read` 文本预览投影（远端文件选择器预览区）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteFsPreview {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// XAML 交互模态状态投影（Web `shell.setInteraction` 载荷）。
///
/// 字段名对齐 Web 侧 `PendingInteraction`（camelCase，`kind` 直通）。
/// `kind` = "none" 表示当前无活动交互（壳关闭覆盖层面板）；
/// "permission" / "ask" / "plan" 三种用户介入模板（统一交互弹窗体系，
/// 见 ELECTRON-MIGRATION.md Phase 5）。`#[serde(default)]` 保证字段扩展
/// 向后兼容（P-2 typed struct 预埋）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InteractionState {
    /// "none" | "permission" | "ask" | "plan"。
    pub kind: String,
    /// 交互 id（permission = tool_call_id；ask = ask id）。
    pub id: String,
    /// 所属会话 seed（回传时定位 activeEntry）。
    pub seed: String,
    // ── permission 字段 ────────────────────────────────
    pub tool_name: String,
    pub reason: String,
    pub paths: Vec<String>,
    pub category: String,
    pub level: u64,
    /// low | medium | high。
    pub risk: String,
    pub consequence: String,
    // ── ask 字段 ───────────────────────────────────────
    pub questions: Vec<AskQuestion>,
    // ── plan 字段 ───────────────────────────────────────
    pub plan_content: String,
    /// todo_activation | 其他（计划审核）。
    pub review_type: String,
    pub todo_items: Vec<PlanTodoItem>,
}

/// plan 审批的任务项（对应 `TodoActivationItem` 协议）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlanTodoItem {
    pub id: String,
    pub title: String,
    pub description: String,
    /// small | medium | large。
    pub complexity: String,
}

/// `ask_user` 中的单个问题（对应 `AskQuestion` 协议，ts-rs 生成）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AskQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub allow_custom: bool,
}

// ── Rust 直连交互队列状态机（读路径直连，不经 WebView）──────────────────
//
// 等价移植 Web `sessionPresentation.pendingInteractions` 组装：
//   permission（tool 频道 pendingPermission 卡片）优先于 ask/plan
//   （control 频道 activeAskPlan），取 [0] 为活动交互。
// daemon 事件 → `parse_interaction_event` / `parse_tool_permission_event`
// → `InteractionMachine::apply` → `snapshot` 组装 InteractionState。
// 幂等：事件重放（SSE 重连续传）经 PartialEq 比对不产生多余 rev。

/// per-seed 交互队列状态机。
#[derive(Debug, Clone, Default)]
pub(crate) struct InteractionMachine {
    /// tool 频道挂起的权限请求（等价 Web tool.cards 中 pendingPermission=true）。
    pub(crate) pending_permissions: Vec<PendingPermission>,
    /// control 频道活动 ask/plan（等价 Web control.activeAskPlan）。
    pub(crate) active_ask_plan: Option<ActiveAskPlan>,
}

/// 挂起的权限请求（`tool_permission_requested` 完整字段；turn_id 仅在事件
/// 层消费，快照形状不含——对齐 Web `PendingInteraction` 投影）。
#[derive(Debug, Clone)]
pub(crate) struct PendingPermission {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) reason: String,
    pub(crate) paths: Vec<String>,
    pub(crate) category: String,
    pub(crate) level: u64,
    pub(crate) risk: String,
    pub(crate) consequence: String,
}

/// control 频道活动 ask/plan（`activeAskPlan` 等价形状；turn_id 不投影）。
#[derive(Debug, Clone)]
pub(crate) enum ActiveAskPlan {
    Ask {
        id: String,
        questions: Vec<AskQuestion>,
    },
    Plan {
        id: String,
        plan_content: String,
        review_type: String,
        todo_items: Vec<PlanTodoItem>,
    },
}

/// control 频道交互事件（`parse_interaction_event` 解析产物，对齐 Web
/// `controlReducer` 的 interaction_requested / interaction_resolved /
/// plan_review_requested / plan_review_resolved / operation_failed 分支）。
pub(crate) enum InteractionEvent {
    AskRequested {
        id: String,
        questions: Vec<AskQuestion>,
    },
    AskResolved {
        id: String,
    },
    PlanRequested {
        id: String,
        plan_content: String,
        review_type: String,
        todo_items: Vec<PlanTodoItem>,
    },
    PlanResolved {
        id: String,
    },
    /// operation_failed（ask_rejected / interaction_not_found）→ 幽灵交互
    /// 自愈：worker 重启后挂起态丢失，SSE 重放的历史 interaction_requested
    /// 无终态时清除活动面板，让 UI 回到可操作状态（对齐 Web reducer）。
    GhostCleanup,
}

/// tool 频道权限事件（`parse_tool_permission_event` 解析产物，对齐 Web
/// `toolReducer` 的 tool_permission_requested / tool_finished 分支）。
pub(crate) enum ToolPermissionEvent {
    Requested {
        tool_call_id: String,
        tool_name: String,
        reason: String,
        paths: Vec<String>,
        category: String,
        level: u64,
        risk: String,
        consequence: String,
    },
    /// tool_finished：权限已响应（Web 侧置 pendingPermission=false，此处
    /// 直接移除——组装只消费 pendingPermission 卡片，语义等价）。
    Resolved { tool_call_id: String },
}

impl InteractionMachine {
    pub(crate) fn apply(&mut self, ev: InteractionEvent) {
        match ev {
            InteractionEvent::AskRequested { id, questions } => {
                self.active_ask_plan = Some(ActiveAskPlan::Ask { id, questions });
            }
            InteractionEvent::AskResolved { id } => {
                if matches!(&self.active_ask_plan, Some(ActiveAskPlan::Ask { id: cur, .. }) if cur == &id)
                {
                    self.active_ask_plan = None;
                }
            }
            InteractionEvent::PlanRequested {
                id,
                plan_content,
                review_type,
                todo_items,
            } => {
                self.active_ask_plan = Some(ActiveAskPlan::Plan {
                    id,
                    plan_content,
                    review_type,
                    todo_items,
                });
            }
            InteractionEvent::PlanResolved { id } => {
                if matches!(&self.active_ask_plan, Some(ActiveAskPlan::Plan { id: cur, .. }) if cur == &id)
                {
                    self.active_ask_plan = None;
                }
            }
            InteractionEvent::GhostCleanup => {
                self.active_ask_plan = None;
            }
        }
    }

    /// 应用 tool 频道权限事件（独立于 control 的 ask/plan 状态机）。
    pub(crate) fn apply_tool(&mut self, ev: ToolPermissionEvent) {
        match ev {
            ToolPermissionEvent::Requested {
                tool_call_id,
                tool_name,
                reason,
                paths,
                category,
                level,
                risk,
                consequence,
            } => {
                // upsert：同 tool_call_id 覆盖（对齐 Web 卡片 patch），
                // 移除后 push 末尾 → 最新请求排最后，first 仍为最旧。
                self.pending_permissions
                    .retain(|p| p.tool_call_id != tool_call_id);
                self.pending_permissions.push(PendingPermission {
                    tool_call_id,
                    tool_name,
                    reason,
                    paths,
                    category,
                    level,
                    risk,
                    consequence,
                });
            }
            ToolPermissionEvent::Resolved { tool_call_id } => {
                self.pending_permissions
                    .retain(|p| p.tool_call_id != tool_call_id);
            }
        }
    }

    /// 组装活动交互（permission 优先，等价 Web `pendingInteractions[0]`）。
    /// 无活动交互时返回 default（kind=""，XAML 覆盖层判空关闭）。
    pub(crate) fn snapshot(&self, seed: &str) -> InteractionState {
        if let Some(p) = self.pending_permissions.first() {
            return InteractionState {
                kind: "permission".into(),
                id: p.tool_call_id.clone(),
                seed: seed.to_string(),
                tool_name: p.tool_name.clone(),
                reason: p.reason.clone(),
                paths: p.paths.clone(),
                category: p.category.clone(),
                level: p.level,
                risk: p.risk.clone(),
                consequence: p.consequence.clone(),
                ..InteractionState::default()
            };
        }
        match &self.active_ask_plan {
            Some(ActiveAskPlan::Plan {
                id,
                plan_content,
                review_type,
                todo_items,
                ..
            }) => InteractionState {
                kind: "plan".into(),
                id: id.clone(),
                seed: seed.to_string(),
                plan_content: plan_content.clone(),
                review_type: review_type.clone(),
                todo_items: todo_items.clone(),
                ..InteractionState::default()
            },
            Some(ActiveAskPlan::Ask { id, questions, .. }) => InteractionState {
                kind: "ask".into(),
                id: id.clone(),
                seed: seed.to_string(),
                questions: questions.clone(),
                ..InteractionState::default()
            },
            None => InteractionState::default(),
        }
    }

    /// 是否存在挂起交互（composer `hasPendingGate` 直连来源，
    /// 等价 Web `activeInteraction(session()) !== null`）。
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending_permissions.is_empty() || self.active_ask_plan.is_some()
    }
}

/// 从 control 频道事件提取交互队列更新。
///
/// 事件形状（qaqh-domain `ControlEvent`，`tag="type"`）：
/// `interaction_requested { interaction_id, turn_id, mode, questions[] }`、
/// `interaction_resolved { interaction_id, resolution }`、
/// `plan_review_requested { interaction_id, turn_id, plan_content, review_type,
/// todo_items?[] }`、`plan_review_resolved { interaction_id, approved }`、
/// `operation_failed { error: { code } }`（幽灵自愈）。`type` 不符返回 None。
pub(crate) fn interaction_event(event: &ControlEvent) -> Option<InteractionEvent> {
    match event {
        ControlEvent::InteractionRequested {
            interaction_id,
            questions,
            ..
        } => Some(InteractionEvent::AskRequested {
            id: interaction_id.clone(),
            questions: questions
                .iter()
                .map(|question| AskQuestion {
                    id: question.id.clone(),
                    question: question.question.clone(),
                    options: question.options.clone(),
                    allow_custom: question.allow_custom,
                })
                .collect(),
        }),
        ControlEvent::InteractionResolved { interaction_id, .. } => {
            Some(InteractionEvent::AskResolved {
                id: interaction_id.clone(),
            })
        }
        ControlEvent::PlanReviewRequested {
            interaction_id,
            plan_content,
            review_type,
            todo_items,
            ..
        } => Some(InteractionEvent::PlanRequested {
            id: interaction_id.clone(),
            plan_content: plan_content.clone(),
            review_type: review_type.clone(),
            todo_items: todo_items
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|item| PlanTodoItem {
                    id: item.id.clone(),
                    title: item.title.clone(),
                    description: item.description.clone(),
                    complexity: item.complexity.clone(),
                })
                .collect(),
        }),
        ControlEvent::PlanReviewResolved { interaction_id, .. } => {
            Some(InteractionEvent::PlanResolved {
                id: interaction_id.clone(),
            })
        }
        ControlEvent::OperationFailed { error, .. }
            if matches!(
                error.code.as_str(),
                "ask_rejected" | "interaction_not_found"
            ) =>
        {
            Some(InteractionEvent::GhostCleanup)
        }
        _ => None,
    }
}

/// 从 tool 频道事件提取权限队列更新。
///
/// 事件形状（qaqh-domain `ToolEvent`）：`tool_permission_requested
/// { tool_call_id, turn_id, round_num, tool_name, reason, paths[],
/// category, level, risk, consequence }`、`tool_finished { tool_call_id, ... }`。
/// 注意 daemon 字段为 snake_case（`allow_custom` 等），与壳投影
/// （camelCase `allowCustom`）不同——解析时手动取 snake_case 键。
pub(crate) fn tool_permission_event(event: &ToolEvent) -> Option<ToolPermissionEvent> {
    match event {
        ToolEvent::ToolPermissionRequested {
            tool_call_id,
            tool_name,
            reason,
            paths,
            category,
            level,
            risk,
            consequence,
            ..
        } => Some(ToolPermissionEvent::Requested {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            reason: reason.clone(),
            paths: paths.clone(),
            category: match category {
                PermissionCategory::Read => "read",
                PermissionCategory::Write => "write",
                PermissionCategory::Exec => "exec",
                PermissionCategory::Net => "net",
            }
            .to_string(),
            level: u64::from(*level),
            risk: match risk {
                PermissionRisk::Low => "low",
                PermissionRisk::Medium => "medium",
                PermissionRisk::High => "high",
            }
            .to_string(),
            consequence: consequence.clone(),
        }),
        ToolEvent::ToolFinished { tool_call_id, .. } => Some(ToolPermissionEvent::Resolved {
            tool_call_id: tool_call_id.clone(),
        }),
        _ => None,
    }
}

/// 解析 daemon `questions` 数组（snake_case 键 → 壳投影 camelCase 形状）。
#[cfg(test)]
pub(crate) fn parse_questions(v: &Value) -> Vec<AskQuestion> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|q| {
                    Some(AskQuestion {
                        id: q.get("id")?.as_str()?.to_string(),
                        question: q.get("question")?.as_str()?.to_string(),
                        options: q
                            .get("options")
                            .and_then(|o| o.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        allow_custom: q
                            .get("allow_custom")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 解析 daemon `todo_items`（可为 null；字段无 camelCase 转换需求）。
#[cfg(test)]
pub(crate) fn parse_todo_items(v: Option<&Value>) -> Vec<PlanTodoItem> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    Some(PlanTodoItem {
                        id: t.get("id")?.as_str()?.to_string(),
                        title: t.get("title")?.as_str()?.to_string(),
                        description: t
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                        complexity: t
                            .get("complexity")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Native composer activity tracking ────────────────────────────────
//
// Canonical conversation events own streaming/usage state. Mode and send
// feedback are local UI state; permission comes from the typed settings cache.

/// 卡死阈值（对齐 Web `SESSION_STALL_TIMEOUT_MS`）：超时视为流式中断。
pub(crate) const COMPOSER_STALL_TIMEOUT_MS: u64 = 4 * 60 * 1000;

/// 子代理幽灵检测窗口：`ToolFinished`（spawn 确认）后超过该时长仍无
/// `[SUBAGENT ...]` 注入 tag 收敛，即标记 Lost（UI 琥珀提示）。
/// 与 spawn 的 cfg.subagent.timeout_secs 默认值相比足够宽容；注入迟到时
/// 真实终态仍会覆盖 Lost（Lost 优先级最低）。
pub(crate) const SUBAGENT_GHOST_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// per-seed composer 活动追踪（isStreaming 判定 + usage 缓存）。
#[derive(Debug, Clone, Default)]
pub(crate) struct ComposerActivity {
    /// activeTurn 是否存在（turn_started 置 true；终态置 false）。
    pub(crate) active_turn: bool,
    /// 最近领域事件时间（epoch ms；0 = 未知 → 保守视为流式中）。
    pub(crate) last_activity_at: u64,
    /// `usage_updated` 缓存（contextTokens = usage.prompt_tokens，对齐 Web）。
    pub(crate) prompt_tokens: u64,
    pub(crate) context_limit: u64,
    pub(crate) model: String,
    /// 工作阶段（状态栏）：round_delta kind / tool 生命周期更新。
    pub(crate) phase: WorkPhase,
}

/// conversation 频道活动事件（`parse_conversation_activity_event` 解析产物）。
pub(crate) enum ConversationActivityEvent {
    /// turn_started：活动开始。
    Started,
    /// turn_completed / turn_failed / conversation_cancelled：活动结束。
    Ended,
    /// round_delta（携带阶段）：thinking/answering 细分。
    Delta(WorkPhase),
    /// round_completed / provider_retrying / provider_tool_status：活动（刷新时间戳）。
    Touched,
    /// usage_updated：活动 + model/context_limit/prompt_tokens 缓存。
    Usage {
        prompt_tokens: u64,
        context_limit: u64,
        model: String,
    },
}

impl ComposerActivity {
    /// 等价 Web `isSessionStreaming`：activeTurn 存在且最近活动未超时；
    /// 时间戳未知（旧数据/恢复间隙）保守按流式中处理。
    pub(crate) fn is_streaming(&self, now: u64) -> bool {
        if !self.active_turn {
            return false;
        }
        if self.last_activity_at == 0 {
            return true;
        }
        now.saturating_sub(self.last_activity_at) < COMPOSER_STALL_TIMEOUT_MS
    }

    pub(crate) fn apply(&mut self, ev: ConversationActivityEvent, now: u64) {
        match ev {
            ConversationActivityEvent::Started => {
                self.active_turn = true;
                self.last_activity_at = now;
            }
            ConversationActivityEvent::Ended => {
                self.active_turn = false;
                self.phase = WorkPhase::Idle;
            }
            ConversationActivityEvent::Touched => {
                // A delta/checkpoint is itself proof that a turn is active. This
                // also recovers after reconnect when TurnStarted was emitted
                // before this client subscribed.
                self.active_turn = true;
                self.last_activity_at = now;
            }
            ConversationActivityEvent::Delta(phase) => {
                self.active_turn = true;
                self.last_activity_at = now;
                // 阶段细分：thinking/answering 流式即状态（与渲染同源）。
                self.phase = phase;
            }
            ConversationActivityEvent::Usage {
                prompt_tokens,
                context_limit,
                model,
            } => {
                // Usage can be the first replayed event after reconnect; treat
                // it as an active-turn signal for the same reason as Touched.
                self.active_turn = true;
                self.prompt_tokens = prompt_tokens;
                self.context_limit = context_limit;
                self.model = model;
                self.last_activity_at = now;
            }
        }
    }
}

/// 从 conversation 频道事件提取活动更新（对齐 Web `applyConversationEventToDraft`
/// 的活动刷新语义：除 compact_* 外所有领域事件都视为活动）。
///
/// 事件形状（qaqh-domain `ConversationEvent`）：`turn_started { turn_id,
/// user_text }`、`turn_completed { turn_id, stop_reason?, usage? }`、
/// `turn_failed { turn_id, error }`、`conversation_cancelled { turn_id? }`、
/// `usage_updated { turn_id, round_num, usage, context_limit, model }`、
/// `round_delta / block_checkpoint / round_completed / provider_retrying /
/// provider_tool_status`。`type` 不符返回 None。
pub(crate) fn conversation_activity_event(
    event: &DomainConversationEvent,
) -> Option<ConversationActivityEvent> {
    match event {
        DomainConversationEvent::TurnStarted { .. } => Some(ConversationActivityEvent::Started),
        DomainConversationEvent::TurnCompleted { .. }
        | DomainConversationEvent::TurnFailed { .. }
        | DomainConversationEvent::ConversationCancelled { .. } => {
            Some(ConversationActivityEvent::Ended)
        }
        DomainConversationEvent::UsageUpdated {
            usage,
            context_limit,
            model,
            ..
        } => Some(ConversationActivityEvent::Usage {
            prompt_tokens: u64::from(usage.prompt_tokens),
            context_limit: u64::from(*context_limit),
            model: model.clone(),
        }),
        DomainConversationEvent::RoundDelta { kind, .. } => {
            // 阶段细分（与渲染同源：thinking/answering 流式即状态）。
            let phase = match kind {
                qaqh_client::RoundDeltaKind::Thinking => WorkPhase::Thinking,
                qaqh_client::RoundDeltaKind::Answering => WorkPhase::Answering,
                qaqh_client::RoundDeltaKind::ToolCalling => WorkPhase::Thinking,
            };
            Some(ConversationActivityEvent::Delta(phase))
        }
        DomainConversationEvent::BlockCheckpoint { .. }
        | DomainConversationEvent::RoundCompleted { .. }
        | DomainConversationEvent::ProviderRetrying { .. }
        | DomainConversationEvent::ProviderToolStatus { .. } => {
            Some(ConversationActivityEvent::Touched)
        }
        DomainConversationEvent::CompactStarted { .. }
        | DomainConversationEvent::CompactProgress { .. }
        | DomainConversationEvent::CompactFinished { .. } => None,
    }
}

/// 当前 unix 时间（epoch ms；系统时钟异常时回退 0——streaming 保守判定）。
pub(crate) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 从 `ToolCallPrepared.args_so_far`（流式 JSON，可能截断）解析 `agent_name`。
/// 解析失败返回 None → 调用方回退短哈希标识。
pub(crate) fn parse_agent_name(args_so_far: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(args_so_far).ok()?;
    value
        .get("agent_name")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.trim().is_empty())
}

/// 解析子代理注入回合文本 `[SUBAGENT 'name' COMPLETED]` 等 → (name, state)。
/// 标签规范见 `crates/qaqh-subagent/src/lib.rs` collect 收尾（COMPLETED /
/// ERROR / CANCELLED / TIMEOUT 变体）。不匹配返回 None。
pub(crate) fn parse_subagent_injection(text: &str) -> Option<(String, SubagentState)> {
    let text = text.trim_start();
    let rest = text.strip_prefix("[SUBAGENT '")?;
    let (name, rest) = rest.split_once("' ")?;
    let state = if rest.starts_with("COMPLETED]") {
        SubagentState::Done
    } else if rest.starts_with("ERROR") {
        SubagentState::Error
    } else if rest.starts_with("TIMEOUT") {
        SubagentState::Timeout
    } else if rest.starts_with("CANCELLED]") {
        SubagentState::Cancelled
    } else {
        return None;
    };
    Some((name.to_string(), state))
}

/// 后端 `SubagentStatus` 控制面事件的 `state` 字段（注入标签原样）→
/// tracker 状态。与 [`parse_subagent_injection`] 保持同一标签约定；
/// 未知值返回 None（忽略，不产生错误状态）。
pub(crate) fn subagent_state_from_tag(state: &str) -> Option<SubagentState> {
    match state {
        "COMPLETED" => Some(SubagentState::Done),
        "ERROR" => Some(SubagentState::Error),
        "TIMEOUT" => Some(SubagentState::Timeout),
        "CANCELLED" => Some(SubagentState::Cancelled),
        _ => None,
    }
}

/// 从 `spawn_subagent` 工具返回（`json_ok`）中解析子代理 seed。
/// 形状探测：JSON 对象同时含 `process_id`（数字）与 `seed`（字符串）
/// 才认——普通工具结果的文本不满足该形状，安全忽略。
pub(crate) fn parse_spawn_seed(model_text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(model_text).ok()?;
    let has_pid = value.get("process_id").is_some_and(|v| v.is_number());
    let seed = value.get("seed").and_then(|v| v.as_str())?;
    if has_pid && !seed.is_empty() {
        Some(seed.to_string())
    } else {
        None
    }
}

/// 数据根目录（对齐 qaqh_types::platform::data_dir）：`QAQH_DATA_DIR`
/// 覆盖，否则 Windows `%USERPROFILE%\.deepx` / Unix `$HOME/.deepx`。
fn data_root_dir() -> std::path::PathBuf {
    std::env::var("QAQH_DATA_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| user_home_dir().join(".deepx"))
}

/// 当前用户 home（Windows `USERPROFILE` / Unix `HOME`）。
fn user_home_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
    } else {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
    }
}

/// 会话元数据文件路径（`<data>/sessions/{seed}/meta.json`）。上下文构成
/// 快照（context_stats）已并入 meta.json（原独立 context_stats.json 退役）。
/// 数据根对齐 qaqh_types::platform::data_dir：`QAQH_DATA_DIR` 覆盖，
/// 否则 Windows `%USERPROFILE%\.deepx` / Unix `$HOME/.deepx`。
pub(crate) fn context_stats_path(seed: &str) -> std::path::PathBuf {
    data_root_dir()
        .join("sessions")
        .join(seed)
        .join("meta.json")
}

/// data-root marker（`<data>/.deepx-data-root.json`）——字段与命名镜像
/// qaqh_types::platform 的 `DataRootMarker`。
#[derive(Debug, Deserialize, Serialize)]
struct DataRootMarker {
    #[serde(rename = "formatVersion")]
    format_version: u32,
    product: String,
    #[serde(rename = "canonicalRoot")]
    canonical_root: String,
    #[serde(rename = "ownerHome")]
    owner_home: String,
    #[serde(rename = "rootId")]
    root_id: String,
}

/// 路径归一（镜像 qaqh_types::platform::normalized_path_text）：反斜杠 →
/// 正斜杠、去 `//?/` 前缀、去尾部 `/`；Windows 统一小写。
fn normalize_data_path_text(path: &std::path::Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    let value = if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        value
    };
    if cfg!(windows) {
        value.trim_end_matches('/').to_ascii_lowercase()
    } else {
        value.trim_end_matches('/').to_string()
    }
}

/// marker rootId（镜像 qaqh_types::platform::data_root_id：FNV-1a 64）。
fn data_root_marker_id(canonical_root: &str, owner_home: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical_root.bytes().chain([0]).chain(owner_home.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("data-{hash:016x}")
}

/// 迁移旧品牌（DeepX——QAQ-Harness 重命名前的产品名）data-root marker：
/// 仅当 canonicalRoot/ownerHome/rootId 全部与当前用户数据根吻合时，把
/// `product` 原位改写为 `QAQ-Harness`（同一产品线升级，数据无损）。
///
/// 背景：daemon `ensure_data_root` 校验 marker 后才启动；旧构建残留的
/// `product:\"DeepX\"` 会让新 daemon 拒绝启动（\"data root marker does not
/// match the current user and path\"）→ 桥 connect 超时（\"daemon did not
/// publish live discovery in time\"）→ 首屏会话/工作区/config 全部失败。
/// 任一字段与当前数据根不吻合即视为异主目录，原样保留（不越权接管）。
///
/// 必须在任何 `connect_client`（spawn daemon）之前调用；返回是否已迁移。
pub(crate) fn migrate_legacy_data_root_marker() -> bool {
    migrate_legacy_data_root_at(&data_root_dir(), &user_home_dir())
}

fn migrate_legacy_data_root_at(data_root: &std::path::Path, home: &std::path::Path) -> bool {
    let marker_path = data_root.join(".deepx-data-root.json");
    let Ok(raw) = std::fs::read_to_string(&marker_path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<DataRootMarker>(&raw) else {
        log_diag("data root marker: unreadable; migration skipped");
        return false;
    };
    if marker.product == "QAQ-Harness" {
        return false;
    }
    if marker.product != "DeepX" {
        log_diag(&format!(
            "data root marker: product {:?} not recognized; migration skipped",
            marker.product
        ));
        return false;
    }
    let canonical_root = normalize_data_path_text(data_root);
    let owner_home = normalize_data_path_text(home);
    if marker.canonical_root != canonical_root
        || marker.owner_home != owner_home
        || marker.root_id != data_root_marker_id(&canonical_root, &owner_home)
    {
        log_diag("data root marker: DeepX marker belongs to another dir/user; migration skipped");
        return false;
    }
    let migrated = DataRootMarker {
        format_version: marker.format_version,
        product: "QAQ-Harness".into(),
        canonical_root,
        owner_home,
        root_id: marker.root_id,
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&migrated) else {
        log_diag("data root marker: migration serialization failed");
        return false;
    };
    let temporary = data_root.join(".deepx-data-root.json.deepx-new");
    if let Err(error) = std::fs::write(&temporary, &bytes) {
        log_diag(&format!(
            "data root marker: migration write failed: {error}"
        ));
        return false;
    }
    match std::fs::rename(&temporary, &marker_path) {
        Ok(_) => {
            log_diag("data root marker: migrated legacy DeepX marker to QAQ-Harness");
            true
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            log_diag(&format!(
                "data root marker: migration rename failed: {error}"
            ));
            false
        }
    }
}

/// `ask_user` 表单中的单个答案（对应 `AskAnswer` 协议：question_id）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AskAnswer {
    pub question_id: String,
    pub answer: String,
}

/// 工作阶段（composer 顶部状态栏数据源）：由 conversation/tool 频道
/// 事件推导（round_delta kind / tool 生命周期 / 交互挂起）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPhase {
    /// 无活动回合。
    #[default]
    Idle,
    /// 模型推理中（round_delta kind=thinking 流式）。
    Thinking,
    /// 模型生成回答中（round_delta kind=answering 流式）。
    Answering,
    /// 工具执行中（tool 频道 ToolStarted；携带工具名）。
    Tool(String),
    /// 交互挂起等待用户（permission/ask/plan 任一 pending）。
    WaitingUser,
}

/// View model consumed directly by the native XAML composer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ComposerState {
    /// 当前会话 seed：壳据此重置草稿（会话切换即清空输入框，同 Web 行为）。
    pub seed: String,
    pub is_streaming: bool,
    pub has_pending_gate: bool,
    /// 工作阶段（状态栏）。
    pub phase: WorkPhase,
    /// plan | code。
    pub mode: String,
    pub model: String,
    pub context_tokens: u64,
    pub context_limit: u64,
    /// 1..=4（对齐 config.permission_level）。
    pub permission_level: u64,
    pub queue_count: u64,
    pub queue_items: Vec<ComposerQueueItem>,
    /// Native send failure shown without clearing the draft.
    pub submit_error: String,
    /// Incremented after a command is accepted so the shell can clear its draft.
    pub send_ack: u64,
    /// 工作状态区模式：`agent`（子代理胶囊）/ `goal_progress`（预留）/ `idle`。
    /// goal 模式后端未接线，本期仅 agent/idle 实际产生。
    pub status_zone: String,
    /// 并行子代理胶囊（工作状态区第二行；空 = 不渲染，行为与现状一致）。
    pub subagents: Vec<SubagentItem>,
    /// 工具模式：standard | minimal | custom（PLAN-TOOL-MODES.md；空 = standard）。
    /// 与 `mode`（plan/code AGENT_MODE）正交：本字段管 allowed 工具集。
    pub tool_mode: String,
    /// 创造模式的自定义工具白名单（仅 custom 生效；空 = 未配置）。
    pub tool_mode_custom_tools: Vec<String>,
}

/// 工具模式本地缓存（standard/minimal/custom + custom_tools）。
/// 与 `composer_mode` 同款乐观更新；初始/外部变化经 meta.json 同步。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ToolModeState {
    pub(crate) mode: String,
    pub(crate) custom_tools: Vec<String>,
}

/// 子代理运行状态（工作状态区胶囊数据源）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    /// 后台运行中（ToolStarted 建实例 → 注入 tag 收敛前）。
    #[default]
    Working,
    /// 注入 `[SUBAGENT 'name' COMPLETED]` 到达。
    Done,
    /// 注入 `[SUBAGENT 'name' ERROR]` 到达。
    Error,
    /// 注入 `[SUBAGENT 'name' TIMEOUT...]` 到达。
    Timeout,
    /// 注入 `[SUBAGENT 'name' CANCELLED]` 到达。
    Cancelled,
    /// 幽灵：spawn 确认后窗口内无注入（注入丢失/被 busy 拒绝）。
    Lost,
}

/// 单个子代理胶囊的视图模型（bridge → composer 状态区）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SubagentItem {
    /// 工具调用 id（tracker 主键；注入 tag 无 call_id，靠 name 关联）。
    pub call_id: String,
    /// 子代理名称（ToolCallPrepared.args_so_far 解析；失败回退短哈希）。
    pub name: String,
    /// 子代理 Ringing seed（spawn 工具返回解析；空 = 尚未解析/未知）。
    /// 面板按需拉取子代理 timeline 的数据源。
    #[serde(default)]
    pub seed: String,
    pub state: SubagentState,
    /// 启动时刻（epoch ms；ToolStarted）。
    pub started_at: u64,
    /// 终态时刻（epoch ms；0 = 运行中）。
    pub finished_at: u64,
}

/// followUpQueue 排队项（壳显示列表 + 删除）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ComposerQueueItem {
    pub id: String,
    pub text: String,
}

/// 图片附件（壳选文件后传路径；Web 侧复用 desktop.readFileBase64 读 base64）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub path: String,
}

/// 文本附件（壳选文件后传路径；Web 侧复用 desktop.readTextFile 读内容）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerTextFile {
    pub file_name: String,
    pub path: String,
}

#[cfg(test)]
mod data_root_tests {
    use super::*;

    #[test]
    fn data_root_marker_id_matches_backend_formula() {
        // 与 qaqh_types::platform::data_root_id 同构（FNV-1a 64）：
        // 旧 DeepX 遗留 marker 的 rootId 必须原样通过校验。
        assert_eq!(
            data_root_marker_id("c:/users/qaqtam/.deepx", "c:/users/qaqtam"),
            "data-d8773e7dcb45bed4"
        );
    }

    #[test]
    fn migrate_legacy_deepx_marker_rewrites_product_in_place() {
        let root = temp_root();
        let home = root.join("home");
        std::fs::create_dir_all(&root.join(".deepx")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let data = std::fs::canonicalize(root.join(".deepx")).unwrap();
        let home = std::fs::canonicalize(&home).unwrap();
        let canonical_root = normalize_data_path_text(&data);
        let owner_home = normalize_data_path_text(&home);
        let root_id = data_root_marker_id(&canonical_root, &owner_home);
        let marker_path = data.join(".deepx-data-root.json");
        std::fs::write(
            &marker_path,
            serde_json::to_vec_pretty(&DataRootMarker {
                format_version: 1,
                product: "DeepX".into(),
                canonical_root: canonical_root.clone(),
                owner_home: owner_home.clone(),
                root_id: root_id.clone(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(migrate_legacy_data_root_at(&data, &home));

        let marker: DataRootMarker =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(marker.product, "QAQ-Harness");
        // 其余字段原样保留（rootId 公式一致，改写后与位置仍吻合）。
        assert_eq!(marker.canonical_root, canonical_root);
        assert_eq!(marker.owner_home, owner_home);
        assert_eq!(marker.root_id, root_id);
        // 幂等：已迁移后不再动作。
        assert!(!migrate_legacy_data_root_at(&data, &home));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn migrate_skips_foreign_or_unknown_markers() {
        let root = temp_root();
        std::fs::create_dir_all(&root.join(".deepx")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let data = std::fs::canonicalize(root.join(".deepx")).unwrap();
        let home = std::fs::canonicalize(root.join("home")).unwrap();
        let canonical_root = normalize_data_path_text(&data);
        let owner_home = normalize_data_path_text(&home);

        // 陌生产品：拒绝迁移。
        std::fs::write(
            data.join(".deepx-data-root.json"),
            serde_json::to_vec_pretty(&DataRootMarker {
                format_version: 1,
                product: "OtherApp".into(),
                canonical_root: canonical_root.clone(),
                owner_home: owner_home.clone(),
                root_id: data_root_marker_id(&canonical_root, &owner_home),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!migrate_legacy_data_root_at(&data, &home));
        let raw = std::fs::read_to_string(data.join(".deepx-data-root.json")).unwrap();
        assert!(raw.contains("OtherApp"));

        // 同产品线但归属另一目录：不越权接管。
        std::fs::write(
            data.join(".deepx-data-root.json"),
            serde_json::to_vec_pretty(&DataRootMarker {
                format_version: 1,
                product: "DeepX".into(),
                canonical_root: "c:/other/place".into(),
                owner_home,
                root_id: String::new(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!migrate_legacy_data_root_at(&data, &home));
        let raw = std::fs::read_to_string(data.join(".deepx-data-root.json")).unwrap();
        assert!(raw.contains("DeepX"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn temp_root() -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "qaqh-winui-marker-test-{}-{nonce}",
            std::process::id()
        ))
    }
}
