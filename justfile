# qaqh-winui-app — QAQ-Harness Windows 桌面层（WinUI3 壳 + 安装器 + 更新器）
# 后端依赖：../QAQ-Harness（path 依赖 ../QAQ-Harness/crates/*），daemon 构建也指向后端仓库。
# 用法: just [recipe]

set windows-shell := ["pwsh.exe", "-NoLogo", "-Command"]

# ── 默认 ────────────────────────────────────────────
default:
    @just --list

# ── 后端 sidecar（来自 QAQ-Harness 后端仓库）────────────
# 在后端仓库构建 daemon + workspace（release）
build-daemon:
    cargo build --release --manifest-path ../QAQ-Harness/Cargo.toml -p qaqh-daemon -p qaqh-workspace

# ── 前端构建 ─────────────────────────────────────────

# 编译 WinUI3 壳（release）
[windows]
build-winui:
    cargo build --release -p qaqh-winui

# 编译原生安装器（release）
[windows]
build-installer:
    cargo build --release -p qaqh-installer

# 编译组件更新器（release）
build-updater:
    cargo build --release -p qaqh-updater

# ── 打包 ─────────────────────────────────────────────

# 打包 winui 运行目录（release/winui-app，完整安装包使用）
[windows]
package-winui-desktop: build-daemon build-winui
    pwsh -File apps/winui/scripts/prepare-daemon.ps1 -BackendRoot ../QAQ-Harness
    ./apps/winui/scripts/assemble-winui.ps1

# 生成完整安装包 EXE
[windows]
winui-package: package-winui-desktop build-installer build-updater
    ./apps/installer/scripts/collect-payload-winui.ps1 -Kind full -BackendRoot ../QAQ-Harness
    ./apps/installer/scripts/finalize.ps1 -Kind full

# SFX 快速拼接（staging 已就位，跳过构建和收集）
[windows]
sfx-quick kind="full":
    ./apps/installer/scripts/finalize.ps1 -Kind {{kind}}

# ── 更新发布（基线累积模型）────────────────────────────

# 编译 qaqh-pack（更新包打包 CLI）
[windows]
build-pack:
    cargo build --release -p qaqh-update

# 发布增量更新包（基于最近检查点基线；runtime/backend）
[windows]
upgrade-package kind="runtime": build-pack
    pwsh -File apps/installer/scripts/upgrade-package.ps1 -Kind {{kind}}

# 发布检查点完整包 + 新基线
[windows]
baseline-release version: build-pack
    pwsh -File apps/installer/scripts/baseline-release.ps1 -Version {{version}}

# ── 检查 & 测试 ──────────────────────────────────────

check:
    cargo check --workspace

test:
    cargo test --workspace

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets

# ── downstream fork 切换（windows-rs fork 本地/git rev）──
[windows]
downstream action="status":
    pwsh -File scripts/dev-downstream.ps1 {{action}}

# ── 工具 ─────────────────────────────────────────────

[windows]
status:
    @Write-Output "=== Rust binaries ==="
    @if (Test-Path 'target/release/qaqh-winui.exe') { '  ✓ qaqh-winui.exe' } else { '  ✗ qaqh-winui.exe' }
    @if (Test-Path 'target/release/QAQ-HarnessInstaller.exe') { '  ✓ QAQ-HarnessInstaller.exe' } else { '  ✗ QAQ-HarnessInstaller.exe' }
    @if (Test-Path 'target/release/qaqh-updater.exe') { '  ✓ qaqh-updater.exe' } else { '  ✗ qaqh-updater.exe' }
    @Write-Output "=== WinUI run dir ==="
    @if (Test-Path 'apps/winui/release/winui-app/QAQ-Harness.exe') { '  ✓ winui-app/QAQ-Harness.exe' } else { '  ✗ winui-app (run just package-winui-desktop)' }

[windows]
clean:
    cargo clean
    @Remove-Item -Recurse -Force 'apps/winui/out' -ErrorAction SilentlyContinue
    @Remove-Item -Recurse -Force 'apps/winui/release/winui-app' -ErrorAction SilentlyContinue
    @Remove-Item -Recurse -Force 'packages' -ErrorAction SilentlyContinue
    @Remove-Item -Recurse -Force 'apps/installer/dist' -ErrorAction SilentlyContinue
    @Remove-Item -Recurse -Force 'apps/installer/staging' -ErrorAction SilentlyContinue
    @Remove-Item -Recurse -Force 'apps/installer/payload/desktop' -ErrorAction SilentlyContinue
    @Write-Output "Clean done."

# 从 version.txt 同步版本号到本仓库 Cargo.toml / package.json
[windows]
sync-version:
    @pwsh -File scripts/sync-version.ps1
