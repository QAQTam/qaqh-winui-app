# qaqh-winui-app 第一轮前端审查修复计划（PLAN）

> 状态：**待复核**（本计划由 2026-08-23/24 渲染管线专项审查与壳层问题报告产出，全部条目为主代理
> 直接实读代码验证，file:line 均基于当前 main；执行前建议按 Step 0 惯例快速复核）
> 审查日期：2026-08-24 · 分支基线：当前 main · 范围：apps/winui + crates/markdown-winui + vendor 补丁面
> 契约约束：前端不产生 wire 变更；跨仓库耦合项以 QAQ-Harness `docs/PLAN.md` §6 issue 登记为准

---

## 0. 总览

| 指标 | 数值 |
|---|---|
| 发现总数 | **20**（🔴高 3 / 🟠中 8 / 🟢低 9） |
| 批次 | P0 错投/数据丢失 3 项 · P1 正确性 8 项 · P2 清扫 9 项 |
| 增强线（非 bug，独立排期） | 设置页原生化 / 斜杠命令 / FontIcon+头像铺设 |
| 需后端协同 | 1 项锁序（H2 保持 PR-2）+ 1 项可选（Ack.data.seed），均已登记 QAQ-Harness PLAN §6#4 |
| 预估工作量 | ~7 天（不含增强线） |

严重度定义：🔴 内容错投/状态机破坏；🟠 特定时序触发的渲染错误、语义丢失、体验缺陷；🟢 卫生与视觉打磨。

---

## 1. 发现清单（按模块）

### 1.1 会话导航（4 项）——设计详见 `docs/nextdev/tab-session-redesign.md`

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-T1 | `session_tabs.rs:149` + fork `tab_view.rs` | selection 按 index 反查 seed：点击瞬间重拉快照 `nth(index)`，与渲染列表差一次 rev bump 即错投会话——正在工作的 ChatView 内容"泄露"进新标签。`on_close_requested` 已有 key 回调，selection 缺失 | 🔴 | P0 |
| F-T2 | `bridge/core_lifecycle.rs:65` | 新会话靠丢弃 Ack 后 15s 轮询 diff 会话列表猜测：并发新建（远端/子代理）即误认；检测窗口内 active 不变、新标签已现，选中态悬空。daemon 已发布 `session.created`（ringing_http.rs lease attach 后）但 UI 从未消费 | 🔴 | P0 |
| F-T3 | `session_tabs.rs:119` | workspace 双重过滤致标签不可见：新建会话按 cwd 归属与当前视图分组不符 → 标签隐藏 + selected=-1；从首页重进对齐上下文才"找回" | 🟠 | P1 |
| F-T4 | `session_tabs.rs:102` | active_seed 变化不伴随 sessions rev bump 时选中不同步（set_active 只在 rev 分支内调用），最长滞后一个轮询周期且可能停在失效标签上 | 🟠 | P1 |

### 1.2 markdown 渲染管线（7 项）

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-R1 | `round_renderer/answer.rs:52,80` + `chat_view/blocks.rs live_view` | 流式期间显示原始 markdown 字面（`parse_live` 结果已计算但视图只渲染 `LiveSegment::Text` 纯 TextBlock）；turn 结束瞬间跳变为样式化 Final 视图，布局跳动 | 🟠 | P1 |
| F-R2 | `chat_view/blocks.rs:136` | 推理摘要正文整段 `is_italic = true`：打包字体无 italic 面 → CJK 合成斜切，观感差；应改次级文字色区分 | 🟠 | P1 |
| F-R3 | fork `backend/winui/mod.rs:309-345 build_run_inline` | 删除线静默丢失：markdown-core 已解析 `is_strikethrough`，fork run 构建不消费该字段 → `~~text~~` 渲染为普通文本，语义误导。需补 bindings（ITextElement 文本装饰通道） | 🟠 | P1 |
| F-R4 | fork `backend/winui/mod.rs:351-364` | 链接不可辨识不可点击：Hyperlink 映射纯 Run（无下划线/颜色/NavigateUri）——文档已登记的 fork 缺口，需补绑定或 HyperlinkButton 通道 | 🟠 | P1 |
| F-R5 | `markdown-winui/lib.rs:248 blocks_to_rich` | 嵌套列表降级为纯文本堆：嵌套 List 走 `block_plain_text` 兜底，缩进层级全丢 | 🟢 | P2 |
| F-R6 | `markdown-winui/lib.rs render_final Rule` | 分隔线渲染为空段落（隐形）："分隔线样式由上层处理"但上层从未实现；占一行空白还误导阅读节奏 | 🟢 | P2 |
| F-R7 | `markdown-winui/lib.rs render_final Quote` | 引用块仅 "> " 文本前缀，无边线/底色视觉语义 | 🟢 | P2 |

### 1.3 字体与合规（2 项）

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-F1 | `shell_store` config 迁移路径 | 存量用户 config 中 `font_family="HarmonyOS Sans SC"` 解析失败落系统回退链（Segoe/雅黑），观感不一致且无提示；应映射旧值 → 内置默认 | 🟢 | P2 |
| F-F2 | `assets/fonts/THIRD_PARTY_NOTICES.txt` | MiSans 官方许可文本未随包分发（zip 内无 license 文件，notices 已标注待补）——合规缺口 | ✅已修 | P2 |

### 1.4 卫生（3 项）

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-D1 | `markdown-winui/lib.rs` 头注释 | "RichTextRun.font_size/is_italic/is_strikethrough 后端尚未消费"已过时：前两者 fork 已消费，仅 strikethrough 属实（F-R3）。过时认知曾误导"标题不加粗"决策 | 🟢 | P2 |
| F-R8 | 行内代码渲染 | 无背景色标识（平台限制：WinUI Run 无 Background 属性）。记录为已知限制；备选方案（Border 包装/InlineContainer）另行评估 | 🟢 | 记录性 |
| F-R9 | 任务列表渲染 | ☑/☐ 字形来自回退字体，字重风格与正文（MiSans）不匹配；可换 Segoe Fluent Icons Checkboxes glyph 或自绘 | 🟢 | P2 |

### 1.5 壳层导航与输入态（4 项）——2026-08-24 第二批用户报告

### 1.6 流式状态、工具可见性、Diff 面板与动效基建（8 项）——2026-08-24 第三批用户报告（已实读代码定根因）

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-N5 | `bridge/core_interaction.rs` composer_snapshot（~L334） | 断连后 phase 标签残留：`state.is_streaming` 走 `COMPOSER_STALL_TIMEOUT_MS`(types.rs:665, 4min) 门控，但 `state.phase = activity.map(\|a\| a.phase)` **裸拷贝无门控**——SSE 断连时若正处于 thinking/answering，delta 停更后 4min Stop 按钮恢复，但「飞速思考中…/奋力回答中…」（composer_bar/status.rs:51-53）永久残留；重连也不修复（turn 已在别处终结，Ended 永不到达）。已落：composer_snapshot L333-341 同源门控（含 stall 超时场景回归测试 composer_phase_goes_idle_when_streaming_stalls） | ✅已修 | P1 |
| F-N6 | 实为 `chat_view/tools.rs:69-112` V4-E 显示策略 + `blocks.rs:58-66` 可见性门控（初判传输丢帧系误诊）| 部分工具调用不显示：**根因是 V4-E 空间回收策略本身**——文件修改类（write/edit/apply_patch）成功完成态不渲染独立行（由「已修改 N 个文件」diff 汇总卡承担），只读类（read/grep/list/web_search）完成即整行回收。用户裁定可审计性 > 空间密度，策略废除。修复：`is_file_mutation/is_readonly` 删除，新增 `tool_row_visible`（仅抑制 Prepared 预览），运行中转圈/完成✓/失败✕ 一律保留状态行，diff 汇总卡照旧叠加；回归测试 `completed_tool_rows_stay_visible_after_v4e_repeal`。**附带发现（转 F-N7）**：reducer 对缺 BlockOpened 的工具事件静默丢弃无自愈——SSE 单帧丢失场景仍会黑洞化 | ✅已修 | P1 |
| F-N7 | `markdown-winui/block_transcript.rs` apply_entry（~L486 ToolUpdated / ~L517 ToolProgress）| reducer 加固：工具事件在 `block_mut()` None 时静默丢弃且无自愈路径（对比活动追踪有 Touched、文本 checkpoint 全量覆盖）；发射端 parse.rs:57 为每个 tool_call 开块依赖单帧可靠送达（timeline_tools_open 去重）。防御性修复：缺块时按 payload upsert 合成块。已落：ensure_block 助手（缺块合成占位，block_order=MAX 沉底）挂入 ToolUpdated/ToolProgress/TextDelta/BlockCheckpoint 四臂；回归测试 tool_updated_self_heals_missing_block_opened | ✅已修 | P2 |
| F-N8 | diff 卡裸 "file"：后端 `QAQ-Harness/crates/qaqh-workspace/src/edit/transaction.rs:221,259` 硬编码；选中态整块主题色：`apps/winui/src/diff_drawer.rs` drawer_content | ① edit 工具家族 `unified_diff(content, &edited, "file")` 把字面量当路径标签（write 家族传真路径，故只有 edit 中招）→ run_edit 增加 path 参数全链透传（handler.rs raw_path→transaction 两处→测试三处）；② 文件列表选中行 AccentSecondary 整块刷底 → 改 Win11 NavigationView 式：3px 圆角 Accent 左竖条 （未选中与行底同色隐形占位）+ SubtleFill 中性浅灰底；竖条画法复用 header.rs divider 先例 border(text_block("")) | ✅已修 | P1 |
| F-N9 | `markdown-winui/src/tool_content.rs:792-804` diff_file_view | Diff 大文件性能隐患：ScrollViewer→vstack **全量急切渲染零虚拟化**——max_height(520) 只裁视口不裁元素树，模型整文件重写 2000 行 = 2000+ XAML 元素一次性上树（UI 线程布局卡顿）；该组件在聊天工具卡与 drawer 双处复用，多卡片累积更糟。已落：tool_content.rs DIFF_VIEW_MAX_ROWS=400 + DiffRowsProps 组件（expanded 状态跨重渲染保持）+ 窗口纯函数 diff_rows_window 含回归测试；聊天卡与 drawer 双处生效，展开按钮按需全量 | ✅已修 | P1 |
| F-N10 | 同文件 unified_diff_row / parse 管线 | 行内词级 diff 高亮（GitHub 式行内红绿细分）：**前端本地**用 similar crate 对成对增删行做词级对齐着色，零 wire/envelope 变更（符合前端 PR wire 自查纪律；后端 qaqh-workspace/file_shared.rs 已用 similar 出 unified_diff，算法同源）。纯观感增强，排在 F-N9 之后（先防卡顿再谈精致） | 🟢 | P2 |
| F-N11 | vendor `reactor/src/style.rs` AnimationConfig + `backend/winui/mod.rs:3740 set_layout_animation` 空壳 stub | 动效基建债：AnimationConfig 已接合成层 opacity/scale/translation（fade/slide 即插即用），但 **size 维度未暴露**、布局动画 API（with_layout_animation，含 spring damping_ratio/period）后端为空实现——todo 重排序动画/灵动岛式 size morph 均被此卡住。修复：AnimationConfig 增加 size 字段走 ElementCompositionPreview 对 Visual.Size 启动 KeyFrame；顺带填 set_layout_animation（同一 ImplicitAnimations 机制）。一次补丁全库列表型 UI 受益，走 dev-downstream 流水线。已落（v1）：vendor 三处——composition/animation.rs ImplicitAnimationCollection::remove、reactor backend/winui/mod.rs apply_layout_animation（Size/Offset 表达式帧隐式补间，独占集合语义）+ stub 替换为真实现（reconciler 派发路径原本就绪）。demo：`cargo run -p qaqh-winui --bin motion_demo`（支持 MOTION_MODE=size|offset|none|both 自动验收序列，7s 自验退出码判据）。**根因补记**：首版崩溃（c000027b stowed + E_INVALIDARG）= Visual.Size 是 **Vector2** 而实现用了 vector3 关键帧；已按官方文档确认后为 composition 包装层补全 Vector2KeyFrameAnimation 全链（bindings_lifted 接口/类/vtable 槽位 + compositor 工厂 + Animation 实现）。自动验收 both 模式 SURVIVED。遗留：spring 曲线需 NaturalMotionAnimation 包装扩展 | ✅已修 | P1 |
| F-N12 | 后端新工具 `read_image`（QAQ-Harness qaqh-workspace/src/read_image/）前端集成 | 已完成：tool_action 语义标签「查看图片」（tools.rs:28）+ CUSTOM_MODE_DEFAULT_TOOLS 陈旧 `image` 条目替换为 `read_image`（composer_bar/mod.rs:80）。**遗留**：TimelineTool 无 images 字段，聊天卡片只能显示文本摘要（"Image read successfully: … W×H …"），无法缩略图预览——需 wire 层扩展（TimelineTool.images 或 ToolBody::Image），涉 envelope 变更另案评估 | ✅部分（缩略图转 F-N13） | P1 |
| F-N13 | `markdown-winui` ToolBody + TimelineTool wire | read_image 缩略图渲染：聊天卡片内直接预览已读图像（现仅文本摘要）。需 envelope 扩展（TimelineTool.images 或 ToolBody::Image 变体），走 RFC/后端协同；UI 侧 Image 控件 + 圆角卡片样式已备 | 🟢 | P2 |
| F-N14 | `markdown-winui` 全部文本→XAML 入口（apply_entry 四臂/parse_unified_diff/turn user_text） | 长对话随机 c000027b（XAML stowed，Microsoft.ui.xaml.dll）：模型/工具输出中的 XML 非法控制字符（\x00-\x08、\x0B、\x0C、\x0E-\x1F 等）流入 Run.Text 即抛 E_INVALIDARG；长对话=更多分块=概率累积。已落：`sanitize_xaml_text` 净化器（保留 \t\n\r 与合法区段）挂入 TextDelta/BlockCheckpoint/ToolProgress/TurnOpened/ToolUpdated card 字段/diff 行渲染六处入口；回归测试 streaming_delta_strips_xml_illegal_control_chars。**取证遗留**：WER dump 被系统即时清理未能提取 stowed HRESULT 原值，已请用户配置 LocalDumps 全量转储（HKLM 需提权）以便下次崩溃精确定位 | ✅已修（防御性，待复现验证） | P0→P1 |

| ID | 位置 | 问题摘要 | 级别 | 批次 |
|---|---|---|---|---|
| F-N4 | `composer_bar/view.rs:20`（Draft 存 use_ref）+ main.rs 视图卸载模型 | 输入草稿两路丢失：①切页面 composer 整树卸载，use_ref 态随子树销毁；②切标签 seed 变化有意重置（mod.rs:15 设计注释明示）——单槽草稿设计缺陷。接近用户数据丢失。已落：BridgeCore.composer_drafts（HashMap<seed,Draft>，上限 32 非活跃逐出）+ 250ms 轮询写穿镜像 + seed 切换存旧取新 + sendAck 双清；预览 %TEMP% 文件随条目存活。回归测试 composer_draft_roundtrip_and_eviction | ✅已修 | P0 |
| F-N1 | `settings_view/view.rs:319 back_button_visible(false)` + bridge 无历史栈 | 进入 settings/skills 后无显著返回入口：NavigationView 返回键被显式禁用；current_view 为扁平 String 无历史栈；唯一出口是侧栏。应提供壳级返回（→ chat + last_active_seed）。已落：发现 TitleBar 原生 back_button_visible/on_back_requested API（settings_view:319 显式关闭的就是它）——header 按视图启用原生返回箭头 + Alt+Left 加速键（current_view_name 守卫，chat/home 下 no-op）；返回目标 active_seed→chat / 空→home | ✅已修 | P1 |
| F-N3 | `core_client.rs:431 CompactFinished` + `core_state.rs:119` | 压缩终态有数据无 UI：CompactFinished 的 completed/failed 已存入 compact_statuses 但无任何渲染消费（compacting 只判 ==running）→ ring 消失即无状态，成败不可辨，用户只能靠绿点变灰反推。已落：HeaderState.compact_result 透传（core_state refresh_header 过滤 running）+ header chip（completed 绿 3s 条件清除 / failed 红常驻）+ 重发压缩前重置 + 回归测试 compact_terminal_result_surfaces_in_header | ✅已修 | P1 |
| F-N2 | `header.rs:158 on_stats` + types.rs stats_open | 幽灵用量统计按钮：stats_open 全仓库唯一消费者是按钮自身高亮（WebView 时代驱动 Web 用量图表，移除后空转）；真实用量已在 info_panel 展示。已移除：header.rs 按钮/回调/标签 + types.rs stats_open 字段与 HeaderFlag::Stats 变体 + core_state.rs match 臂 + /usage 文案指向 info 面板（6 文件 9 处） | ✅已修 | P2 |
*干净区：timeline drain 的 seed 过滤在出队时刻生效且有测试锁定（tests.rs:195）；
chat_view 会话切换的缓存 mem::take 对称零拷贝；vsync 泵 panic 防护完备；
turn 级 memo 使终态切换重渲染粒度可控；高亮 LRU 缓存命中路径零 lexing。*

---

## 2. 修复批次

### P0 —— 内容错投（会话导航）

| ID | 修复方向 | 回归测试 |
|---|---|---|
| F-T1 | fork TabView selection 回调增加稳定 key 出参（对齐 close_requested）；应用侧删除 nth 反查直用 key | 渲染列表与点击间插入 rev bump → 选中仍是所点 tab |
| F-T2 | bridge 置一次性 pending 标记替代轮询任务；消费 `session.created` → set_active_seed + workspace 对齐；15s 兜底清除并提示 | 连续两次"+"→ 各自成 tab 无错投；创建期间旧会话持续输出不污染新 transcript |
| F-N4 | 草稿存储上移 bridge core（composer_drafts per-seed HashMap，容量上限对齐 session_cache 模式）；seed 切换语义改存旧取新；sendAck 只清当前 seed；附件预览临时文件生命周期随草稿 | 切页面/切标签往返草稿保留；发送后仅当前 seed 清空 |
### P1 —— 渲染正确性

| ID | 修复方向一句话 | 测试 |
|---|---|---|
| F-T3 | 创建成功后 current_workspace := 新会话归属；active 不可见时放宽过滤而非 selected=-1 | cwd 归属 W2 时在未分组视图新建 → 标签可见且选中 |
| F-T4 | active 同步移出 rev 门控（每 tick 廉价比较即可） | seed 直变（无列表变更）→ 一周期内选中跟随 |
| F-R1 | live 视图消费既有 parse_live inlines 渲染富文本（memo 粒度按 segment）| 流式期间 **bold** 即时加粗；终态无布局跳变 |
| F-R2 | 推理摘要去 is_italic，改 SecondaryText 前景色区分 | 目视 + 无合成斜体 |
| F-R3 | fork bindings 补 Strikethrough 通道（bindgen 辅助生成，禁手刻槽位） | ~~x~~ 渲染带删除线 |
| F-R4 | fork Hyperlink 通道补 Underline+AccentForeground+NavigateUri | 链接可辨识可点击 |
| F-N1 | header 标题左侧壳级 BackButton（view∈{settings,skills} 时显示）：navigate("chat", last_active_seed)；Alt+Left 加速键 | 设置页一键回工作对话 |
| F-N3 | compact 位终态 chip：completed 压缩完成✓ 3s 淡出 / failed 压缩失败常驻至下次操作；数据源用既有 compact_statuses 终态值 | 完成与失败可辨识，无需看绿点反推 |
### P2 —— 清扫

| 批 | 内容 |
|---|---|
| L-render | F-R5 嵌套列表递归渲染（缩进 padding 递增）· F-R6 Rule 用 Border 高 1px 主题分隔线 · F-R7 引用块左缘 accent 条 + 底色 |
| L-font | F-F1 存量字体名迁移映射 · F-F2 补 MiSans 许可文本随包 |
| L-doc | F-D1 头注释更正 · F-R9 任务列表 glyph 统一 |
| L-shell | F-N2 移除幽灵统计按钮（on_stats / HeaderFlag::Stats / stats_open 字段）+ slash 表 /usage 文案修正指向 info 面板 |
### E —— 增强线（独立排期，非 bug）

设置页原生化（ToggleSwitch/NumberBox/PasswordBox/InfoBar；~~Expander 卡片骨架~~ F-N15 定案全 app 禁用 Expander，分组改 settings_section_header + 平铺 vstack）·
Composer 精简 + 强度滑柄 + 模型/强度入口（设计定稿见 `docs/nextdev/composer-streamline.md`；批次 B vendor 补丁挂起待上游同步）·
全局 FontIcon 化 + PersonPicture 头像 + **AnimatedIcon 微动画**（hover 齿轮/放大镜/返回箭头；vendor 需从零投影 AnimatedIcon+AnimatedVisuals 通道 ~150-250 行，PointerHandlers 已备 hover 事件；与 Slider 刻度同批，待 vendor 同步）·
Composer AutoSuggestBox 斜杠命令 ·
Ctrl+K 命令面板。实施时各自立 RFC。

---

## 3. 跨项联合设计点

1. **Tab 状态机簇（F-T1~T4）**：key 化选中是其余三项的前提；T3/T4 在 key 化后
   表现面收敛，必须同 PR 序列落地（S1→S2→S3→S4，见 redesign doc §3）。
2. **run 属性契约一次到位（F-R3+F-R4）**：fork bindings 缺口补全时同步盘点
   RichTextRun 全部字段（含未来 underline/color 需求），按 WinMD 生成清单一次
   生成——避免逐属性打补丁反复动 vendor 快照。
3. **H2 锁序（跨仓库）**：F-T2 的后端半边（SessionCreate{close_current:false}
   绕过引擎重置）= QAQ-Harness PLAN H2，保持 PR-2 不顺延；前后端各半同时上线。
4. **bridge 持有 per-seed UI 态簇（F-T2+F-N4）**：pending_new_session 标记与
   composer_drafts 同属"组件卸载后仍需存活"的壳级状态，统一走 BridgeCore
   HashMap+rev 模式（对齐 session_cache/transcript 缓存先例）；同批设计、可分 PR。
---

## 4. gh issue 清单（随对应 PR 创建）

1. `[frontend] TabView selection 携带稳定 key（vendor 补丁 S1）` — 随 PR-F1
2. `[frontend] 消费 session.created 替换新建会话轮询（S3）` — 随 PR-F2，关联 QAQ-Harness PR-2/H2 与其 PLAN §6#4
3. `[frontend] 富文本 run 属性契约补全：strikethrough + hyperlink（S-fork）` — 随 PR-F4
4. `[frontend] Composer 草稿持久化：per-seed 存储 + 页面切换存活（F-N4）` — 随 PR-F7
5. `[frontend] 壳级返回键 + 压缩终态反馈 + 移除幽灵统计按钮（F-N1/N2/N3）` — 随 PR-F8
> 后端侧协同项已在 QAQ-Harness `docs/PLAN.md` §6 登记（#4），此处不重复开票。

---

## 5. 执行顺序（PR 划分）

| 顺序 | PR | 内容 | 预估 |
|---|---|---|---|
| Step 0 | 快速复核 | 本表 file:line 逐条过一遍（多数为本会话实读，成本低） | 0.5 天 |
| PR-F1 | Tab key 化 | F-T1（fork widget + 应用接入）| 1 天 |
| PR-F2 | 新建会话事件化 | F-T2 + F-T3 + F-T4（bridge + tabs） | 1 天 |
| PR-F3 | 渲染修正簇 | F-R2 + F-R5 + F-R6 + F-R7 + F-D1 | 1 天 |
| PR-F4 | run 属性契约 | F-R3 + F-R4（fork bindgen 辅助） | 1 天 |
| PR-F5 | 流式富文本 | F-R1 | 1 天 |
| PR-F6 | 卫生批 | F-F1 + F-F2 + F-R9 | 0.5 天 |
| PR-F7 | 输入态持久化 | F-N4（composer_drafts 上移 + seed 语义改造，与 F-T2 同批设计） | 1 天 |
| PR-F8 | header 导航/反馈簇 | F-N1 返回键 + F-N3 压缩 chip + F-N2 删幽灵按钮 | 1 天 || 增强线 | 独立排期 | 设置页原生化优先（与后端大砍刀零耦合） | 另计 |

---

## 6. 验证策略

**每 PR 强制**：回归测试先行失败 → 修复后通过；`cargo test --workspace` +
`cargo clippy --workspace --all-targets` 干净（workspace lints：unwrap_used /
string_slice deny）。涉及 vendor 补丁的 PR（PR-F1/F4）走 dev-downstream
本地验证 → off → rev bump 流水线。

**专项**：
- Tab 时序用例矩阵见 `docs/nextdev/tab-session-redesign.md` §4（五场景手工清单）
- wire 自查：前端 PR 零 envelope 变更（天然满足，CI diff 提示即可）

**完成标准**：20 项全部三态之一——已修 / 已否决(附理由) / 记录为已知限制；
汇总收尾报告并回填本文档状态列。

---

## 7. 风险与缓解

| 风险 | 缓解 |
|---|---|
| vendor 补丁需走快照 bump 流程（dev-downstream） | PR-F1 顺带打样流水线，后续复用 |
| F-R1 流式富文本的性能回归（live 重解析已在 UI 线程节流） | memo 粒度按 segment；diagnostics幀间隔指标对照 |
| MiSans 许可文本缺失（F-F2） | 已补齐：MISANS_LICENSE.txt 官方原文随包 |
| H2 若在后端被顺延 | 前端 S1-S4 仍可独立合入（错投主因已除），仅引擎侧幽灵状态残留——在 issue #2 中显式声明此降级边界 |

---

*本文档为第一轮前端审查的唯一事实源；第二轮建议范围：composer_bar 状态机、
info_panel/diff_drawer 数据流、settings_view 草稿脏位模型。*
