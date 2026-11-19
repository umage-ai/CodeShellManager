//! Copies the bundled ConPTY (conpty.dll + OpenConsole.exe) beside the built executables, from
//! `CSM_CONPTY_DIR` or `<workspace>/.conpty/<arch>/` (filled by scripts/get-conpty.ps1). Without
//! them the app runs on the inbox ConPTY and says so in its log.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CSM_CONPTY_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
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
            println!("cargo:warning={name} not found in {} — the spike will use the inbox ConPTY. Run scripts/get-conpty.ps1.", src.display());
            continue;
        }
        let to = profile_dir.join(name);
        let same = std::fs::metadata(&from).ok().map(|m| m.len()) == std::fs::metadata(&to).ok().map(|m| m.len());
        if !same && let Err(e) = std::fs::copy(&from, &to) {
            println!("cargo:warning=could not copy {name} to {}: {e}", to.display());
        }
    }
}
