# Composer 精简设计：单卡两区 + 强度滑柄 + 模型/强度入口

> 状态：**设计定稿，待实施**（批次 B 已解挂：vendor 冻结定案转自有组件维护，2026-08-29）· 2026-08-29
> 范围：`apps/winui/src/composer_bar/*` · 依赖登记：QAQ-Harness（effort RPC）
> 设计约束：遵循 Fluent Design 2 语汇；ZCode（Electron 参照物）仅吸收 composer 交互模式，不引入其视觉体系

---

## 1. 背景与问题

用户对现 composer 的三点反馈：**输入框偏高、控件冗余、model/effort 无入口**。

### 1.1 现状解剖（空态总高 ~200px，`composer_bar/view.rs` + 截图核对）

| 层 | 内容 | 现状值 | 问题 |
|---|---|---|---|
| 拖拽条 | 顶部横条 | ~16px 常驻显形 | 视觉噪音 |
| 输入区 | 卡中卡（外层卡 + 内层 TextBox 双边框） | TextBox 默认 **84px**（`mod.rs:39`） | 空态仅一行占位，显得空旷；双边框是冗余感来源（Fluent 2 输入基准 32px） |
| 工具行 | 📎 ▸ 工具模式`标准▼` ▸ `执行`（模式切换） ▸ ⤢（沉浸） ▸ 选择工作目录 ▸ 权限`L1▼` ▸ 发送 | 7 项 | 「选择工作目录」是一次性会话设置，不配常驻；「执行」纯文本似状态标签；`L1` 黑话无解释 |
| token 条 | 全宽进度线 + "11,168 tokens" | ~16px 独占一行 | 二级指标占据一级空间 |

### 1.2 控件身份对照（防误解）

| 界面文案 | 实际控件 | 数据通道 |
|---|---|---|
| `标准▼` | 工具模式五选一（标准/极限·8/极限·6/极限·4/创造） | `session.set_tool_mode` |
| `L1▼` | 权限级别 L1-L4 | `config.set_permission_level` |
| `执行` | 执行/规划模式切换 | 本地 toggle |
| —— | 模型选择 | **不存在**（仅右面板只读展示） |
| —— | 强度（effort） | **不存在**（client/daemon 均无 RPC） |

RPC 面现状：`config.load/save/set_permission_level` + `session.activity/list/set_tool_mode`。**无会话级 model/effort setter**。

## 2. 目标设计

```
改前（~200px）                        改后（~100px）
┌─ ⌶ 拖拽条 ─────────────┐          ┌─ ⌶(hover 显形) ─────────────────┐
│ ┌──────────────────┐  │          │ 向 QAQ-Harness 提问…             │
│ │ 向 QAQ-Harness    │  │          │                    (56px 空态)  │
│ │ 提问...  (84px)   │  │          ├─────────────────────────────────┤
│ └──────────────────┘  │          │ 📎 标准▾ ⌇   强度─●─低|MAX 权限L1▾ ➤│
│ 📎 标准▾ 执行 ⤢ 📁目录  L1▾ ➤│          ├─────────────────────────────────┤
│ ━━━━━━ 11,168 tokens ━━│          │ ━━━━━━━━━━━━━━ 11.2K ━━━━━━━━━━━ │
└────────────────────────┘          └─────────────────────────────────┘
```

高度预算：卡 padding 8 + 输入空态 56 + footer 36 + token 线 2 ≈ **102px**（改前 ~200px，降 50%）。

## 3. 实施批次

### 批次 A —— 纯前端（vendor 同期即可做）

| ID | 动作 | 细节 |
|---|---|---|
| A1 | **去卡中卡** | TextBox 去边框/独立背景，直接坐进圆角卡（radius 8）；`INPUT_DEFAULT_HEIGHT` 84→56，`INPUT_MIN_HEIGHT` 64→48，AUTO_MAX/MANUAL_MAX 180/360 不变；拖拽 resize 逻辑保留，clamp 语义改按「输入区高度」不变。**配轻投影**（ThemeShadow，Z 16-32，receiver 仅挂对话滚动区；需批次 B2 透出后启用——影子与卡片化同车，避免中间态） |
| A2 | **工作目录移出** | `composer-workspace` 按钮（view.rs:723）移至会话标题栏做 chip（选过显示短路径，现逻辑 `short_cwd` 直接复用）；空态卡上保留一次性入口 |
| A3 | **token 条降级** | 独占行 → 卡底 2px 贴边进度线；数字并到线右端 caption（"11.2K"），tooltip 显全量 |
| A4 | **权限语义 chip** | `L1▼` ComboBox → `权限 L1 ▾` MenuFlyout 四项带说明；**守卫等价迁移**：`rendered_pl == 0` 跳过逻辑（view.rs:622-627，Bug#2）必须在 menu 选择路径保持 |
| A5 | **执行/规划 toggle chip** | 文本按钮 → 图标+文 toggle chip，选中态有底色；备选：收进工具模式下拉尾部（默认不采） |
| A6 | **沉浸式角标** | ⤢ 移到卡右上角 hover 显形，footer 减负 |

### 批次 B —— vendor fork 补丁（已解挂：vendor 2026-08-29 冻结定案，转自有组件维护，见 vendor 仓库 VENDOR.md）

| ID | 动作 | 细节 |
|---|---|---|
| B1 | **Slider 刻度三件套透传** | `widgets/slider.rs` 加 `tick_frequency/tick_placement/snaps_to` 字段+builder → `generated.rs:680 slider_bindings` 推三条 Prop → mount 层照 `Prop::Step` 手写 arm（`backend/winui/mod.rs:2004`）加 `SetTickFrequency/SetTickPlacement/SetSnapsTo`。FFI 槽位已由 bindgen 投影（`bindings.rs:14565-14570`），**遵守 PLAN 禁手刻槽位惯例，不碰 bindings.rs**。估 60-80 行，纯加性 |
| B2 | **ThemeShadow widget 透出**（供 A1 配影） | FFI 已投影 `IThemeShadow`+工厂（`bindings.rs:16607`）与 `UIElement.SetShadow/SetTranslation`（`:18206,18236`）；widget 层（Border/通用 modifiers）加 elevation 字段 → mount 臂：创建 ThemeShadow、`SetShadow`、`Receivers.Add(transcript 滚动区)`、`SetTranslation(z)`。估 60-100 行。**全 app 仅 composer 一处用影**（层级信息量最大化）；InfoPanel 明确不加——停靠列的层级语言是底色/描边（Fluent 分层规范），非投影 |

补上后强度柄写法：`Slider::new(v).range(0,3).step(1).tick_frequency(1).tick_placement(Outside).snaps_to(Ticks)`。
（历史备注：曾因 vendor 上游同步挂起；2026-08-29 上游重写 reactor、同步链终止，vendor 冻结为自有组件，本批次解除挂起。）

### 批次 C —— 跨仓库依赖

| ID | 动作 | 依赖 |
|---|---|---|
| C1 | **强度滑柄**（footer 右簇，~120px，两端 caption 低/MAX，当前档位名实时刷新） | 前端先行：本地状态 + 乐观更新（同权限滑杆模式 `advanced.rs:184`）；**daemon 需新 RPC `session.set_effort`**，立 QAQ-Harness issue；RPC 就绪后换写路径，档位语义（4 档 snap / 连续 0-1）随 daemon 定 |
| C2 | **模型选择器** | 枚举 settings 已配置 models；单模型 → 只读 chip；多模型 → 下拉。**会话级切换语义需 daemon 确认**（现仅 config 全局 model）；同样先登记 issue |

## 4. 约束

1. **F-N15 红线**：无 Expander、无 OnApplyTemplate 期状态 churn（本设计全部用 Border/TextBox/MenuFlyout/Slider，均已在 app 有存活实证）。
2. **防护不回归**：Bug#2（权限程序化同步误写）与 BUG-017（tool_mode 空态 SelectionChanged 被吞）的守卫在控件形态变化时等价迁移。
3. Fluent 2：卡片 radius 8、subtle 按钮、MenuFlyout、Slider 原生模板；不引入 ZCode 的非 Fluent 视觉。
4. vendor patch 遵守「bindgen 辅助生成，禁手刻槽位」；且批次 B 必须等上游同步完成。

## 5. 测试清单

- [ ] A1：空态结构断言（TextBox 无自边框；卡内 padding 预算）+ 拖拽 resize 手动高度仍 clamp 360
- [ ] A2：标题栏 chip 选目录 → 会话 cwd 生效；composer 无残留按钮
- [ ] A4：权限 chip 四项选择回调序列与旧 ComboBox 等价；`rendered_pl==0` 时任何同步事件跳过（Bug#2 回归用例保留）
- [ ] A5/A6：toggle chip 状态切换；沉浸式角标 hover 显形、点击进入 360px 手动高度
- [ ] B1：vendor selftest（`reactor_selftest`）加 tick 三属性断言；app 侧档位吸附 on_value_changed 只发整数
- [ ] C1：滑柄乐观更新 + config 持久化；RPC 缺位时不崩、静默降级为本地态
- [ ] 全量 `cargo test -p qaqh-winui` 83+ 全过

## 6. 验收

- 空态 composer 总高 ≤110px（含 token 线与卡 padding）
- footer ≤5 项且全部语义可读（无裸黑话）
- 模型/强度入口存在（强度允许占位态：滑柄可见、写路径待 RPC）
- 自动复现验证无新增 crash bucket（LocalDumps 零新 dump）
