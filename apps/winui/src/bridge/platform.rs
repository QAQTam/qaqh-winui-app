//! Win32/COM platform helpers used by the Bridge facade.

use serde_json::{Value, json};

/// Open a path/URL with the system shell (best effort).
pub(crate) fn open_external(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", target])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
        Ok(())
    }
}

/// Show the native file/folder picker (Win32 `IFileOpenDialog`).
///
/// **Must be called on the STA UI thread** — current call sites are the
/// header/sidebar click handlers (UI thread), previously `Bridge::handle_message`.
/// Mirrors Electron `dialog.showOpenDialog` semantics consumed by the shell:
///   - `directory` -> `FOS_PICKFOLDERS` (folder picker)
///   - `multiple`  -> `FOS_ALLOWMULTISELECT` (result becomes a JSON array)
///   - `image_filter` -> picture file types filter
///   - cancel      -> `null`; single -> string; multiple -> array of strings
pub(crate) fn show_open_dialog(
    directory: bool,
    multiple: bool,
    image_filter: bool,
    title: Option<&str>,
) -> Result<Value, String> {
    use windows::Win32::{
        CLSCTX_ALL, COMDLG_FILTERSPEC, CoCreateInstance, ERROR_CANCELLED, FOS_ALLOWMULTISELECT,
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    };
    use windows::core::{HSTRING, w};

    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL as u32)
            .map_err(|e| format!("CoCreateInstance(FileOpenDialog): {e}"))?;

        let mut options = FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        if directory {
            options |= FOS_PICKFOLDERS;
        }
        if multiple {
            options |= FOS_ALLOWMULTISELECT;
        }
        dialog
            .SetOptions(options)
            .ok()
            .map_err(|e| format!("IFileDialog::SetOptions: {e}"))?;

        if let Some(title) = title.filter(|t| !t.is_empty()) {
            let title = HSTRING::from(title);
            dialog
                .SetTitle(&title)
                .ok()
                .map_err(|e| format!("IFileDialog::SetTitle: {e}"))?;
        }

        if image_filter {
            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Images"),
                    pszSpec: w!("*.png;*.jpg;*.jpeg;*.gif;*.webp;*.bmp"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
            ];
            dialog
                .SetFileTypes(filters.len() as u32, filters.as_ptr())
                .ok()
                .map_err(|e| format!("IFileDialog::SetFileTypes: {e}"))?;
        }

        // Show() is modal; ERROR_CANCELLED (user pressed Cancel / Esc) is
        // mapped to `null`, matching the preload API's cancel semantics.
        // (0.100 HRESULT has no from_win32 helper; build the code inline.)
        // 传真实 owner：此前 `Show(None)` 无 owner——对话框打开期间主窗口
        // 未被禁用、模态循环仍泵送主窗口消息，此时再点「选择工作区」会
        // 嵌套弹出第二个对话框（「二次弹窗」根因之一）。
        let hr = dialog.Show(active_owner_hwnd());
        if hr.is_err() && hr.0 == ((ERROR_CANCELLED as u32 | 0x8007_0000) as i32) {
            return Ok(json!(null));
        }
        hr.ok().map_err(|e| format!("IFileDialog::Show: {e}"))?;

        if multiple {
            let items = dialog
                .GetResults()
                .map_err(|e| format!("IFileOpenDialog::GetResults: {e}"))?;
            let count = items
                .GetCount()
                .map_err(|e| format!("IShellItemArray::GetCount: {e}"))?;
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let item = items
                    .GetItemAt(i)
                    .map_err(|e| format!("IShellItemArray::GetItemAt({i}): {e}"))?;
                paths.push(shell_item_path(&item)?);
            }
            Ok(json!(paths))
        } else {
            let item = dialog
                .GetResult()
                .map_err(|e| format!("IFileDialog::GetResult: {e}"))?;
            Ok(json!(shell_item_path(&item)?))
        }
    }
}

/// Resolve an `IShellItem` to its filesystem path (`SIGDN_FILESYSPATH`).
/// The returned `PWSTR` is CoTaskMem-allocated and freed here.
pub(crate) fn shell_item_path(item: &windows::Win32::IShellItem) -> Result<String, String> {
    use windows::Win32::{CoTaskMemFree, SIGDN_FILESYSPATH};
    let pw = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
        .map_err(|e| format!("IShellItem::GetDisplayName(SIGDN_FILESYSPATH): {e}"))?;
    let path =
        unsafe { pw.to_string() }.map_err(|e| format!("selected path is not valid UTF-16: {e}"))?;
    unsafe { CoTaskMemFree(pw.0 as _) };
    Ok(path)
}

/// 文件/目录对话框的 owner 窗口：取当前线程的活动顶层窗口（用户点击按钮
/// 时即主窗口）。取不到（理论边界）时回退 `None`，等价旧行为。
///
/// 用真实 owner 后，`IFileOpenDialog` 会禁用 owner 窗口——对话框打开期间
/// 主窗口不可点击，「再点一次按钮 → 嵌套弹出第二个对话框」不再可能。
/// （user32 的 `GetActiveWindow` 不在 windows fork 的 feature 集内，用
/// 最小 extern 声明直连。）
pub(crate) fn active_owner_hwnd() -> Option<windows::Win32::HWND> {
    use windows::Win32::HWND;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetActiveWindow() -> *mut core::ffi::c_void;
    }
    let hwnd = unsafe { GetActiveWindow() };
    if hwnd.is_null() {
        None
    } else {
        Some(HWND(hwnd))
    }
}
