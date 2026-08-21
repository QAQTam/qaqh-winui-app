//! 嵌入应用图标与版本信息（QAQ Harness）。
//!
//! 图标来源：assets/app.ico（由设计稿 PNG 生成，16–256 多尺寸）。
//! 仅 Windows 目标需要资源段；winres 在非 Windows 主机上不编译。

fn main() {
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "QAQ Harness");
        res.set("FileDescription", "QAQ Harness 安装程序");
        res.compile().expect("embed icon resource");
    }
}
