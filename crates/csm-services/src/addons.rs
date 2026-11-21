//! The add-on folder on disk (docs/addons.md): `<config dir>/addons/<id>/addon.toml`.
//!
//! Reads only, on the caller's thread (the engine calls it from a worker). The text is handed to
//! `csm_core::addons::parse`; nothing here interprets it. Hostile input throughout: sizes are
//! capped before a file is read, symlinks are not followed, and a folder that is not an add-on
//! id is reported rather than skipped silently.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use csm_core::addons::manifest::{MAX_MANIFEST_BYTES, valid_id};
use sha2::{Digest, Sha256};

/// Most folders looked at in one scan; more are reported as one error and ignored.
pub const MAX_FOLDERS: usize = 256;
/// The manifest file name inside each add-on folder.
pub const MANIFEST_FILE: &str = "addon.toml";

/// One add-on folder as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The folder name (the add-on id it must equal).
    pub folder: String,
    pub path: PathBuf,
    /// The manifest text, or why it could not be read.
    pub text: Result<String, String>,
}

/// The add-ons folder beside `state.json`'s folder.
pub fn addons_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("addons")
}

/// Creates the add-ons folder if it is missing (the "Open folder" button needs it to exist).
pub fn ensure_dir(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root)
}

/// Every folder under `root` that could be an add-on, sorted by name. A missing `root` is an
/// empty list (no add-ons yet), not an error.
pub fn scan(root: &Path) -> Result<Vec<Found>, String> {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("could not read {}: {e}", root.display())),
    };
    let mut dirs: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_file() || name.starts_with('.') {
            continue;
        }
        dirs.push((name, entry.path(), ft.is_symlink()));
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    let overflow = dirs.len().saturating_sub(MAX_FOLDERS);
    dirs.truncate(MAX_FOLDERS);
    let mut out: Vec<Found> = dirs.into_iter().map(|(folder, path, link)| read_one(folder, path, link)).collect();
    if overflow > 0 {
        out.push(Found { folder: "(more folders)".into(), path: root.to_path_buf(), text: Err(format!("more than {MAX_FOLDERS} folders; {overflow} ignored")) });
    }
    Ok(out)
}

fn read_one(folder: String, path: PathBuf, symlink: bool) -> Found {
    let text = if symlink {
        Err("symlinked add-on folders are not followed; copy the folder instead".to_owned())
    } else if !valid_id(&folder) {
        Err("the folder name is not a valid add-on id (a-z, 0-9, '-', up to 64 characters)".to_owned())
    } else {
        read_manifest(&path.join(MANIFEST_FILE))
    };
    Found { folder, path, text }
}

/// Reads `addon.toml`, refusing anything over [`MAX_MANIFEST_BYTES`] (checked twice: by the
/// metadata and by the bounded read, in case the file grows in between).
pub fn read_manifest(file: &Path) -> Result<String, String> {
    // `symlink_metadata`: a manifest that is a link (to anything) is refused, not followed.
    let meta = fs::symlink_metadata(file).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("no {MANIFEST_FILE} in this folder"),
        _ => format!("could not read {MANIFEST_FILE}: {e}"),
    })?;
    if meta.file_type().is_symlink() {
        return Err(format!("{MANIFEST_FILE} is a link; copy the file instead"));
    }
    if !meta.is_file() {
        return Err(format!("{MANIFEST_FILE} is not a file"));
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST_FILE} is larger than {MAX_MANIFEST_BYTES} bytes"));
    }
    let f = fs::File::open(file).map_err(|e| format!("could not open {MANIFEST_FILE}: {e}"))?;
    let mut buf = Vec::new();
    f.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut buf).map_err(|e| format!("could not read {MANIFEST_FILE}: {e}"))?;
    if buf.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST_FILE} is larger than {MAX_MANIFEST_BYTES} bytes"));
    }
    String::from_utf8(buf).map_err(|_| format!("{MANIFEST_FILE} is not UTF-8 text"))
}

/// The SHA-256 of a manifest's text, lowercase hex. Approvals store it: an add-on whose
/// `addon.toml` changes after the person approved it is off until they approve it again.
pub fn digest(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn write(root: &Path, folder: &str, text: &str) {
        let d = root.join(folder);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(MANIFEST_FILE), text).unwrap();
    }

    #[test]
    fn missing_root_is_no_add_ons() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scan(&dir.path().join("nope")).unwrap(), Vec::new());
    }

    #[test]
    fn folders_are_sorted_and_read() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "beta", "b");
        write(dir.path(), "alpha", "a");
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        fs::write(dir.path().join("loose.toml"), "x").unwrap();
        fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        let found = scan(dir.path()).unwrap();
        let names: Vec<&str> = found.iter().map(|f| f.folder.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "empty"]);
        assert_eq!(found[0].text.as_deref(), Ok("a"));
        assert!(found[2].text.as_ref().unwrap_err().contains("no addon.toml"));
    }

    #[test]
    fn invalid_folder_names_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Not Valid", "x");
        let found = scan(dir.path()).unwrap();
        assert!(found[0].text.as_ref().unwrap_err().contains("not a valid add-on id"));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.toml");
        fs::write(&target, "x").unwrap();
        let d = dir.path().join("linked");
        fs::create_dir_all(&d).unwrap();
        std::os::unix::fs::symlink(&target, d.join(MANIFEST_FILE)).unwrap();
        let found = scan(dir.path()).unwrap();
        assert!(found[0].text.as_ref().unwrap_err().contains("is a link"));
    }

    #[test]
    fn oversized_manifest_is_refused_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_MANIFEST_BYTES as usize + 1);
        write(dir.path(), "big", &big);
        let found = scan(dir.path()).unwrap();
        assert!(found[0].text.as_ref().unwrap_err().contains("larger than"));
    }

    #[test]
    fn non_utf8_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("bin");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(MANIFEST_FILE), [0xff, 0xfe, 0x00]).unwrap();
        let found = scan(dir.path()).unwrap();
        assert!(found[0].text.as_ref().unwrap_err().contains("not UTF-8"));
    }

    #[test]
    fn digest_is_stable_hex() {
        assert_eq!(digest("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(digest("abc").len(), 64);
    }
}
