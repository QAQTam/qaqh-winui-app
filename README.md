# qaqh-winui-app

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Version](https://img.shields.io/badge/version-0.0.0--alpha.0-orange)

**QAQ-Harness Windows 桌面层** —— 原生 WinUI 3 桌面壳 + 安装器 + 更新器。后端（daemon / workspace）位于独立的 [QAQ-Harness](../QAQ-Harness) monorepo，本仓库通过 path 依赖其 `qaqh-client` / `qaqh-types` crate。

> 无 WebView：全部界面由 [windows-reactor](https://github.com/QAQTam/qaqh-winui-reactor-vendor) fork 以 React 风格 hooks 直接驱动原生 XAML 控件树。

## 仓库结构

```text
apps/
├── winui/       # qaqh-winui —— 主桌面应用（原生 XAML 视图族）
├── installer/   # qaqh-installer —— egui 安装程序（macOS 风格步骤导航）
└── updater/     # qaqh-updater —— 组件更新规划/执行器 + 维护/卸载 UI

crates/
├── markdown-core             # ChatView markdown 渲染核心（final 解析 + 流式 live 语义）
├── markdown-winui            # AST → reactor 富文本模型对接（RichTextBlock）
├── qaqh-fluent               # Windows 11 Fluent 视觉原语 / 动效（motion）
├── qaqh-app-notifications    # Microsoft.Windows.AppNotifications WinMD 绑定
└── qaqh-update               # 更新协议引擎（catalog/planner/state；自后端三刀重构迁入）

scripts/                       # dev-downstream.ps1 / sync-version.ps1
justfile                       # 构建 / 打包 / 发布 / 检查 全流程入口
version.txt                    # 版本号单一来源（sync-version 同步到 Cargo.toml/package.json）
```

## 架构要点

### 视图层（apps/winui）

- **壳组件**（`apps/winui/src/main.rs`）：Mica 窗口承载视图族——sidebar（可拖拽宽度）/ 标题栏 / 会话标签条 / 右侧四行 Grid（chat、skills、home、settings）。非当前视图行高归零且不挂载组件，子树卸载即停掉内部轮询。
- **Bridge 层**（`apps/winui/src/bridge/`）：`qaqh-client`（Ringing 协议）→ 原生视图的唯一数据通道。`BridgeCore`（tokio 侧）负责 daemon 连接管理 + SSE 频道事件解析，按域拆分（sessions / timeline / settings / skills / interaction / notifications / remote …）。
- **单一 UI 泵**：仅一个 50ms DispatcherTimer。每 tick 必跑 `bridge.pump()`（事件队列延迟敏感）；250ms 门控同步视图/开屏状态，500ms 门控同步字体/主题/OOBE。
- **开屏与 OOBE**：daemon 冷启动期以原生 ProgressRing 覆盖层过渡（60s 超时露出错误详情）；首次启动进入三步引导（完成标志 `%LOCALAPPDATA%\QAQ-Harness\oobe.done`）。
- **聊天渲染管线**：conversation 事件 → timeline 快照 + live 追加 → `chat_view` Transcript 控件树；diff 抽屉（V7）在 turn 末尾「查看详情」打开全屏覆盖层。

### 数据与更新

- 用户数据根：`%USERPROFILE%\.deepx`（`QAQH_DATA_DIR` 可覆盖），会话元数据在 `<data>/sessions/{seed}/meta.json`；诊断日志 `.deepx-winui.log`（`QAQH_WINUI_LOG` 重定向）。
- **更新协议**（基线累积模型）：`qaqh-update` CLI 生成增量包（基于最近检查点基线）或检查点完整包；`qaqh-updater` 负责 stage → apply-staged（重启时原子应用，支持 rollback）→ handoff，另提供 `maintain`（修复安装）/ `uninstall`（含可选删除用户数据）维护 UI。

## 环境要求

- Windows 10/11 + WinUI 3 运行时（Windows App SDK）
- Rust（edition 2024，MSVC toolchain）
- [just](https://github.com/casey/just)（命令入口）、PowerShell 7（pwsh）
- 后端仓库需克隆为本仓库的兄弟目录：`../QAQ-Harness`
- 下游 windows-rs fork 固定 rev（见根 `Cargo.toml`），本地开发可用 `just downstream switch/status` 在 fork 本地路径与 git rev 间切换

## 快速开始

```pwsh
# 查看全部命令
just

# 编译三个产物（release）
just build-daemon      # 后端 daemon + workspace（在后端仓库构建）
just build-winui       # 主桌面应用
just build-installer   # 安装器
just build-updater     # 更新器

# 组装 WinUI 运行目录（release/winui-app，含 prepare-daemon sidecar）
just package-winui-desktop

# 生成完整安装包 EXE
just winui-package

# SFX 快速拼接（staging 已就位时跳过构建）
just sfx-quick full
```

## 更新发布

```pwsh
just upgrade-package runtime    # 增量更新包（runtime/backend）
just baseline-release 0.1.0     # 检查点完整包 + 新基线
```

## 开发检查

```pwsh
just check    # cargo check --workspace
just test     # cargo test --workspace
just fmt      # cargo fmt --all --check
just clippy   # cargo clippy --workspace --all-targets
```

Workspace lints：`unwrap_used = deny`、`string_slice = deny`。

## 工具与演示

```pwsh
cargo run -p qaqh-winui --bin diff_demo          # 双栏 diff 视觉验证演示
cargo run -p qaqh-winui --bin diff_drawer_demo   # Diff 弹层（V7）演示
just status          # 各产物是否就位
just clean           # 清理 target / 运行目录 / 打包中间产物
just sync-version    # 从 version.txt 同步版本号
```

## 相关仓库

- 后端 monorepo：`QAQ-Harness`（daemon / workspace / qaqh-client / qaqh-types）
- 下游 UI fork：[qaqh-winui-reactor-vendor](https://github.com/QAQTam/qaqh-winui-reactor-vendor)

## 许可证

[MIT](LICENSE)
