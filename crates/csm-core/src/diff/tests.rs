use super::*;

const GIT: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,6 @@ fn main() {
 one
-two
+TWO
+two and a half
 three
 four
 five
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
--- a/old name.txt
+++ b/new name.txt
@@ -1 +1 @@
-a
\\ No newline at end of file
+b
\\ No newline at end of file
diff --git a/img.png b/img.png
index 3333333..4444444 100644
Binary files a/img.png and b/img.png differ
diff --git a/new.rs b/new.rs
new file mode 100644
index 0000000..5555555
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+fn a() {}
+fn b() {}
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-bye
";

#[test]
fn git_output_with_renames_binary_and_no_newline_markers() {
    let p = parse_unified(GIT);
    assert!(!p.truncated);
    assert_eq!(p.files.len(), 5);
    let f = &p.files[0];
    assert_eq!((f.path(), f.status, f.binary), ("src/lib.rs", FileStatus::Modified, false));
    assert_eq!(f.hunks.len(), 1);
    let h = &f.hunks[0];
    assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len, h.heading.as_str()), (1, 5, 1, 6, "fn main() {"));
    assert_eq!(h.lines.len(), 7);
    assert_eq!((h.lines[1].kind, h.lines[1].old_no, h.lines[1].new_no, h.lines[1].text.as_str()), (LineKind::Del, Some(2), None, "two"));
    assert_eq!((h.lines[2].kind, h.lines[2].old_no, h.lines[2].new_no), (LineKind::Add, None, Some(2)));
    assert_eq!((h.lines[4].old_no, h.lines[4].new_no), (Some(3), Some(4)), "numbers follow both sides");
    assert_eq!(f.counts(), (2, 1));

    let r = &p.files[1];
    assert_eq!((r.status, r.old_path.as_deref(), r.new_path.as_deref()), (FileStatus::Renamed, Some("old name.txt"), Some("new name.txt")));
    assert!(r.hunks[0].lines.iter().all(|l| l.no_eol), "the markers attach to the line before them");
    assert_eq!(r.hunks[0].lines.len(), 2, "the marker is not a line");

    let b = &p.files[2];
    assert!(b.binary && b.hunks.is_empty());
    assert_eq!(b.path(), "img.png");

    let n = &p.files[3];
    assert_eq!((n.status, n.path(), n.hunks[0].lines.len()), (FileStatus::Added, "new.rs", 2));
    let d = &p.files[4];
    assert_eq!((d.status, d.path()), (FileStatus::Deleted, "gone.rs"));
}

#[test]
fn a_plain_unified_diff_without_git_headers_and_a_dashed_deleted_line() {
    let patch = "--- a/x.txt\t2020-01-01\n+++ b/x.txt\t2020-01-02\n@@ -1,2 +1,2 @@\n--- not a header\n+++ not a header either\n ctx\n--- y.txt\n+++ y.txt\n@@ -5 +5 @@\n-old\n+new\n";
    let p = parse_unified(patch);
    assert_eq!(p.files.len(), 2);
    assert_eq!(p.files[0].path(), "x.txt");
    let l = &p.files[0].hunks[0].lines;
    assert_eq!(l.len(), 3, "counted lines are body even when they look like headers: {l:?}");
    assert_eq!(l[0].text, "-- not a header");
    assert_eq!(l[1].text, "++ not a header either");
    assert_eq!(p.files[1].path(), "y.txt");
    assert_eq!(p.files[1].hunks[0].old_start, 5);
}

#[test]
fn crlf_is_folded_and_other_control_characters_are_kept_for_the_renderer_to_mark() {
    let p = parse_unified("--- a/f\r\n+++ b/f\r\n@@ -1 +1 @@\r\n-a\r\n+b\u{1b}[31m\u{202e}\r\n");
    let l = &p.files[0].hunks[0].lines;
    assert_eq!(l[0].text, "a");
    assert_eq!(l[1].text, "b\u{1b}[31m\u{202e}", "the parser does not interpret or drop control characters");
    assert_eq!(p.files[0].path(), "f");
}

#[test]
fn quoted_paths_are_decoded() {
    let p = parse_unified("diff --git \"a/caf\\303\\251 \\\"x\\\".txt\" \"b/caf\\303\\251 \\\"x\\\".txt\"\n--- \"a/caf\\303\\251 \\\"x\\\".txt\"\n+++ \"b/caf\\303\\251 \\\"x\\\".txt\"\n@@ -1 +1 @@\n-a\n+b\n");
    assert_eq!(p.files[0].path(), "caf\u{e9} \"x\".txt");
}

#[test]
fn malformed_headers_and_truncated_hunks_do_not_panic_and_show_what_there_is() {
    // A header that does not parse: the hunk runs until the next header.
    let p = parse_unified("--- a/f\n+++ b/f\n@@ nonsense @@\n x\n-y\n+z\n@@ -9,1 +9,1 @@\n-q\n+r\n");
    assert_eq!(p.files[0].hunks.len(), 2);
    assert_eq!(p.files[0].hunks[0].lines.len(), 3);
    assert_eq!(p.files[0].hunks[0].old_start, 0);
    // The header promises more lines than there are.
    let p = parse_unified("--- a/f\n+++ b/f\n@@ -1,50 +1,50 @@\n a\n-b\ndiff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1 +1 @@\n-c\n+d\n");
    assert_eq!(p.files.len(), 2, "a `diff --git` line ends a hunk that ran short");
    assert_eq!(p.files[0].hunks[0].lines.len(), 2);
    // Garbage: no files, no panic.
    for s in ["", "\n", "hello", "@@", "@@ -1 +1 @@", "+++", "--- ", "\\", "diff --git", "Binary files", "@@ -99999999999 +1,2 @@\n+x", "\u{0}\u{1b}[2J"] {
        let _ = parse_unified(s);
    }
    let p = parse_unified("@@ -4000000000,2 +4000000000,2 @@\n a\n b\n");
    assert_eq!(p.files[0].hunks[0].lines[1].old_no, Some(4_000_000_001));
    let p = parse_unified("@@ -4294967295,3 +1,3 @@\n a\n b\n c\n");
    assert_eq!(p.files[0].hunks[0].lines[2].old_no, Some(u32::MAX), "numbers saturate");
}

#[test]
fn caps_are_enforced() {
    let long = "x".repeat(MAX_LINE_CHARS + 500);
    let p = parse_unified(&format!("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-{long}\n+ok\n"));
    let l = &p.files[0].hunks[0].lines[0];
    assert!(l.cut && l.text.chars().count() == MAX_LINE_CHARS);

    let mut many = String::from("--- a/f\n+++ b/f\n@@ -1,1 +1,300000 @@\n");
    for _ in 0..(MAX_LINES + 10) {
        many.push_str("+a\n");
    }
    let p = parse_unified(&many);
    assert!(p.truncated);
    assert_eq!(p.files[0].hunks[0].lines.len(), MAX_LINES);

    let mut files = String::new();
    for i in 0..(MAX_FILES + 3) {
        files.push_str(&format!("diff --git a/f{i} b/f{i}\n--- a/f{i}\n+++ b/f{i}\n@@ -1 +1 @@\n-a\n+b\n"));
    }
    let p = parse_unified(&files);
    assert_eq!(p.files.len(), MAX_FILES);
    assert!(p.truncated);
}

#[test]
fn binary_looking_text_is_still_text() {
    let p = parse_unified("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-\u{0}\u{1}\u{2}\n+\u{fffd}\u{fffd}\n");
    assert_eq!(p.files[0].hunks[0].lines.len(), 2);
    let p = parse_unified("diff --git a/x b/x\nGIT binary patch\nliteral 12\nzc$@(abc\n\nliteral 0\nHcmV?d00001\n\ndiff --git a/y b/y\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-a\n+b\n");
    assert!(p.files[0].binary && p.files[0].hunks.is_empty());
    assert_eq!(p.files[1].hunks.len(), 1, "the binary body is skipped to the next file");
}

#[test]
fn split_alignment_pairs_blocks_and_pads_the_shorter_side() {
    let p = parse_unified("--- a/f\n+++ b/f\n@@ -1,6 +1,7 @@\n ctx\n-d1\n-d2\n+a1\n+a2\n+a3\n mid\n-only del\n tail\n+only add\n");
    let h = &p.files[0].hunks[0];
    let rows = align_split(h);
    let show = |r: &SplitRow<'_>| (r.old.map(|c| (c.no, c.line.text.clone())), r.new.map(|c| (c.no, c.line.text.clone())));
    assert_eq!(show(&rows[0]), (Some((1, "ctx".into())), Some((1, "ctx".into()))));
    assert_eq!(show(&rows[1]), (Some((2, "d1".into())), Some((2, "a1".into()))));
    assert_eq!(show(&rows[2]), (Some((3, "d2".into())), Some((3, "a2".into()))));
    assert_eq!(show(&rows[3]), (None, Some((4, "a3".into()))), "a longer block has an empty cell opposite");
    assert_eq!(show(&rows[4]).0.map(|c| c.0), Some(4));
    assert_eq!(show(&rows[5]), (Some((5, "only del".into())), None));
    assert_eq!(show(&rows[7]), (None, Some((7, "only add".into()))));
    assert_eq!(rows.len(), 8);
    // A deletion after an addition starts a new block.
    let p = parse_unified("--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n+n\n-o\n");
    assert_eq!(align_split(&p.files[0].hunks[0]).len(), 2);
}
