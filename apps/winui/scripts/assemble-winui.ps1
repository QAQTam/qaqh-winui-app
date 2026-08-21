# assemble-winui.ps1 — 组装 winui 壳运行目录（apps/winui/release/winui-app/）
#
# 原生布局（WebView/renderer 已移除）：
#   winui-app/
#     QAQ-Harness.exe                     ← 壳（安装器硬编码入口名）
#     <WinAppSDK self-contained DLL / PRI / MUI>
#     resources/
#       qaqh-daemon.exe / qaqh-workspace.exe / daemon-manifest.json
#     config/config.toml
#
# 前置：just build-daemon + just build-winui + prepare-daemon.ps1（sidecar）

param(
    [string]$SidecarDir = "apps/winui/out/sidecar",
    [string]$ConfigToml = "apps/installer/payload/config/default.toml",
    [string]$OutDir = "apps/winui/release/winui-app"
)

$ErrorActionPreference = "Stop"
$workspaceRoot = (Resolve-Path ".").Path
$outFull = [System.IO.Path]::GetFullPath((Join-Path $workspaceRoot $OutDir))

Write-Host "=== 组装 winui 运行目录 ==="
if (Test-Path -LiteralPath $outFull) {
    Remove-Item -LiteralPath $outFull -Recurse -Force
}
New-Item -ItemType Directory -Path $outFull -Force | Out-Null

# 1. 壳（命名 QAQ-Harness.exe，安装器 create_shortcut 硬编码该入口）
$shellExe = Join-Path $workspaceRoot "target/release/qaqh-winui.exe"
if (-not (Test-Path -LiteralPath $shellExe -PathType Leaf)) {
    throw "缺少 winui 壳: $shellExe（先跑 just build-winui）"
}
Copy-Item -LiteralPath $shellExe -Destination (Join-Path $outFull "QAQ-Harness.exe")

# 2. self-contained WinAppSDK 运行时 DLL（紧邻 exe）
$releaseDir = Join-Path $workspaceRoot "target/release"
Get-ChildItem -LiteralPath $releaseDir -Filter "*.dll" | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $outFull $_.Name)
}

# 2a. AppNotificationManager.Register() 依赖的 Insights 资源 DLL。
#     windows-reactor-setup 的 runtime.txt 列表不含该文件，自包含部署缺失时
#     Register 报 0x8007007E（"Unable to load resource dll.
#     Microsoft.WindowsAppRuntime.Insights.Resource.dll"）→ 桌面通知全部失败。
#     从构建期下载缓存的 WindowsAppSDK Runtime 包补齐；缓存路径不带版本假设
#     ——按 Microsoft.WindowsAppRuntime.dll 哈希匹配合适的 Runtime 版本
#     （必须与壳实际链接的运行时一致，错配仍会加载失败）。
$insightsCopied = $false
$runtimeCache = Join-Path $env:LOCALAPPDATA "windows-reactor-setup/temp"
if (Test-Path -LiteralPath $runtimeCache -PathType Container) {
    $shellRuntimeHash = (Get-FileHash -LiteralPath (Join-Path $releaseDir "Microsoft.WindowsAppRuntime.dll") -Algorithm SHA256).Hash.ToLowerInvariant()
    Get-ChildItem -LiteralPath $runtimeCache -Directory -Filter "Microsoft.WindowsAppSDK.Runtime-*" | ForEach-Object {
        if ($insightsCopied) { return }
        $extract = Join-Path $_.FullName ".msix_extract"
        $runtimeDll = Join-Path $extract "Microsoft.WindowsAppRuntime.dll"
        if (-not (Test-Path -LiteralPath $runtimeDll -PathType Leaf)) { return }
        $candidateHash = (Get-FileHash -LiteralPath $runtimeDll -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($candidateHash -ne $shellRuntimeHash) { return }
        $insights = Join-Path $extract "Microsoft.WindowsAppRuntime.Insights.Resource.dll"
        if (-not (Test-Path -LiteralPath $insights -PathType Leaf)) { return }
        Copy-Item -LiteralPath $insights -Destination (Join-Path $outFull "Microsoft.WindowsAppRuntime.Insights.Resource.dll")
        $insightsCopied = $true
    }
}
if (-not $insightsCopied) {
    throw "缺少 Microsoft.WindowsAppRuntime.Insights.Resource.dll（运行时缓存 $runtimeCache 中未找到与壳匹配的版本）；桌面通知将初始化失败（Register 0x8007007E）。请先执行 just build-winui 以填充 windows-reactor-setup 缓存。"
}

# 2b. WinAppSDK self-contained 资源文件（Mica/控件渲染必需，缺失则窗口创建失败）
Get-ChildItem -LiteralPath $releaseDir -Filter "*.pri" | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $outFull $_.Name)
}

# 2c. WinAppSDK self-contained 语言资源目录（<lang>/*.mui）。
#     每个语言目录含 Microsoft.ui.xaml.dll.mui / Microsoft.UI.Xaml.Phone.dll.mui。
#     XAML 控件初始化时按系统 UI 语言加载对应 MUI 资源（如中文系统的
#     zh-CN\Microsoft.ui.xaml.dll.mui），缺失会导致 MUI 加载失败
#     （ERROR_MUI_FILE_NOT_LOADED 0x80073B01）→ XAML 控件初始化失败
#     → 白屏 + stowed exception 闪退（崩溃模块 Microsoft.ui.xaml.dll）。
#     模式匹配 BCP-47 风格目录名（af-ZA、en-us、az-Latn-AZ、sr-Cyrl-RS 等），
#     不会误伤 build/deps/examples 等 cargo 目录。
Get-ChildItem -LiteralPath $releaseDir -Directory | Where-Object {
    $_.Name -match '^[a-z]{2}(-[A-Za-z0-9]+)*$'
} | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $outFull $_.Name) -Recurse -Force
}

# 3. resources/ — daemon sidecar（prepare-daemon.ps1 产出）
$resources = Join-Path $outFull "resources"
New-Item -ItemType Directory -Path $resources -Force | Out-Null
$sidecarFull = [System.IO.Path]::GetFullPath((Join-Path $workspaceRoot $SidecarDir))
foreach ($f in @("qaqh-daemon.exe", "qaqh-workspace.exe", "daemon-manifest.json")) {
    $src = Join-Path $sidecarFull $f
    if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
        throw "缺少 sidecar 文件: $src（先跑 just package-winui-desktop 的 prepare-daemon 步骤）"
    }
    Copy-Item -LiteralPath $src -Destination (Join-Path $resources $f)
}

# 3b. WinUI 内容资产——内置字体与其完整许可证。目录名大小写必须与
# `ms-appx:///Assets/...` FontFamily URI 完全一致。
$assetsSrc = Join-Path $workspaceRoot "apps/winui/assets"
if (-not (Test-Path -LiteralPath $assetsSrc -PathType Container)) {
    throw "缺少 WinUI 内容资产: $assetsSrc"
}
Copy-Item -LiteralPath $assetsSrc -Destination (Join-Path $outFull "Assets") -Recurse -Force

# 4. config
$configSrc = Join-Path $workspaceRoot $ConfigToml
if (Test-Path -LiteralPath $configSrc -PathType Leaf) {
    $configDir = Join-Path $outFull "config"
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Copy-Item -LiteralPath $configSrc -Destination (Join-Path $configDir "config.toml")
}

$fileCount = (Get-ChildItem -LiteralPath $outFull -Recurse -File).Count
Write-Host "  ✓ $fileCount 个文件 → $outFull"
