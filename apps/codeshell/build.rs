//! Copies the bundled ConPTY (conpty.dll + OpenConsole.exe) beside the built executables, from
//! `CSM_CONPTY_DIR` or `<workspace>/.conpty/<arch>/` (filled by scripts/get-conpty.ps1). Without
//! them the app runs on the inbox ConPTY and says so in its log.
//!
//! On Windows it also embeds `assets/codeshell.ico` as the exe's icon. That needs the Windows
//! SDK's rc.exe; without it the build goes on with a warning and the exe has no icon.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CSM_CONPTY_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    embed_icon();
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };
    let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else { return };
    let src = std::env::var_os("CSM_CONPTY_DIR").map(PathBuf::from).unwrap_or_else(|| manifest.join("..").join("..").join(".conpty").join(arch));
    let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else { return };
    // OUT_DIR = <target>/<profile>/build/<pkg>[-<hash>|/<hash>]/out (the layout varies by cargo
    // version): the profile dir is the parent of the nearest `build` ancestor.
    let Some(profile_dir) = out.ancestors().find(|p| p.file_name().is_some_and(|n| n == "build")).and_then(|b| b.parent()).map(PathBuf::from) else {
        return;
    };
    for name in ["conpty.dll", "OpenConsole.exe"] {
        let from = src.join(name);
        println!("cargo:rerun-if-changed={}", from.display());
        if !from.is_file() {
            println!("cargo:warning={name} not found in {} — the app will use the inbox ConPTY. Run scripts/get-conpty.ps1.", src.display());
            continue;
        }
        let to = profile_dir.join(name);
        // By content hash, not length: a different build of the same size must be copied too.
        let same = matches!((file_hash(&from), file_hash(&to)), (Some(a), Some(b)) if a == b);
        if !same && let Err(e) = std::fs::copy(&from, &to) {
            println!("cargo:warning=could not copy {name} to {}: {e}", to.display());
        }
    }
}

/// SHA-256 of a file, or `None` when it cannot be read.
fn file_hash(path: &Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(path).ok()?;
    Some(Sha256::digest(&bytes).into())
}

fn embed_icon() {
    let ico = "assets/codeshell.ico";
    println!("cargo:rerun-if-changed={ico}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico);
    // The names Task Manager, the file's Properties and the firewall prompt show (the default
    // would be the crate name "codeshell"). The version fields come from the workspace version,
    // which the release workflow stamps.
    res.set("ProductName", "CodeShellManager");
    res.set("FileDescription", "CodeShellManager");
    res.set("CompanyName", "umage.ai");
    if let Err(e) = res.compile() {
        println!("cargo:warning=the exe icon was not embedded ({e}); install the Windows SDK for rc.exe");
    }
}
