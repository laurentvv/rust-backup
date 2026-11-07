//! Embeds the Windows executable icon and version resources.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set(
            "FileDescription",
            "RustBackup — encrypted, deduplicated backups",
        );
        res.set("ProductName", "RustBackup");
        res.set("LegalCopyright", "© 2026");
        res.set_language(0x040C); // French
        if let Err(e) = res.compile() {
            println!("cargo:warning=winres failed: {e}");
        }
    }
}
