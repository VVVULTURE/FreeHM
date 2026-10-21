// Embeds the icon, version information and an application manifest (per-monitor DPI
// awareness, Common Controls 6, `asInvoker` — FreeHM itself never requests elevation).
fn main() {
    println!("cargo:rerun-if-changed=../../assets/icons/freehm.ico");
    println!("cargo:rerun-if-changed=freehm.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icons/freehm.ico");
        res.set_manifest_file("freehm.manifest");
        res.set("FileDescription", "FreeHM hardware statistics overlay");
        res.set("ProductName", "FreeHM");
        res.set("LegalCopyright", "MIT License — see LICENSE");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
