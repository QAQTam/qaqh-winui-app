# 事故档案：F-N15 / T9 — 长对话 resume 渲染调度缺陷与 c000027b 崩溃族

> 记录日期：2026-08-24 · 状态：**A 方案止血完成**，深挖与装机版定责挂起
> 影响面：WinUI 前端（`apps/winui`）+ reactor vendor（`crates/libs/reactor`）

---

## 1. 现象（三层）

### 1.1 dev 构建崩溃 —— ✅ 已实锤已修
- **表现**：resume 一个长会话时进程闪退，事件查看器报 `c000027b`（stowed exception）。
- **复现率**：100% 确定性。同一批 **155 个脏节点 ID 序列逐次完全相同** → 排除竞态，判定为**确定性的渲染调度缺陷**。
- **触发条件**：大快照 reconcile。种子节点在 reconcile 时携带 state_dirty 标记进入渲染管线，但 `update_output` 的遍历没有覆盖到它们，标记遗留至帧末断言点。

### 1.2 装机版崩溃 —— ✅ 已实锤已修（2026-08-29 定案，见 §4.2）
- dev 的 assert 崩修复后，装机版仍会在 `Microsoft.ui.xaml.dll` 内部抛 stowed exception。
- **2026-08-29 定案**：LocalDumps 抓到转储后，`!analyze` 解出 stowed HRESULT = **0x80040111 CLASS_E_CLASSNOTAVAILABLE**。全链：`Expander::OnApplyTemplate`（Mux 控件库版）→ VSM `GoToState` → storyboard 进树 → `InheritanceContextChanged` → `BindingExpression` 重连（`PropertyPathListener::ReConnect`）→ `TryGetDependencyPropertyByName` → 类构造器 → `MuxGetActivationFactoryImpl` → **80040111** → stow → fail-fast。
- 触发条件：resume 大上下文会话（大量块进虚拟化 Measure），启动后无交互纯布局帧即崩；`TryGetDependencyPropertyByName` 的类构造器缓存造成非确定性（缓存热则存活）。
- 二分实验闭环：同一 daemon、同一恢复会话，仅去设置页 Expander 仍同 bucket 崩；全局搜实锤真凶为 **chat 工具块 Expander**（`chat_view/blocks.rs` 过程摘要折叠）与 **info 面板 todo 卡 Expander**。
- 处置：**全 app 停用原生 Expander**，折叠交互统一改为「tap header（`on_tapped`）+ 条件渲染」；设置页分组退化为 section header + 平铺 vstack。vendor metadata 链为何走 `GetActivationFactory<IManagedActivationFactory>` 冷路径失败，仍属未解深因（§4.4）。

### 1.3 卡顿 —— ⚠️ 定性观察，未量化
- 长对话滚动/交互变卡。最可疑上游：F-N6 废除工具行空间回收后，**工具行组件永生（常驻暴增）**。
- 这既是当时崩溃链的诱因，本身也是独立的性能负担。ItemsView 虚拟化曾评估为"性价比低"而搁置。
- 无节点数/帧率量化数据。

---

## 2. 因果链（dev 崩溃）

```
F-N6 废除工具行回收（产品决策：读写编辑全程可见）
  → 长对话常驻组件暴增
  → resume 大快照 reconcile
  → 种子节点的脏标记未被 update_output 消费（155 个 STALE）
  → reconciler/mod.rs:336 debug_assert fail-fast abort
  → c000027b
```

## 3. 已落地的修复

| 修复 | 内容 | 验证 |
|---|---|---|
| **A 方案（止血）** | `reconciler/mod.rs` ~L330：断言改为**消费式**——`take_state_dirty` 清除遗留标记放行，不再 fail-fast；附 `[F-N15] consumed N stale nodes` 日志 | 实测四波消费 145/69/3/2，零 panic，应用稳定运行 |
| F-N14 净化 | `markdown-winui` `sanitize_xaml_text`（保留 \t\n\r）挂六处文本入口 | markdown-winui 64 绿 |
| vendor Vector2 全链 | F-N11 动画案中发现 `Visual.Size` 是 Vector2，喂 vector3 关键帧 → E_INVALIDARG → stowed crash。补全 bindings_lifted 四处（接口 IID / 类结构 / ICompositor vtable typed 化 / 工厂方法） | motion_demo 自动序列 exit 0 |

## 4. 未闭环清单

### 4.1 T9-B：STALE 节点起源深挖（正确性问题，优先级最高）
A 方案只是防御。核心疑问未定论：
- **假设一（摘树残留）**：节点已从组件树摘除，但脏标记残留在集合里 → 无害，纯垃圾。
- **假设二（树上跳过）**：节点仍在树上，但 `update_output` 遍历路径跳过了它 → **真丢更新**，UI 会显示陈旧内容。

**判定方法（待执行）**：在消费点打印 STALE 节点的父链 + 组件类型。若能沿父链走通到根 → 假设二成立，需修 update_output 遍历覆盖。

### 4.2 装机版 stowed exception 定责 —— ✅ 已闭环（2026-08-29）
- LocalDumps（`QAQ-Harness.exe` + `qaqh-winui.exe` 双进程名键）→ 6 秒 100% 自动复现（启动即 resume）→ cdb `!analyze -v` 解出 0x80040111 全栈。
- 根因与处置见 §1.2。Expander 崩溃族就此关闭；后续新增折叠 UI 一律走 tap header + 条件渲染，不引入 Expander。

### 4.4 vendor metadata 链深因 —— 部分闭环（2026-08-29 dxaml 源码分析）

对照源码 `microsoft-ui-xaml winui3-release-2.4.0`（与 app 打包的 `Microsoft.WindowsAppSDK.Runtime 2.4.0` 同版本；MUX `Microsoft.ui.xaml.dll 3.2.0.2511`、MUXC `Microsoft.UI.Xaml.Controls.dll 3.2.3.2608`，二进制内均已含静默探测机制）：

**触发写法（实锤）**：Expander 模板 storyboard 关键帧用真 Binding 取值——
`<DiscreteDoubleKeyFrame Value="{Binding RelativeSource={RelativeSource TemplatedParent}, Path=TemplateSettings.ContentHeight}" />`。
且 `Expander.cpp` 的 **OnApplyTemplate 末尾同步调 `UpdateExpandState(false)` → GoToState**，即模板应用途中就要重连这条绑定 → `TryGetDependencyPropertyByName` 落到 `ExpanderTemplateSettings` 类。

**80040111 是设计内良性失败（实锤）**：MUXC 的 `DllTryGetActivationFactory`（dllmain.cpp）注释明说：TemplateSettings 族类型（点名 `PersonPictureTemplateSettings`）**没有 activation factory，探测返回 CLASS_E_CLASSNOTAVAILABLE 属预期**；MUX 侧 `MuxGetActivationFactoryImpl` 静默探测后回退，`RunClassConstructorIfNecessary`（ReflectionAPI.cpp）吞掉激活失败、退 `IXamlType.RunInitializer` 并置 `ExecutedClassConstructor` 缓存位。快照源码内无任何一路把 80040111 判为致命 → **升级 WinAppSDK 无济于事，Expander 禁用令维持为最终缓解**。

**根因精修（假说 B 胜出，最后一环仍属推断）**：崩溃不是激活失败本身，而是其后效——key frame 绑定在 storyboard Enter 时解析不出值（DP 注册处理 `ProcessRegistrations` 的时机 vs 首次 storyboard 进树的竞态），storyboard/绑定失败在动画路径上触发 fail-fast，而 fail-fast 携带的错误上下文正是探测链上 lingering 的 80040111。待定罪点：fail-fast 的确切调用者。

**非确定性来源（实锤）**：类构造器尝试按类缓存（失败也置位）；DP 注册处理时机与首次 storyboard 进树竞态；任何先前的 Expander 实例化都会把整链焐热。30 turns 大会话首轮 Measure 帧内大量 Expander 排队 → 冷命中概率被推满。

**改造红线（风险面测绘完成）**：MUXC 模板内在 **storyboard 关键帧**用 `{Binding ...TemplateSettings.*}` 的控件族——Expander、SplitView、ProgressBar、CommandBar/CommandBarFlyout、Reveal 材质、NavigationView（pane 过渡）。实践中只有 Expander 在 OnApplyTemplate 内同步 GoToState 才踩中冷窗口，其余仅在后挂载的状态转换时运行（此时已热）。规则：**禁用 Expander 维持不变**；上述其余控件可继续用，但若未来出现「模板重应用/Measure 期内状态churn」的用法需重新评估。

### 4.3 motion 目检疑云
- 派发链路日志确认正常（backend 收到配置、集合挂上），但人工目检报告"无动画硬变大"。
- 主假设：XAML 布局直写压制了补间值。
- 对策备选：Scale 补偿技巧 / 显式 StartAnimation / spring 物理曲线扩展（T8）。

## 5. 关键文件锚点

| 文件 | 锚点 |
|---|---|
| vendor `reactor/src/reconciler/mod.rs` | ~L330 A 方案消费逻辑；原 :336 debug_assert |
| vendor `reactor/src/backend/winui/mod.rs` | ~L1163 apply_layout_animation |
| vendor `composition/src/bindings_lifted.rs` | Vector2 全链四处 |
| 本仓 `crates/markdown-winui/src/block_transcript.rs` | 六入口净化 + 自愈 upsert（F-N7） |
| 本仓 `apps/winui/src/bin/motion_demo.rs` | 动画验证 demo（MOTION_MODE=offset/size/both） |

## 6. 教训

1. **Visual.Size 是 Vector2**——vector3 关键帧喂它必 E_INVALIDARG，且以 c000027b stowed 形式延迟爆出，栈面完全不指向真凶。
2. **debug_assert 是双刃剑**——fail-fast 让 bug 无处藏身（好），但发布链若混入 debug 断言会把可防御的问题变成必崩（坏）。A 方案的"消费+计数日志"模式值得推广：既不吞问题（有数字可查）也不炸进程。
3. **daemon 无日志 sink**（2026-08-24 补）：msgloop 所有 log::error 曾全部丢弃——排查压缩失败案时才发现，已补 `%USERPROFILE%\.deepx\qaqh-daemon.log` 文件 logger。
