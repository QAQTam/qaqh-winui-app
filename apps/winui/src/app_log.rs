//! 统一诊断日志：`<安装目录>/log/<组件>/<组件>.log`（按类型分文件夹）。
//!
//! 此前 7 个 `log_diag` 各自为政：6 处写 CWD 相对的 `.qaqh-winui.log`
//!（从桌面/开始菜单启动就把日志撒在启动目录，且互相覆盖语义不明），
//! 1 处（chat_view）写 `%TEMP%\qaqh-winui.log`——排查问题时两份日志
//! 无法交叉定位。现统一经 [`write`] 落盘：
//!
//! - 根目录解析（进程内缓存，首个写入者触发），按序取第一个可写者：
//!   1. `QAQH_WINUI_LOG_DIR` 环境变量（测试/诊断重定向）；
//!   2. exe 同级 `log\`（安装目录；绿色/用户级安装可写）；
//!   3. `%LOCALAPPDATA%\QAQ-Harness\log`（exe 目录只读时，如
//!      Program Files；与 ui-preferences.json 同区）；
//!   4. `%TEMP%\qaqh-winui-log`（兜底）。
//!   候选以「建目录 + 试写探测文件」验证，避免选到只读根后整类日志
//!   静默丢失。
//! - 每条记录统一 `[unix_ms] msg` 前缀（原 bridge 侧无时间戳，多组件
//!   日志无法对齐时序）。
//! - append 语义保留；各组件写各自类别文件夹，无锁竞争；写失败静默
//!  （与旧 log_diag 一致——日志永不致错）。

use std::path::PathBuf;
use std::sync::OnceLock;

/// 进程内缓存已解析的日志根目录（首个写入者触发解析）。
fn base_dir() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        // 1) 显式重定向（测试/诊断）。
        if let Ok(dir) = std::env::var("QAQH_WINUI_LOG_DIR") {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir);
            }
        }
        // 2) 安装目录（exe 同级）。
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join("log");
                if ensure_writable(&candidate) {
                    return candidate;
                }
            }
        }
        // 3) exe 目录不可写（如 Program Files）→ 用户区。
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.trim().is_empty() {
                let candidate = PathBuf::from(local).join("QAQ-Harness").join("log");
                if ensure_writable(&candidate) {
                    return candidate;
                }
            }
        }
        // 4) 兜底：临时目录。
        std::env::temp_dir().join("qaqh-winui-log")
    })
}

/// 建目录并试写探测文件，验证可写性。
fn ensure_writable(dir: &PathBuf) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 追加一条 `<category>/<category>.log`。`category` 仅接受各 log_diag
/// 写死的字面量（app/bridge/chat_view/composer/info_panel/interaction/
/// shell），不做路径净化。
pub(crate) fn write(category: &str, msg: &str) {
    use std::io::Write;
    let dir = base_dir().join(category);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{category}.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}
