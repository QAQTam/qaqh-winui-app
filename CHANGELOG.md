# Changelog

所有显著变更记录于此文件。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循语义化版本（当前处于 alpha，`version.txt` 为单一来源）。

## [Unreleased]

### Changed — 渲染管线（2026-08-24 会话）

- **默认字体换装 MiSans 可变字体**：打包 `MiSansVF.ttf`（wght 150–700 单轴连续），
  `DEFAULT_UI_FONT_FAMILY` 指向 `ms-appx:///Assets/fonts/MiSansVF.ttf#MiSans VF`。
  FontWeight 400/600/700 全部由 DWrite 按轴实例化为真实字重——此前 HarmonyOS Sans SC
  仅打包 Regular 单一静态面，bold/semibold 均为算法合成加粗（笔画发糊的根因）。
  Cascadia Mono/Code 保持不变。同步移除 HarmonyOS 字体与许可文件，
  `THIRD_PARTY_NOTICES.txt` 重写；MiSans 官方许可协议已随包补齐
  （`MISANS_LICENSE.txt`，取自官方发布页原文）。
- **段落间距体系落地**：`final_view` 弃用"多段合并单 RichTextBlock"渲染（XAML
  Paragraph 无 Margin 通道，段间距恒为 0），改为每段独立 RichTextBlock +
  vstack 统一段距 8（web 式均匀 margin 模型，对齐 marked→CSS 行为）。
- **标题层级差异化段前距**：`FinalBlock` 新增 `Heading { level, paragraph }`
  变体携带层级信息；h1 段前 16 / h2 14 / h3+ 12，段后统一 8——论文排版惯例
  "段前 > 段后"，标题与其后内容成组。
- 行内代码 CJK 回退链清理：移除死引用 `HarmonyOS Sans SC`（未系统安装，裸族名
  回退无意义），链尾落在系统 `Microsoft YaHei UI`。

### Fixed — 工具可见性（2026-08-24 会话）

- **补齐 MiSans 官方许可文本**：新增 `assets/fonts/MISANS_LICENSE.txt`
  （《MiSans 字体知识产权许可协议》官方原文，来源 hyperos.mi.com/font/download，
  附本应用三条合规说明）；THIRD_PARTY_NOTICES.txt 相应指向本地副本并修正
  分发条款表述。

- **长对话 XAML stowed 崩溃加固（F-N14）**：模型/工具输出中的 XML 非法
  控制字符（如 \x0B、\x00）流入 RichTextBlock/Run.Text 会以 c000027b
  stowed exception 整窗崩溃——流式分块越多概率越高。新增
  `sanitize_xaml_text` 净化器挂载于 transcript 全部文本入口（流式 delta、
  checkpoint、工具输出/参数、用户消息、diff 行），非法字符在进入存储前
  剥除；含回归测试。

- **布局动画补丁落地（F-N11 v1）**：vendor reactor 的
  `set_layout_animation` 从空壳变为真实现——XAML 布局驱动的 Size/Offset
  变化现由合成层隐式动画补间（`.with_layout_animation()`，300ms EaseOut，
  独占元素 ImplicitAnimations 集合；enter/exit 走独立 Show/Hide API 不受
  影响）。composition 包装层新增 `ImplicitAnimationCollection::remove`。
  新增 `motion_demo` bin 供验收（含 MOTION_MODE 自动化序列）。
  首版以 vector3 关键帧喂 Vector2 属性（Visual.Size）导致 stowed
  崩溃，已为 composition 包装层补全 Vector2KeyFrameAnimation
  （接口/类/vtable/工厂全链），自动验收全序列存活。spring 物理
  曲线待 NaturalMotionAnimation 包装扩展后跟进。

- **输入草稿持久化（F-N4，🔴→✅）**：草稿此前是 composer 组件内
  use_ref——切页面整树卸载即销毁、切会话有意重置，两条路都丢字。
  现在草稿快照上移 BridgeCore（HashMap<seed, Draft>，容量 32、非活跃
  逐出）：250ms 心跳写穿镜像 + 切会话存旧取新 + 发送确认双清；
  附件预览临时文件随条目生命周期存活。含往返与容量回归测试。

- **壳级返回入口（F-N1）**：进入 settings/skills 后标题栏左侧显示原生
  返回箭头（TitleBar back_button_visible），点击或 Alt+Left 回到上次
  会话（无会话则回首页）；chat/home 视图下加速键为 no-op。

- **Timeline reducer 缺块自愈（F-N7）**：BlockOpened 单帧在 SSE 间隙
  丢失时，该工具块的后续所有事件原本被静默丢弃（永久不可见）。现在
  ToolUpdated/ToolProgress/TextDelta/BlockCheckpoint 四类事件遇到缺失
  块时按 payload 合成占位块自愈（block_order=MAX 沉底避免撞序），
  含回归测试。

- **Diff 大文件折叠保护（F-N9）**：diff_file_view 原先全量急切渲染所有行
  （模型整文件重写 2000 行 = 2000+ 元素一次性上树）。现在超过 400 行只
  渲染前 400 + 「显示全部 N 行」按钮按需展开；折叠/展开状态在流式更新
  间保持。窗口判定抽为纯函数并含回归测试。

- **压缩终态可见（F-N3）**：CompactFinished 的 completed/failed 此前只写
  内部 map 无任何 UI（ring 消失即无状态，成败靠绿点反推）。现在标题栏
  压缩按钮旁显示终态 chip——completed 绿色「压缩完成 ✓」3 秒自动淡出
  （条件清除防竞态误删新压缩的 running），failed 红色「压缩失败 ✕」
  常驻至再次发起压缩；重发前自动重置上次终态。含回归测试。

- **修复断连后状态标签残留（F-N5）**：composer_snapshot 的 phase 此前
  裸拷贝存储值、无 stall 超时门控——SSE 断连时若正处于思考/作答中，
  「飞速思考中…/奋力回答中…」标签永久残留（重连也不修复）。现与
  is_streaming 同源门控，超时归 Idle；含回归测试。
- **移除幽灵用量统计按钮（F-N2）**：stats_open 全仓唯一消费者是按钮
  自身高亮（WebView 时代遗物）。删除 header 按钮/回调/标签、
  HeaderState.stats_open 字段、HeaderFlag::Stats 变体及 match 臂；
  /usage slash 文案改为指向 info 面板。

- **适配后端新工具 `read_image`**：创造模式默认工具表以 `read_image` 替换
  已废弃的 `image`（前 image_query）；工具卡语义标签归入「查看图片」。
  图像本体经 ToolResult.images 走消息层进模型视觉上下文，前端卡片显示
  归一化摘要（尺寸/格式）；卡内缩略图预览待 wire 扩展（PLAN F-N13）。

- **Diff 面板选择样式 Win11 化（F-N8）**：文件列表选中行弃用整块主题色背景，
  改为左侧 3px 圆角 accent 竖条（NavigationView 选择指示器语义）+ 中性浅灰
  行底；未选中态竖条与行底同色隐形占位，文本对齐零跳动。配套修复：edit
  工具家族 diff 的路径标签硬编码 "file"（后端 qaqh-workspace transaction.rs），
  现透传真实路径，drawer 文件名恢复可辨。

- **废除 V4-E「完成即回收」工具行策略（F-N6）**：read/grep/list 等只读工具完成后整行消失、
  write/edit/apply_patch 成功后仅剩「已修改 N 个文件」汇总卡——用户裁定可审计性优先于空间密度。
  现在所有工具调用在 chat 流内保留完整状态行（运行中转圈 / 完成 ✓ / 失败 ✕），文件修改类的
  diff 汇总卡照旧叠加；仅 Prepared 预览（未开始执行）继续抑制。`chat_view/tools.rs` 新增
  `tool_row_visible` 取代 `is_file_mutation/is_readonly` 门控，含回归测试。

### Added

- `markdown-winui`: `FinalBlock::Heading` 变体 + 层级信息测试
  （`heading_blocks_carry_level_for_spacing`）。

### Fixed

- 设置页字体说明文案与新默认字体对齐（「内置默认（MiSans）」）。

### 相关文档

- `docs/nextdev/tab-session-redesign.md` —— 会话标签条 key 化选中 +
  session.created 消费设计（泄露/丢失双修复，待实施）。
