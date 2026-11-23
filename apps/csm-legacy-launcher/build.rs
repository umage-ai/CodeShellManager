//! On Windows, embeds the app's icon and names (`apps/codeshell/assets/codeshell.ico`): a taskbar
//! pin or shortcut to `CodeShellManager.exe` shows them. Needs the Windows SDK's rc.exe; without
//! it the build goes on with a warning and the exe has no icon.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = "../codeshell/assets/codeshell.ico";
    println!("cargo:rerun-if-changed={ico}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico);
    res.set("ProductName", "CodeShellManager");
    res.set("FileDescription", "CodeShellManager");
    res.set("CompanyName", "umage.ai");
    if let Err(e) = res.compile() {
        println!("cargo:warning=the exe icon was not embedded ({e}); install the Windows SDK for rc.exe");
    }
}
