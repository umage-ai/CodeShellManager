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

/// Largest file copied out of a sample folder.
const MAX_SAMPLE_FILE: u64 = 1024 * 1024;

/// What [`install_samples`] did, by add-on id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// Folders copied into the add-ons folder.
    pub installed: Vec<String>,
    /// Folders left alone because something is already there under that id.
    pub skipped: Vec<String>,
}

/// Copies each sample add-on folder under `samples` into `root` (`<config dir>/addons`) unless
/// `root/<id>` already exists (as anything, a link included): nothing is ever overwritten or
/// merged, so a sample the person edited or enabled is untouched. Only the files in a sample
/// folder and in its sub-folders one level down are copied (regular files up to 1 MiB; links are not followed). A missing
/// `samples` folder is an error: the build has none. Blocking file I/O: call from a worker.
pub fn install_samples(samples: &Path, root: &Path) -> Result<InstallReport, String> {
    let entries = fs::read_dir(samples).map_err(|e| format!("could not read the samples folder {}: {e}", samples.display()))?;
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, _)| valid_id(name))
        .collect();
    dirs.sort();
    dirs.truncate(MAX_FOLDERS);
    ensure_dir(root).map_err(|e| format!("could not create {}: {e}", root.display()))?;
    let mut report = InstallReport::default();
    for (id, src) in dirs {
        let dest = root.join(&id);
        if fs::symlink_metadata(&dest).is_ok() {
            report.skipped.push(id);
            continue;
        }
        // `create_dir` (not `_all`) fails if something appeared since the check.
        if fs::create_dir(&dest).is_err() {
            report.skipped.push(id);
            continue;
        }
        if let Err(e) = copy_files(&src, &dest) {
            let _ = fs::remove_dir_all(&dest);
            return Err(format!("could not copy {id}: {e}"));
        }
        report.installed.push(id);
    }
    Ok(report)
}

fn copy_files(src: &Path, dest: &Path) -> std::io::Result<()> {
    copy_level(src, dest, 0)
}

/// Copies the regular files of `src` and, one level down, of its sub-folders (a theme's `fonts/`).
fn copy_level(src: &Path, dest: &Path, depth: u8) -> std::io::Result<()> {
    for entry in fs::read_dir(src)?.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() && depth == 0 {
            let sub = dest.join(entry.file_name());
            fs::create_dir(&sub)?;
            copy_level(&entry.path(), &sub, 1)?;
            continue;
        }
        if !ft.is_file() || entry.metadata().map_or(true, |m| m.len() > MAX_SAMPLE_FILE) {
            continue;
        }
        fs::copy(entry.path(), dest.join(entry.file_name()))?;
    }
    Ok(())
}

/// Reads a theme's font file (`[theme.style] ui_font = "fonts/X.otf"`): the path is checked
/// ([`csm_core::addons::theme::check_font_path`]), resolved against `addon_dir`, and refused when it
/// leaves the folder (a link or a junction included), is not a regular file, is larger than
/// [`csm_core::addons::theme::MAX_FONT_BYTES`], or does not parse as a font with a Latin `a`. Blocking
/// file I/O: call from a worker.
pub fn read_theme_font(addon_dir: &Path, rel: &str) -> Result<Vec<u8>, String> {
    use csm_core::addons::theme::{MAX_FONT_BYTES, check_font_path};
    check_font_path(rel)?;
    let root = fs::canonicalize(addon_dir).map_err(|e| format!("the add-on folder could not be read: {e}"))?;
    let file = fs::canonicalize(root.join(rel)).map_err(|e| format!("{rel}: {}", if e.kind() == std::io::ErrorKind::NotFound { "no such file".to_owned() } else { e.to_string() }))?;
    if !file.starts_with(&root) {
        return Err(format!("{rel}: the file is outside the add-on's folder"));
    }
    let meta = fs::metadata(&file).map_err(|e| format!("{rel}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{rel}: not a file"));
    }
    if meta.len() > MAX_FONT_BYTES {
        return Err(format!("{rel}: {} bytes is over the {} MiB limit", meta.len(), MAX_FONT_BYTES / (1024 * 1024)));
    }
    let mut data = Vec::new();
    fs::File::open(&file).and_then(|f| f.take(MAX_FONT_BYTES + 1).read_to_end(&mut data)).map_err(|e| format!("{rel}: {e}"))?;
    if data.len() as u64 > MAX_FONT_BYTES {
        return Err(format!("{rel}: over the {} MiB limit", MAX_FONT_BYTES / (1024 * 1024)));
    }
    check_font_bytes(&data).map_err(|e| format!("{rel}: {e}"))?;
    Ok(data)
}

/// Whether `data` is a font the UI can use: a TrueType or OpenType file whose first face has a glyph
/// for `a` and for the space.
pub fn check_font_bytes(data: &[u8]) -> Result<(), String> {
    let font = swash::FontRef::from_index(data, 0).ok_or("not a TrueType or OpenType font")?;
    let map = font.charmap();
    if map.map('a') == 0 || map.map(' ') == 0 {
        return Err("the font has no Latin letters".into());
    }
    Ok(())
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
    fn samples_are_copied_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("samples");
        write(&samples, "alpha", "a");
        write(&samples, "beta", "b");
        fs::write(samples.join("beta").join("extra.txt"), "x").unwrap();
        fs::create_dir_all(samples.join("Bad Name")).unwrap();
        let root = dir.path().join("addons");
        write(&root, "beta", "mine");
        let r = install_samples(&samples, &root).unwrap();
        assert_eq!(r.installed, vec!["alpha"]);
        assert_eq!(r.skipped, vec!["beta"]);
        assert_eq!(fs::read_to_string(root.join("alpha").join(MANIFEST_FILE)).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("beta").join(MANIFEST_FILE)).unwrap(), "mine");
        assert!(!root.join("beta").join("extra.txt").exists());
        let again = install_samples(&samples, &root).unwrap();
        assert!(again.installed.is_empty() && again.skipped.len() == 2);
        assert!(install_samples(&dir.path().join("none"), &root).is_err());
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
    const W95FA: &[u8] = include_bytes!("../../../samples/addons/theme-fun/fonts/W95FA.otf");

    fn addon_with_font(bytes: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("fonts")).unwrap();
        fs::write(dir.path().join("fonts").join("a.otf"), bytes).unwrap();
        dir
    }

    #[test]
    fn a_theme_font_inside_the_folder_loads() {
        let dir = addon_with_font(W95FA);
        assert_eq!(read_theme_font(dir.path(), "fonts/a.otf").unwrap(), W95FA);
    }

    #[test]
    fn a_theme_font_that_is_not_a_font_or_is_missing_is_refused() {
        let dir = addon_with_font(b"this is not a font at all, only text");
        assert!(read_theme_font(dir.path(), "fonts/a.otf").unwrap_err().contains("not a TrueType or OpenType font"));
        assert!(read_theme_font(dir.path(), "fonts/none.otf").unwrap_err().contains("no such file"));
        let dir = addon_with_font(&W95FA[..40]);
        assert!(read_theme_font(dir.path(), "fonts/a.otf").is_err(), "a truncated font");
    }

    #[test]
    fn a_theme_font_over_the_size_cap_is_refused_unread() {
        let dir = addon_with_font(&[0u8; 0]);
        let big = fs::File::create(dir.path().join("fonts").join("a.otf")).unwrap();
        big.set_len(csm_core::addons::theme::MAX_FONT_BYTES + 1).unwrap();
        let e = read_theme_font(dir.path(), "fonts/a.otf").unwrap_err();
        assert!(e.contains("over the 4 MiB limit"), "{e}");
    }

    #[test]
    fn a_theme_font_path_cannot_leave_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let addon = dir.path().join("addon");
        fs::create_dir_all(addon.join("fonts")).unwrap();
        fs::write(dir.path().join("outside.otf"), W95FA).unwrap();
        for bad in ["../outside.otf", "fonts/../../outside.otf", "/outside.otf"] {
            assert!(read_theme_font(&addon, bad).is_err(), "{bad}");
        }
        let absolute = dir.path().join("outside.otf").display().to_string().replace(std::path::MAIN_SEPARATOR, "/");
        assert!(read_theme_font(&addon, &absolute).is_err(), "{absolute}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let addon = dir.path().join("addon");
        fs::create_dir_all(addon.join("fonts")).unwrap();
        fs::write(dir.path().join("outside.otf"), W95FA).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.otf"), addon.join("fonts").join("a.otf")).unwrap();
        assert!(read_theme_font(&addon, "fonts/a.otf").unwrap_err().contains("outside"));
    }

    #[test]
    fn samples_bring_their_font_sub_folder() {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("samples");
        write(&samples, "alpha", "a");
        fs::create_dir_all(samples.join("alpha").join("fonts")).unwrap();
        fs::write(samples.join("alpha").join("fonts").join("f.otf"), "font").unwrap();
        let root = dir.path().join("addons");
        install_samples(&samples, &root).unwrap();
        assert_eq!(fs::read_to_string(root.join("alpha").join("fonts").join("f.otf")).unwrap(), "font");
    }
}
