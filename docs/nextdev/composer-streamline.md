# Composer 精简设计：单卡两区 + 强度滑柄 + 模型/强度入口

> 状态：**批次 A 已落地，批次 B 已落地**（批次 C 跨仓依赖挂账）· 2026-08-29
> 范围：`apps/winui/src/composer_bar/*` + `apps/winui/src/header.rs` + `crates/qaqh-fluent` · 依赖登记：QAQ-Harness（effort RPC）
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

### 批次 A —— 纯前端（✅ 已落地 2026-08-29；偏差见下）

| ID | 动作 | 落地记录 |
|---|---|---|
| A1 | **去卡中卡** | ✅ `INPUT_DEFAULT_HEIGHT` 84→56、`INPUT_MIN_HEIGHT` 64→48（常量测试锁定）；TextBox 零边框 + `.background(LayerFill)` 直接坐进 `elevated_command_surface` 卡（新 Fluent 原语：LayerFill + radius 8 + `.elevation(16)`，批次 B2 ThemeShadow 落直接父面板）；自动高度公式 44+20n → 36+20n（单行=56）。**额外回收**：空态零高占位（status/queue/slash/error/attach 行）从 `grid(())` 改 `Element::Empty`——零高真实子节点会在 vstack spacing 两侧产生幻影空隙（Empty 不挂载原生节点），空态省 ~24-32px |
| A2 | **工作目录移出** | ✅ 标题栏（header.rs footer）恢复工作区 chip：Folder 图标 + `short_cwd` 短路径（完整路径 tooltip），点击走挂账的 `on_workspace` 合并流（组织工作区 create/select + 会话级 set 兜底）；workspace_error 红字提示随迁（无错不挂载）。composer 卡仅未选目录时保留一次性入口（cwd 空时显示，选后消失） |
| A3 | **token 条降级** | ✅ 4px 独立行 → 卡底 2px 贴边线（卡 padding bottom=0 + WinUI Border 子内容圆角裁切）；短计数 caption（`fmt_tokens_short`："11.2K"）与线同格叠放、渲染在线上（线即其下划线），行高 = caption 行高不新增独立行；全量千分位数字进 tooltip，分段 tooltip 保留 |
| A4 | **权限语义 chip** | ✅ `L1▼` ComboBox → `权限 L{n}` MenuFlyout（`PERMISSION_MENU` 四项带一句话说明，语义对齐 settings_view PERMISSION_LADDER）。**守卫等价迁移**：MenuFlyout 无程序化同步事件，`rendered_pl==0` 语义收敛为 `permission_change_allowed`（0 档拒绝一切写 + 同值跳过，测试锁定）+ pl==0 时按钮禁用（tooltip 外观一致地表达"配置未加载"） |
| A5 | **执行/规划 toggle chip** | ✅ 图标+文：执行=`Play` subtle / 规划=`List` accent（Fluent toggle 选中语言：非默认态 accent 底）；点击互切 spawn_set_mode 不变 |
| A6 | **沉浸式角标** | ⚠️ **偏差落地**：⤢ 移出 footer，并入顶部拖拽条右端（grip + ⤢ 同行 28px 条，两列并排不叠压）。**hover 显形未做**：需 `IsHitTestVisible`（隐形控件仍参与命中，会拦截输入区点击；vendor 未投影该属性）——vendor 冻结期为自有组件，后续按需补投影后改 hover 显形 |

落地后的空态卡高约 144px（原 ~200px+；验收目标 ≤110 未达——地板 = 顶部条 28 + footer 32 + 输入 56 + 卡 padding 8×2，其中顶部条因 A6 偏差比原 12px 反增 16px；进一步压高需 hover 显形或小号控件资源，均依赖 vendor 补投影）。footer 常态 6 项（附件/工具模式/模式 chip/弹性空白/权限/发送），语义全部可读。

### 批次 B —— vendor fork 补丁（✅ 已落地 2026-08-29，vendor `62385df`；B1+B2 随冻结基线入 VENDOR.md 补丁清单）

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

- [x] A1：高度常量 56/48 测试锁定（`a1_height_constants_meet_fluent2_baseline`）；TextBox 零边框 + LayerFill 同卡底色；拖拽 resize clamp 360 不变
- [x] A2：标题栏 chip（header-workspace）复用挂账 on_workspace 合并流；composer 仅空态保留入口（cwd 非空渲染 Element::Empty）
- [x] A4：`permission_menu_level` 四项解析 + 未知拒绝、`permission_change_allowed` Bug#2 守卫（rendered==0 拒绝/同值跳过）测试锁定
- [x] A5/A6：toggle chip 双态（accent/subtle）；⤢ 并入顶部条（hover 显形偏差已记录，待 vendor IsHitTestVisible）
- [x] A3：`fmt_tokens_short` 分档测试；分段/全量 tooltip 保留
- [x] B1：vendor selftest tick 三属性断言（`tests/slider_ticks.rs` 2/2）；app 侧档位吸附 on_value_changed 只发整数
- [ ] C1：滑柄乐观更新 + config 持久化；RPC 缺位时不崩、静默降级为本地态（批次 C，待 daemon RPC）
- [x] 全量 `cargo test -p qaqh-winui` 87 通过（原 83 + 新增 4）

## 6. 验收

- 空态 composer 总高：**~144px（未达 ≤110 目标）**——地板为顶部条 28 + footer 32 + 输入 56 + 卡 padding 16；压高手段（hover 显形顶部条、24px 小号按钮）均依赖 vendor 补 `IsHitTestVisible`/控件资源投影，已登记
- footer 常态 6 项且全部语义可读（无裸黑话：权限带档名，模式带图标）✅
- 模型/强度入口：批次 C（强度滑柄 UI 前端先行 + daemon `session.set_effort` 挂账；模型选择器待 daemon 语义）
- 自动复现验证无新增 crash bucket（LocalDumps 零新 dump）——本批全部使用 app 已有存活实证的控件形态（Border/TextBox/Button/MenuFlyout/Grid），无 Expander、无 OnApplyTemplate 期状态 churn（F-N15 红线遵守）
