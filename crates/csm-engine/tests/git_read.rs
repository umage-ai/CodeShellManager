#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! `git_diff`, `git_show_file` and `git_ls_files` end to end against a real repository: the three
//! diff sources, files at HEAD, in the index and in the working tree, the refusals (option-like
//! revisions, `..`, absolute paths, ignored files, a folder that is not absolute) and the rule that
//! viewing a diff never runs a program the repository's config names.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::H;
use csm_engine::ToolError;
use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=csm", "-c", "user.email=csm@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn real(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap().display().to_string();
    PathBuf::from(c.strip_prefix(r"\\?\").unwrap_or(&c))
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

/// A repository with two commits: `a.txt`, `src/lib.rs` (then changed), and an ignored `secret.env`.
fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let dir = real(tmp.path()).join("r");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(&dir, &["init", "-b", "main"]);
    std::fs::write(dir.join(".gitignore"), "secret.env\n").unwrap();
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "fn main() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "first"]);
    std::fs::write(dir.join("src/lib.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
    git(&dir, &["commit", "-am", "second"]);
    Repo { _tmp: tmp, dir }
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn err(r: Result<csm_engine::ToolOutput, ToolError>) -> String {
    match r {
        Ok(o) => panic!("expected an error, got {:?}", o.message),
        Err(e) => e.to_string(),
    }
}

fn files(d: &Value) -> Vec<(String, String)> {
    d["files"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap().to_owned(), f["status"].as_str().unwrap().to_owned())).collect()
}

#[test]
fn working_staged_and_range_diffs_list_files_and_patches() {
    let r = repo();
    let h = H::new();
    let folder = s(&r.dir);
    std::fs::write(r.dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(r.dir.join("new.txt"), "fresh\n").unwrap();
    std::fs::write(r.dir.join("secret.env"), "TOKEN=1\n").unwrap();
    std::fs::write(r.dir.join("src/lib.rs"), "fn main() {\n    println!(\"hi\");\n}\n// staged\n").unwrap();
    git(&r.dir, &["add", "src/lib.rs"]);
    std::fs::write(r.dir.join("src/lib.rs"), "fn main() {\n    println!(\"hi\");\n}\n// more\n").unwrap();

    let w = h.ok("git_diff", json!({"folder": folder}));
    assert_eq!(files(&w.data), vec![("a.txt".to_owned(), "M".to_owned()), ("src/lib.rs".to_owned(), "M".to_owned())]);
    assert_eq!(w.data["untracked"], json!(["new.txt"]), "untracked, and not the ignored file");
    assert!(w.data["head"].as_str().unwrap().len() >= 40);
    let patch = w.data["patch"].as_str().unwrap();
    assert!(patch.contains("diff --git a/a.txt b/a.txt") && patch.contains("-two") && patch.contains("+TWO"), "{patch}");
    assert_eq!(w.data["truncated"], false);

    let st = h.ok("git_diff", json!({"folder": folder, "source": "staged"}));
    assert_eq!(files(&st.data), vec![("src/lib.rs".to_owned(), "M".to_owned())]);
    assert!(!st.data["patch"].as_str().unwrap().contains("// more"), "the index, not the working tree");
    assert_eq!(st.data["untracked"], json!([]));

    let range = h.ok("git_diff", json!({"folder": folder, "source": "range", "base": "HEAD~1", "head": "HEAD"}));
    assert_eq!(files(&range.data), vec![("src/lib.rs".to_owned(), "M".to_owned())]);
    assert!(range.data["patch"].as_str().unwrap().contains("println"));

    // Names only, restricted to a path, with no context lines.
    let names = h.ok("git_diff", json!({"folder": folder, "patch": false, "paths": ["a.txt"]}));
    assert_eq!(files(&names.data), vec![("a.txt".to_owned(), "M".to_owned())]);
    assert_eq!(names.data["patch"], "");
    let zero = h.ok("git_diff", json!({"folder": folder, "paths": ["a.txt"], "context": 0}));
    assert!(zero.data["patch"].as_str().unwrap().lines().all(|l| l != " one" && l != " three"), "no context lines");

    // A rename is one entry with both names.
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["mv", "a.txt", "b.txt"]);
    let mv = h.ok("git_diff", json!({"folder": folder, "source": "staged", "patch": false}));
    assert!(mv.data["files"].as_array().unwrap().iter().any(|f| f["status"] == "R" && f["old_path"] == "a.txt" && f["path"] == "b.txt"), "{}", mv.data);

    // Cut at max_bytes, and it says so.
    let cut = h.ok("git_diff", json!({"folder": folder, "source": "range", "base": "HEAD~1", "max_bytes": 40}));
    assert_eq!(cut.data["truncated"], true);
    assert!(cut.data["patch"].as_str().unwrap().len() <= 40);
}

#[test]
fn a_session_can_be_named_instead_of_a_folder_and_an_unborn_branch_works() {
    let r = repo();
    let h = H::new();
    let id = h.ok("session_new", json!({"folder": s(&r.dir), "command": "pwsh"})).data["id"].as_str().unwrap().to_owned();
    let d = h.ok("git_diff", json!({"session_id": id, "source": "range", "base": "HEAD~1"}));
    assert_eq!(files(&d.data).len(), 1);
    let both = err(h.ui("git_diff", json!({"session_id": id, "folder": s(&r.dir)})));
    assert!(both.contains("not both"), "{both}");
    assert!(err(h.ui("git_diff", json!({}))).contains("session_id"));

    let fresh = tempfile::tempdir().unwrap();
    let dir = real(fresh.path());
    git(&dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("x.txt"), "x\n").unwrap();
    git(&dir, &["add", "x.txt"]);
    std::fs::write(dir.join("y.txt"), "y\n").unwrap();
    let d = h.ok("git_diff", json!({"folder": s(&dir)}));
    assert_eq!(d.data["head"], Value::Null);
    assert_eq!(files(&d.data), vec![("x.txt".to_owned(), "A".to_owned())]);
    assert_eq!(d.data["untracked"], json!(["y.txt"]));
}

#[test]
fn files_are_read_at_a_revision_in_the_index_and_in_the_working_tree() {
    let r = repo();
    let h = H::new();
    let folder = s(&r.dir);
    std::fs::write(r.dir.join("a.txt"), "one\nWORKING\nthree\n").unwrap();
    git(&r.dir, &["add", "a.txt"]);
    std::fs::write(r.dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();

    let head = h.ok("git_show_file", json!({"folder": folder, "rev": "HEAD", "path": "a.txt"}));
    assert_eq!(head.data["text"], "one\ntwo\nthree\n");
    assert_eq!(head.data["binary"], false);
    let blob = head.data["blob"].as_str().unwrap().to_owned();
    assert_eq!(blob, git(&r.dir, &["rev-parse", "HEAD:a.txt"]).trim());
    let index = h.ok("git_show_file", json!({"folder": folder, "rev": ":0", "path": "a.txt"}));
    assert_eq!(index.data["text"], "one\nWORKING\nthree\n");
    let tree = h.ok("git_show_file", json!({"folder": folder, "path": "a.txt"}));
    assert_eq!(tree.data["text"], "one\nTWO\nthree\n");
    assert_ne!(tree.data["blob"], index.data["blob"]);
    assert_eq!(tree.data["blob"], git(&r.dir, &["hash-object", "--no-filters", "a.txt"]).trim());
    let old = h.ok("git_show_file", json!({"folder": folder, "rev": "HEAD~1", "path": "src/lib.rs"}));
    assert_eq!(old.data["text"], "fn main() {}\n");
    let sub = h.ok("git_show_file", json!({"folder": s(&r.dir.join("src")), "path": "src/lib.rs"}));
    assert!(sub.data["text"].as_str().unwrap().contains("println"), "paths are relative to the repository, not to the folder given");

    // Binary files are named, not shown; a long file is cut and says so.
    std::fs::write(r.dir.join("bin.dat"), b"abc\0def").unwrap();
    let bin = h.ok("git_show_file", json!({"folder": folder, "path": "bin.dat"}));
    assert_eq!((bin.data["binary"].clone(), bin.data["text"].clone()), (json!(true), json!("")));
    std::fs::write(r.dir.join("big.txt"), "line of text\n".repeat(1000)).unwrap();
    let big = h.ok("git_show_file", json!({"folder": folder, "path": "big.txt", "max_bytes": 100}));
    assert_eq!(big.data["truncated"], true);
    assert!(big.data["text"].as_str().unwrap().len() <= 100);
    assert_eq!(big.data["size"], 13_000);
}

#[test]
fn a_hostile_revision_path_or_folder_is_refused_before_git_runs() {
    let r = repo();
    let h = H::new();
    let folder = s(&r.dir);
    for rev in ["--output=pwned.txt", "-p", "a..b", "HEAD;ls", "HEAD x", ""] {
        assert!(h.ui("git_show_file", json!({"folder": folder, "rev": rev, "path": "a.txt"})).is_err(), "{rev:?}");
        assert!(h.ui("git_ls_files", json!({"folder": folder, "rev": rev})).is_err(), "{rev:?}");
    }
    assert!(h.ui("git_diff", json!({"folder": folder, "source": "range", "base": "--help"})).is_err());
    assert!(h.ui("git_diff", json!({"folder": folder, "source": "range"})).is_err(), "range needs a base");
    assert!(h.ui("git_diff", json!({"folder": folder, "source": "nonsense"})).is_err());
    for path in ["../outside.txt", "/etc/passwd", "C:/Windows/win.ini", "a/../../b", "a\\b", "", ".git/config", ".GIT/HEAD", "bad\nname"] {
        assert!(h.ui("git_show_file", json!({"folder": folder, "path": path})).is_err(), "{path:?}");
        assert!(h.ui("git_diff", json!({"folder": folder, "paths": [path]})).is_err(), "{path:?}");
    }
    assert!(h.ui("git_diff", json!({"folder": folder, "paths": (0..65).map(|i| format!("f{i}")).collect::<Vec<_>>()})).is_err());
    assert!(h.ui("git_show_file", json!({"folder": "relative/dir", "path": "a.txt"})).is_err());
    assert!(h.ui("git_show_file", json!({"folder": "", "path": "a.txt"})).is_err());
    assert!(!r.dir.join("pwned.txt").exists());

    // Git ignores the file, so the tool does not read it; nor does it list it.
    std::fs::write(r.dir.join("secret.env"), "TOKEN=1\n").unwrap();
    let e = err(h.ui("git_show_file", json!({"folder": folder, "path": "secret.env"})));
    assert!(e.contains("ignored"), "{e}");
    // A path that is not there, and one that is a folder.
    assert!(err(h.ui("git_show_file", json!({"folder": folder, "path": "nope.txt"}))).contains("does not exist"));
    assert!(h.ui("git_show_file", json!({"folder": folder, "path": "src"})).is_err());
    assert!(err(h.ui("git_show_file", json!({"folder": folder, "rev": "HEAD", "path": "nope.txt"}))).contains("not in HEAD"));
    // Not a repository at all.
    let plain = tempfile::tempdir().unwrap();
    assert!(h.ui("git_diff", json!({"folder": s(&real(plain.path()))})).is_err());
    assert!(h.ui("git_show_file", json!({"folder": s(&real(plain.path())), "path": "a.txt"})).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_repository_is_not_followed() {
    let r = repo();
    let h = H::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.txt"), "secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("private.txt"), r.dir.join("link.txt")).unwrap();
    let e = err(h.ui("git_show_file", json!({"folder": s(&r.dir), "path": "link.txt"})));
    assert!(e.contains("outside the repository"), "{e}");
}

#[test]
fn viewing_a_diff_never_runs_a_program_the_repository_names() {
    let r = repo();
    let h = H::new();
    // If git ran these, it would fail: the programs do not exist.
    git(&r.dir, &["config", "diff.external", "csm-no-such-diff-program"]);
    git(&r.dir, &["config", "diff.csmx.textconv", "csm-no-such-textconv-program"]);
    git(&r.dir, &["config", "diff.noprefix", "true"]);
    std::fs::write(r.dir.join(".gitattributes"), "*.txt diff=csmx\n").unwrap();
    std::fs::write(r.dir.join("a.txt"), "one\nchanged\nthree\n").unwrap();
    let d = h.ok("git_diff", json!({"folder": s(&r.dir)}));
    let patch = d.data["patch"].as_str().unwrap();
    assert!(patch.contains("+changed") && patch.contains("diff --git a/a.txt b/a.txt"), "no external diff, no textconv, and the a/ b/ prefixes hold: {patch}");
}

#[test]
fn ls_files_lists_the_index_or_a_tree() {
    let r = repo();
    let h = H::new();
    let folder = s(&r.dir);
    std::fs::write(r.dir.join("later.txt"), "x\n").unwrap();
    git(&r.dir, &["add", "later.txt"]);
    let idx = h.ok("git_ls_files", json!({"folder": folder}));
    let list: Vec<&str> = idx.data["files"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(list, vec![".gitignore", "a.txt", "later.txt", "src/lib.rs"]);
    assert_eq!(idx.data["truncated"], false);
    let head = h.ok("git_ls_files", json!({"folder": folder, "rev": "HEAD"}));
    assert_eq!(head.data["files"], json!([".gitignore", "a.txt", "src/lib.rs"]));
    // A file with a space or a non-ASCII name is returned as it is, not quoted.
    std::fs::write(r.dir.join("håndbog notes.md"), "x\n").unwrap();
    git(&r.dir, &["add", "-A"]);
    let all = h.ok("git_ls_files", json!({"folder": folder}));
    assert!(all.data["files"].as_array().unwrap().iter().any(|f| f == "håndbog notes.md"), "{}", all.data);
}
