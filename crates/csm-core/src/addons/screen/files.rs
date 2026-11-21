//! `file_list`: CSM builds the folder tree from the paths a plugin sends, so the plugin stays thin.

use std::collections::{BTreeMap, HashSet};

/// Most path segments the tree keeps; a deeper path puts the rest in its last name.
pub const MAX_SEGMENTS: usize = 16;
/// Most rows [`file_rows`] returns (folders and files).
pub const MAX_ROWS: usize = 50_000;

/// One changed or listed file, as the plugin sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Forward slashes, no leading `./` or `/`.
    pub path: String,
    /// `M`, `A`, `D`, `R`, `C`, `U` or `?`.
    pub status: Option<char>,
    pub old_path: Option<String>,
    pub badges: Vec<super::Badge>,
}

/// One row of the list as it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub depth: usize,
    /// What the row shows: a folder or file name (the whole path in the flat view).
    pub name: String,
    /// The folder's path (what folding is keyed by), or the file's full path.
    pub path: String,
    pub dir: bool,
    /// The file's index in the entries (`None` for a folder).
    pub entry: Option<usize>,
    /// A folder that is folded (its children are not in the rows).
    pub folded: bool,
}

/// A path as the list keeps it: backslashes become `/`, a leading `./` or `/` goes.
pub fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.trim_start_matches("./").trim_start_matches('/');
    s.to_owned()
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

fn emit(dir: &Dir, prefix: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<FileRow>) {
    for (name, sub) in &dir.dirs {
        if out.len() >= MAX_ROWS {
            return;
        }
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let folded = collapsed.contains(&path);
        out.push(FileRow { depth, name: name.clone(), path: path.clone(), dir: true, entry: None, folded });
        if !folded {
            emit(sub, &path, depth + 1, collapsed, out);
        }
    }
    let mut files: Vec<&(String, usize)> = dir.files.iter().collect();
    files.sort_by_key(|(n, _)| n.to_lowercase());
    for (name, i) in files {
        if out.len() >= MAX_ROWS {
            return;
        }
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        out.push(FileRow { depth, name: name.clone(), path, dir: false, entry: Some(*i), folded: false });
    }
}

/// The rows to draw for `files`. `tree`: folders (folded when in `collapsed`) then files, names
/// sorted; otherwise one row per file in the plugin's order. An entry with an empty path is
/// skipped.
pub fn file_rows(files: &[FileEntry], tree: bool, collapsed: &HashSet<String>) -> Vec<FileRow> {
    if !tree {
        return files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.path.is_empty())
            .take(MAX_ROWS)
            .map(|(i, f)| FileRow { depth: 0, name: f.path.clone(), path: f.path.clone(), dir: false, entry: Some(i), folded: false })
            .collect();
    }
    let mut root = Dir::default();
    for (i, f) in files.iter().enumerate() {
        let segs: Vec<&str> = f.path.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            continue;
        }
        let last_start = segs.len().min(MAX_SEGMENTS).saturating_sub(1);
        // Folders for all but the last kept segment; a path deeper than the cap puts the rest in the file name.
        let name = segs.get(last_start..).map(|r| r.join("/")).unwrap_or_default();
        let mut cur = &mut root;
        for seg in segs.iter().take(last_start) {
            cur = cur.dirs.entry((*seg).to_owned()).or_default();
        }
        cur.files.push((name, i));
    }
    let mut out = Vec::new();
    emit(&root, "", 0, collapsed, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(p: &str) -> FileEntry {
        FileEntry { path: p.into(), status: Some('M'), old_path: None, badges: Vec::new() }
    }

    #[test]
    fn folders_are_built_from_paths_dirs_first() {
        let files = vec![f("src/lib.rs"), f("README.md"), f("src/ui/a.rs"), f("src/ui/b.rs"), f("Cargo.toml")];
        let rows = file_rows(&files, true, &HashSet::new());
        let shown: Vec<(usize, &str, bool)> = rows.iter().map(|r| (r.depth, r.name.as_str(), r.dir)).collect();
        assert_eq!(
            shown,
            vec![(0, "src", true), (1, "ui", true), (2, "a.rs", false), (2, "b.rs", false), (1, "lib.rs", false), (0, "Cargo.toml", false), (0, "README.md", false)]
        );
        assert_eq!(rows[2].path, "src/ui/a.rs");
        assert_eq!(rows[2].entry, Some(2));
    }

    #[test]
    fn a_folded_folder_hides_its_children() {
        let files = vec![f("src/lib.rs"), f("src/ui/a.rs"), f("x.md")];
        let collapsed: HashSet<String> = ["src".to_owned()].into();
        let rows = file_rows(&files, true, &collapsed);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["src", "x.md"]);
        assert!(rows[0].folded);
        let flat = file_rows(&files, false, &collapsed);
        assert_eq!(flat.len(), 3, "the flat view has no folders to fold");
        assert_eq!(flat[1].name, "src/ui/a.rs");
    }

    #[test]
    fn hostile_paths_are_bounded() {
        let deep = vec!["d"; 200].join("/") + "/f.rs";
        let rows = file_rows(&[f(&deep), f(""), f("///"), f("a//b")], true, &HashSet::new());
        assert!(rows.iter().map(|r| r.depth).max().unwrap() < MAX_SEGMENTS);
        assert!(rows.iter().any(|r| r.name == "b"));
        let many: Vec<FileEntry> = (0..70_000).map(|i| f(&format!("d{i}/f"))).collect();
        assert!(file_rows(&many, true, &HashSet::new()).len() <= MAX_ROWS);
        assert!(file_rows(&many, false, &HashSet::new()).len() <= MAX_ROWS);
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(normalize_path("a\\b\\c.rs"), "a/b/c.rs");
        assert_eq!(normalize_path("./x/y"), "x/y");
        assert_eq!(normalize_path("/etc/passwd"), "etc/passwd");
    }
}
