fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("ProductName", "Codex Usage Widget")
            .set("FileDescription", "Codex Usage Widget")
            .compile()
            .expect("compile Windows icon and version information");
    }
}
