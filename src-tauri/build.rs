fn main() {
    // 图标在编译期嵌入 exe(资源图标 + 默认窗口图标),而 icons/ 由 CI 从
    // assets 源图生成且不进 git;cargo 默认不追踪这些文件,换图标而源码未变时
    // 会复用带旧图标的缓存产物,故显式声明依赖。
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    tauri_build::build()
}
