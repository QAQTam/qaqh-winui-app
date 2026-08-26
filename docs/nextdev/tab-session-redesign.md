# 会话标签条重构设计：key 化选中 + session.created 消费

> 状态：设计定稿，待实施 · 2026-08-24
> 关联：QAQ-Harness `docs/PLAN.md` §6 issue#4（frontend-sync）、PR-2（H2 锁序）
> 症状：① 点"+"新建标签后正在工作的 ChatView 内容泄露到新会话；② 标签页丢失，
> 须点品牌标识回首页才找回。

---

## 1. 根因（三处独立缺陷叠加）

### D1 选择映射按 index 反查 seed —— 泄露的主因
`session_tabs.rs::on_selection_changed(index)` 在点击瞬间重新拉
`session_snapshot()` 按 `nth(index)` 换算 seed。TabView 渲染用的是组件本地
items（500ms 定时器喂），两次列表差一次 rev bump → `nth(index)` 指向另一会话
→ resume 错对象 → 其缓存/快照内容铺进 ChatView。
（`on_close_requested` 已有 key 回调，唯独 selection 没有——fork 缺口。）

### D2 新会话靠 15s 轮询 diff 猜 —— 时序污染源
`core_lifecycle.rs::spawn_new_session` 丢弃 Ack，每 500ms 刷新会话列表取
"不在 before 集合的 seed"。期间任何并发新建（远端/子代理/他窗口）都会被误认；
且检测完成前 active_seed 不变，新标签已出现而选中仍停留旧标签。

### D3 workspace 双重过滤 —— "丢失"的机制
标签可见性 = `!archived && ws_match(current_workspace)`；selected 只在可见集
合内找。新建会话按 cwd 归属 workspace 与当前视图分组不符 → 新标签不可见 +
selected=-1（D3 注释自认的降级态）。从首页开始页重进会把 workspace 上下文对
齐回来——这正是"点品牌标识能找回"的原因。

## 2. 方案

### F1 fork TabView：selection 回调携带稳定 key（vendor 补丁）
reactor widget `tab_view.rs` 的 selection 事件增加 key 出参（与
`on_close_requested(key)` 对齐）。应用侧删除 index→nth 反查，直接
`spawn_resume(&key)`。**错投类泄露从机制上消失**（key = seed 本身）。

### F2 bridge 消费 `session.created` 事件（无需协议改动）
daemon 已在 lease attach 后发布（ringing_http.rs，`publish_session_created`）。
bridge 增加一次性 `pending_new_session` 标记：
- 点"+"→ 置标记（替代整个 30×500ms 轮询任务）
- 收到 session.created 且标记在 → `set_active_seed(seed)` +
  workspace 对齐（F3）+ 清标记；超时兜底 15s 后清除并提示失败

可选增强（依赖后端 PR-2 附带）：`RingingCommandAck.data.seed` 加法兼容字段，
Ack 直返比 SSE 更快，作为首选路径、事件作兜底。

### F3 workspace 对齐规则
创建成功后 `current_workspace := 新会话.workspace_id`，保证新标签立即可见。
若用户正处于其他分组视图，改为跟随跳转（与 VS Code「新建文件落在当前工作区」
心智一致），不再产生不可见标签。

### F4 降级态收紧
active 不在可见集合时不再 selected=-1 裸奔：临时放宽过滤显示全部非归档标签
并高亮 active（防御性，F1-F3 落地后理论上不可达）。

## 3. 实施顺序与依赖

| 步骤 | 内容 | 依赖 | 规模 |
|---|---|---|---|
| S1 | F1 vendor TabView key 回调 | 无（fork 自主） | 小 |
| S2 | 应用层换用 key 选中 + 删 nth 反查 | S1 | 小 |
| S3 | F2 bridge 事件消费替换轮询 | 无（事件已存在） | 中 |
| S4 | F3+F4 workspace 对齐与降级收紧 | S2/S3 | 小 |
| S5* | Ack.data.seed 首选路径 | 后端 PR-2 附带（可选） | 小 |

S1-S4 与后端 PLAN 零阻塞，可立即开工；唯一跨仓库动作是 H2 保持在 PR-2
（同一链路的后端半边：close_current:false 绕过引擎重置）。

## 4. 测试清单

- 渲染列表与点击间插入 rev bump → 选中的仍是点击的 tab（S1/S2 回归）
- 连续快速点"+"两次 → 恰好一个新会话/两个各自成 tab，无错投
- 创建期间旧会话持续输出 → 旧内容不出现在新 transcript
- cwd 归属 W2 时在未分组视图新建 → 新标签可见且选中（S4 回归）
- session.created 迟到 >15s → 兜底提示，无僵尸标记

## 5. 退役衔接

本设计落地后即满足侧栏会话管理退役条件：导航职责回归 NavigationView
（首页/技能/设置），文档生命周期归标签条，历史/归档浏览归首页承接。
