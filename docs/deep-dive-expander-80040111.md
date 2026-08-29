# 深挖档案：Expander 模板激活冷路径与 0xc000027b / 80040111 崩溃族

> 关联事故档案：[`incident-fn15-stale-nodes-and-c000027b.md`](incident-fn15-stale-nodes-and-c000027b.md) §1.2（现象定案）、§4.2（dump 定责）、§4.4（源码深因，本文是其完整展开）。
> 分析日期：2026-08-29。源码基线：`F:\microsoft-ui-xaml-winui3-release-2.4.0`（下文相对路径均基于此）。

## 0. 一句话结论

**80040111（CLASS_E_CLASSNOTAVAILABLE）是 dxaml 对 TemplateSettings 族类型探测的「设计内静默失败」，不是注册缺口；真正致命的是它的后效——storyboard 关键帧绑定在冷窗口期解析不出值引发 fail-fast，fail-fast 携带的错误上下文恰是留存下来的 80040111。** 升级 WinAppSDK 无济于事（出货 2.4.0 已含全部容错设计），全 app 禁用 Expander 是最终缓解。

## 1. 版本对齐

| 对象 | 版本 | 证据 |
|---|---|---|
| app 打包运行时 | `Microsoft.WindowsAppSDK.Runtime 2.4.0` | `%LOCALAPPDATA%\windows-reactor-setup\temp\` 内 nupkg；build.rs 自包含部署 |
| MUX（`Microsoft.ui.xaml.dll`） | 3.2.0.2511 | 安装目录与 `target\release` 均同版本 |
| MUXC（`Microsoft.UI.Xaml.Controls.dll`） | 3.2.3.2608 | 同上；二进制含 `DllTryGetActivationFactory` 导出字符串 |
| 分析用源码 | `winui3-release-2.4.0` tag | 与运行时包同版本号，可视为同代实现 |

结论：**源码快照与出货二进制同代**——读到的容错逻辑就是线上实际逻辑，不是考古旧代码。二进制字符串检查（`DllTryGetActivationFactory` 出现于两个 DLL）独立佐证。

## 2. 触发链（源码逐环实锤）

### 2.1 模板关键帧用「真 Binding」取值

`src/controls/dev/Expander/Expander.xaml:42-43`（展开态）:

```xml
<DoubleAnimationUsingKeyFrames Storyboard.TargetName="ExpanderContent"
    Storyboard.TargetProperty="(UIElement.RenderTransform).(CompositeTransform.TranslateY)">
  <DiscreteDoubleKeyFrame KeyTime="0"
      Value="{Binding RelativeSource={RelativeSource TemplatedParent}, Path=TemplateSettings.ContentHeight}" />
```

同款写法还有 `:57`（SplineDoubleKeyFrame 过渡）、`:72`（`NegativeContentHeight`，折叠态）。关键点：这是运行时 `{Binding}`，不是编译期 `TemplateBinding`——值要在绑定引擎里按字符串路径解析。

### 2.2 OnApplyTemplate 内同步 GoToState（Expander 独有）

`src/controls/dev/Expander/Expander.cpp:31`（`OnApplyTemplate` 开头）→ `:80-81`（末尾）:

```cpp
UpdateExpandState(false);   // → VisualStateManager.GoToState(...)
UpdateExpandDirection(false);
```

模板尚未完全挂载进可视化树就切换视觉状态 → storyboard 进树 → 目标元素 `InheritanceContextChanged` → 关键帧的 `PropertyPathListener::ReConnect` 被迫**此刻**重连。这与 dump 中 stowed 原始栈逐帧吻合。

### 2.3 按名解析落到无 factory 的类型

重连要解析 `TemplateSettings`（Expander 的 DP）再解析 `ContentHeight`（**ExpanderTemplateSettings** 的 DP）。后者走 `TryGetDependencyPropertyByName`（`src/dxaml/xcp/components/metadata/MetadataAPI.cpp:1304`）：对非内置类型先 `RunClassConstructorIfNecessary()`（`:1331`）。

`ExpanderTemplateSettings` 的 IDL 声明（`src/controls/dev/Expander/Expander.idl:5-11`）与 `PersonPictureTemplateSettings` 完全同款——而后者被 MUXC 源码注释**点名**为无 activation factory 类型。

### 2.4 探测是「静默失败 + 层层回退」的设计

`ctl::GetActivationFactory`（`src/dxaml/xcp/components/com/inc/ComPtr.h:990-1024`）受 containment 门禁 `WINAPPSDK_CHANGEID_62676756` 控制：开 → `MuxGetActivationFactory` 快速路径（dump 栈中有 `MuxGetActivationFactoryImpl`，证明门禁已开）。

`MuxGetActivationFactoryImpl`（`src/dxaml/xcp/core/dll/xcpcore.cpp:398-515`）对 `Microsoft.UI.Xaml.Controls.*` 类型的顺序：

1. 探 MUXC，用**静默导出** `DllTryGetActivationFactory`（`:433`）；
2. 探 MUX 自身 `DllGetActivationFactory`（`:444` 附近，"Some Controls types live in MUX itself"）;
3. 回退 `RoGetActivationFactory`（函数尾部）。

MUXC 侧的静默导出（`src/controls/dev/dll/dllmain.cpp:286-330`）注释是本案最直接的书证：

> But some types, like "Microsoft.UI.Xaml.Controls.PersonPictureTemplateSettings", **don't have an activation factory**. In this case, **it's fine and expected** that no activation factory will be found. We just don't want to confuse the customer by raising a WinRT error.

### 2.5 类构造器路径是容错的

`RunClassConstructorIfNecessary`（`src/dxaml/xcp/core/metadata/ReflectionAPI.cpp:451-484`）:

```cpp
if (TypeKind_Metadata && SUCCEEDED(ctl::GetActivationFactory(...)))
    IGNOREHR(spManagedActivationFactory->RunClassConstructor());
else
    IGNOREHR(pXamlTypeNoRef->RunInitializer());   // 激活失败 → 走 IXamlType 兜底
m_flags |= MetaDataTypeInfoFlags::ExecutedClassConstructor;   // 失败也置缓存位（:478）
```

两个分支都 `IGNOREHR`，且**失败也会置 `ExecutedClassConstructor` 位**——同类型后续按名解析不再走激活路径。快照源码内**不存在**把 80040111 判为致命并向上传播的路径。

## 3. 假说裁决

| 假说 | 内容 | 裁决 |
|---|---|---|
| A：注册缺口 | vendored/出货 dxaml 缺 Mux 类型注册，激活冷路径必死，修 vendor 或升级可解 | **否决**。出货 2.4.0 已含静默探测+回退+容错全套设计；源码内无可致死路径 |
| B：时序竞态（后效致命） | 激活失败本身被吞；致命的是 storyboard Enter 时关键帧绑定解析不出值，动画路径 fail-fast，携带的上下文恰是探测链留存的 80040111 | **胜出**（最后一环——fail-fast 的确切调用者——仍属推断，需活体断点定罪） |

B 的证据支撑：dump 里 stowed 异常的原始栈顶是 `TryGetActivationFactoryFromModule+85`（探测现场），但快照源码中该 HR 被吞——矛盾的唯一自洽解释是 80040111 作为**线程上已起源的错误上下文**被后续 fail-fast（`RoFailFastWithErrorContext` 类机制）捞起打包，而非传播的返回值。

## 4. 非确定性机制（为何有时不崩）

1. **按类缓存**：`ExecutedClassConstructor` 置位后不再走激活路径（失败也置位）。第一个踩冷路径的 Expander 决定生死；任何更早的 Expander 实例化（设置页、其他 turn）都会焐热整链。
2. **DP 注册处理时机**：`TryGetDependencyPropertyByName` 内 `ProcessRegistrations()`（MetadataAPI.cpp:1339 附近）与首次 storyboard 进树的先后是竞态；注册先处理完则 DP 直接命中，绑定连接成功，无任何错误。
3. **会话形状**：resume 656638e0（28~30 turns，大量过程摘要块）时首轮 Measure 帧内多个 Expander 排队，冷命中概率被推满——这是 2026-08-29 从「偶发」变「6 秒必现」的原因。
4. 缓存是进程内的，每次重启重新掷骰——解释了 14:17 四连崩中 R3 崩 R4 活、以及一个装机版实例存活 20 分钟。

## 5. 风险面测绘（原生化改造红线）

扫描全部 MUXC 模板，**storyboard 关键帧**内用 `{Binding ...TemplateSettings.*}` 的控件族：

| 控件 | 关键帧绑定数 | 本 app 使用 | 风险评估 |
|---|---|---|---|
| **Expander** | 4 | 已禁用（3445a98） | **禁用维持**——唯一在 OnApplyTemplate 内同步 GoToState 的控件 |
| SplitView | 42/39 | 未用 | 转换期才跑 storyboard，后挂载时已热 |
| CommandBar / CommandBarFlyout | 68/36 | command_surface | 同上 |
| ProgressBar | 18 | 使用 | 同上 |
| NavigationView | 2 | 重度使用 | 同上（且绑定经 ElementName 指向内嵌 SplitView） |
| Reveal 材质 | 68 | 未用 | 同上 |

**红线**：上述控件可继续使用，但任何「Measure 期内模板重应用 / 状态 churn」的新用法会复刻 Expander 的冷窗口，引入前需重新评估。新折叠类 UI 一律走 tap header + 条件渲染（见 blocks.rs / info_panel.rs 现行实现），不引入 Expander。

## 6. 缓解现状与验证

- `3445a98`：blocks.rs 过程摘要、info_panel todo 卡的 Expander 换为 tap header + 条件渲染；设置页分组用 `settings_section_header` + 平铺 vstack。cargo test 83 全过。
- 两轮独立自动复现：resume 30 turns，90s/45s 存活、零新 dump（修复前 100% 六秒崩）。

## 7. 未闭环与重启条件

| 事项 | 状态 | 重启条件 |
|---|---|---|
| fail-fast 确切调用者定罪 | 推断（§3-B 最后一环） | 同族崩溃复发，或需要恢复 Expander 时：活体断点捕获 `RoFailFastWithErrorContext` 调用点 |
| 动画开关（reduced motion）对触发概率的影响 | 未验证 | 顺手实验：切 motion 设置跑自动复现，可再剥一层非确定性 |
| 上游 dxaml 的 storyboard-enter 对绑定失败的处理是否可改 | 未评估 | 若未来要向 microsoft-ui-xaml 上游提 issue/PR，需先补齐 §3-B 最后一环的实锤 |

## 8. 关键锚点速查

| 环节 | 锚点 |
|---|---|
| 关键帧 Binding | `Expander/Expander.xaml:42,43,57,72` |
| OnApplyTemplate 同步 GoToState | `Expander/Expander.cpp:31,80-81` |
| TemplateSettings 无 factory 书证 | `controls/dev/dll/dllmain.cpp:286-330` |
| 静默探测+回退 | `dxaml/xcp/core/dll/xcpcore.cpp:398-515`（探测 helper :326） |
| containment 门禁 | `dxaml/xcp/components/com/inc/ComPtr.h:990-1024` |
| 类构造器容错+缓存置位 | `dxaml/xcp/core/metadata/ReflectionAPI.cpp:451-484`（:478 置位） |
| 按名解析入口 | `dxaml/xcp/components/metadata/MetadataAPI.cpp:1304,1331` |
