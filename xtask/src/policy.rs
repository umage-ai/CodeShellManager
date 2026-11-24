//! Source rules, checked with `syn` (CLAUDE.md "Rules"):
//!
//! 1. `std::process::Command` only in the modules that own processes: `csm-engine/src/proc.rs`
//!    (probes, with `CREATE_NO_WINDOW`), `csm-pty` (the PTY children), `csm-setup` (agent config
//!    and the doctor's self-test) and this crate.
//! 2. No `std::fs`, `std::process`, `std::net`, `thread::sleep` or blocking `.recv()` in UI frame
//!    code (`csm-ui`'s screens and widgets): send the work to a background thread.
//! 3. `unsafe` only in `csm-pty`'s ConPTY module; every other crate root says
//!    `#![forbid(unsafe_code)]`; no `#[allow(unsafe_code)]` elsewhere.
//! 4. Crate roots of production crates deny `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/
//!    `unreachable!` (`DENY_ROOTS`).
//!
//! Items under `#[cfg(test)]` or `#[test]`, and `tests/`, `benches/` and `examples/` folders, are
//! not checked. A line may opt out with a `// policy-ok: <reason>` comment on it (rule 2 only).

use std::collections::HashMap;
use std::path::Path;

use proc_macro2::Span;
use syn::visit::Visit;

use crate::Findings;

/// Files where `std::process::Command` may be named (a prefix match on `/`-separated paths).
///
/// `crates/csm-services/src/git/run.rs` is the git process layer: it needs what `csm_engine::proc`
/// doesn't give (stderr, the git environment and `WSLENV`, the builders' MSVCRT-quoted raw argv,
/// and a whole-tree kill on timeout, since a fetch's real work is in child processes), and
/// csm-services (L2) can't depend on the engine (L3). Same guarantees as `proc`: no console
/// window, closed stdin, a deadline, capped capture; blocking, called from workers only.
///
const PROCESS_ALLOWED: &[&str] =
    &["crates/csm-engine/src/proc.rs", "crates/csm-pty/", "crates/csm-setup/src/", "crates/csm-services/src/git/run.rs", "xtask/"];
/// Where `unsafe` may appear.
const UNSAFE_ALLOWED: &[&str] = &["crates/csm-pty/src/conpty.rs", "crates/csm-pty/src/conpty/"];
/// UI frame code: nothing here may touch the disk, spawn, or wait.
const UI_FRAME: &[&str] =
    &["crates/csm-ui/src/screens/", "crates/csm-ui/src/widgets.rs", "crates/csm-ui/src/terminal/view.rs", "crates/csm-ui/src/terminal/render.rs"];
/// Crate roots that must carry the clippy denies. Extend as each crate is cleaned up.
const DENY_ROOTS: &[&str] = &["vendor/csm-remote-proto/", "crates/csm-peer-proto/", "crates/csm-peer/", "crates/csm-remote/", "crates/csm-mcp/", "crates/csm-setup/", "apps/csm-cli/", "apps/csm-session-summary/", "apps/csm-review/", "crates/csm-pty/", "xtask/"];
const DENY_LINTS: &[&str] = &["unwrap_used", "expect_used", "panic", "unimplemented", "todo", "unreachable"];

pub fn check() -> Result<Findings, String> {
    let root = crate::root();
    let mut problems = Vec::new();
    for dir in ["crates", "apps"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        let mut crates: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        crates.sort();
        for c in crates {
            walk(&c.join("src"), &root, &mut problems);
        }
    }
    walk(&root.join("xtask").join("src"), &root, &mut problems);
    // vendor/csm-remote-proto is outside the workspace (vendor/*) but its sources are checked like CSM's own.
    walk(&root.join("vendor").join("csm-remote-proto").join("src"), &root, &mut problems);
    Ok(problems)
}

fn walk(dir: &Path, root: &Path, problems: &mut Findings) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            walk(&p, root, problems);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            match std::fs::read_to_string(&p) {
                Ok(text) => problems.extend(check_source(&rel, &text)),
                Err(e) => problems.push(format!("{rel}: could not read: {e}")),
            }
        }
    }
}

fn allowed(list: &[&str], file: &str) -> bool {
    list.iter().any(|p| file == *p || (p.ends_with('/') && file.starts_with(p)))
}

/// Is `file` a crate root (`src/lib.rs` or `src/main.rs`)?
fn is_root(file: &str) -> bool {
    file.ends_with("/src/lib.rs") || file.ends_with("/src/main.rs")
}

/// All problems in one source file. `file` is the `/`-separated path from the repository root.
pub fn check_source(file: &str, text: &str) -> Findings {
    let parsed = match syn::parse_file(text) {
        Ok(f) => f,
        Err(e) => return vec![format!("{file}: does not parse: {e}")],
    };
    let mut v = Policy {
        file,
        process_ok: allowed(PROCESS_ALLOWED, file),
        unsafe_ok: allowed(UNSAFE_ALLOWED, file),
        ui_frame: allowed(UI_FRAME, file),
        lines: text.lines().collect(),
        aliases: HashMap::new(),
        problems: Vec::new(),
    };
    v.collect_aliases(&parsed.items);
    if is_root(file) {
        v.check_root(&parsed.attrs);
    }
    v.visit_file(&parsed);
    v.problems
}

struct Policy<'a> {
    file: &'a str,
    process_ok: bool,
    unsafe_ok: bool,
    ui_frame: bool,
    lines: Vec<&'a str>,
    /// `use` names to their full paths, so `Command` resolves to `std::process::Command`.
    aliases: HashMap<String, Vec<String>>,
    problems: Findings,
}

fn flatten(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            flatten(&p.tree, prefix, out);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            let mut full = prefix.clone();
            full.push(n.ident.to_string());
            out.push((n.ident.to_string(), full));
        }
        syn::UseTree::Rename(r) => {
            let mut full = prefix.clone();
            full.push(r.ident.to_string());
            out.push((r.rename.to_string(), full));
        }
        syn::UseTree::Glob(_) => out.push(("*".to_owned(), prefix.clone())),
        syn::UseTree::Group(g) => {
            for t in &g.items {
                flatten(t, prefix, out);
            }
        }
    }
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let text = quote_attr(a);
        a.path().is_ident("test") || (a.path().is_ident("cfg") && text.contains("test") && !text.contains("not (test") && !text.contains("not(test"))
    })
}

fn quote_attr(a: &syn::Attribute) -> String {
    use quote_free::ToStr;
    a.meta.to_str()
}

/// `syn` without `quote`: tokens to text through `proc_macro2`'s own `Display`.
mod quote_free {
    pub trait ToStr {
        fn to_str(&self) -> String;
    }
    impl ToStr for syn::Meta {
        fn to_str(&self) -> String {
            match self {
                syn::Meta::Path(p) => p.segments.iter().map(|s| s.ident.to_string()).collect::<Vec<_>>().join("::"),
                syn::Meta::List(l) => {
                    format!("{} ({})", l.path.segments.iter().map(|s| s.ident.to_string()).collect::<Vec<_>>().join("::"), l.tokens)
                }
                syn::Meta::NameValue(n) => n.path.segments.iter().map(|s| s.ident.to_string()).collect::<Vec<_>>().join("::"),
            }
        }
    }
}

impl Policy<'_> {
    fn collect_aliases(&mut self, items: &[syn::Item]) {
        for item in items {
            match item {
                syn::Item::Use(u) if !has_cfg_test(&u.attrs) => {
                    let mut out = Vec::new();
                    flatten(&u.tree, &mut Vec::new(), &mut out);
                    self.aliases.extend(out);
                }
                syn::Item::Mod(m) if !has_cfg_test(&m.attrs) => {
                    if let Some((_, inner)) = &m.content {
                        self.collect_aliases(inner);
                    }
                }
                _ => {}
            }
        }
    }

    fn at(&self, span: Span) -> String {
        format!("{}:{}", self.file, span.start().line)
    }

    fn opted_out(&self, span: Span) -> bool {
        self.lines.get(span.start().line.saturating_sub(1)).is_some_and(|l| l.contains("policy-ok:"))
    }

    fn resolve(&self, segments: &[String]) -> Vec<String> {
        match segments.first().and_then(|f| self.aliases.get(f)) {
            Some(full) => full.iter().chain(segments.iter().skip(1)).cloned().collect(),
            None => segments.to_vec(),
        }
    }

    fn check_path(&mut self, segments: Vec<String>, span: Span) {
        let full = self.resolve(&segments);
        let s: Vec<&str> = full.iter().map(String::as_str).collect();
        if !self.process_ok && s.starts_with(&["std", "process", "Command"]) {
            self.problems.push(format!("{}: `std::process::Command` outside the modules that own processes (use csm_engine::proc)", self.at(span)));
        }
        if self.ui_frame && !self.opted_out(span) {
            let banned = s.starts_with(&["std", "fs"])
                || s.starts_with(&["std", "process"])
                || s.starts_with(&["std", "net"])
                || s.starts_with(&["std", "thread", "sleep"])
                || s.starts_with(&["std", "io", "stdin"]);
            if banned {
                self.problems.push(format!(
                    "{}: `{}` in UI frame code (blocking or I/O; send it to a background thread and poll a channel)",
                    self.at(span),
                    s.join("::")
                ));
            }
        }
    }

    fn check_root(&mut self, attrs: &[syn::Attribute]) {
        let list = |name: &str| -> Vec<String> { attrs.iter().filter(|a| a.path().is_ident(name)).map(quote_attr).collect() };
        let forbids = list("forbid").iter().any(|t| t.contains("unsafe_code"));
        let denies_unsafe = list("deny").iter().any(|t| t.contains("unsafe_code"));
        let pty = self.file.starts_with("crates/csm-pty/");
        if !forbids && !(pty && denies_unsafe) {
            self.problems.push(format!("{}: crate root lacks `#![forbid(unsafe_code)]`", self.file));
        }
        if allowed(DENY_ROOTS, self.file) {
            let denies = list("deny");
            for lint in DENY_LINTS {
                if !denies.iter().any(|t| t.contains(lint)) {
                    self.problems.push(format!("{}: crate root lacks `#![deny(clippy::{lint})]`", self.file));
                }
            }
        }
    }

    fn unsafe_found(&mut self, span: Span, what: &str) {
        if !self.unsafe_ok {
            self.problems.push(format!("{}: {what} outside csm-pty's ConPTY module", self.at(span)));
        }
    }
}

impl<'ast> Visit<'ast> for Policy<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if !has_cfg_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if has_cfg_test(&f.attrs) {
            return;
        }
        if f.sig.unsafety.is_some() {
            self.unsafe_found(f.sig.fn_token.span, "`unsafe fn`");
        }
        syn::visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if has_cfg_test(&f.attrs) {
            return;
        }
        if f.sig.unsafety.is_some() {
            self.unsafe_found(f.sig.fn_token.span, "`unsafe fn`");
        }
        syn::visit::visit_impl_item_fn(self, f);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if has_cfg_test(&i.attrs) {
            return;
        }
        if let Some(u) = &i.unsafety {
            self.unsafe_found(u.span, "`unsafe impl`");
        }
        syn::visit::visit_item_impl(self, i);
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        if let Some(u) = &t.unsafety {
            self.unsafe_found(u.span, "`unsafe trait`");
        }
        syn::visit::visit_item_trait(self, t);
    }

    fn visit_item_foreign_mod(&mut self, m: &'ast syn::ItemForeignMod) {
        self.unsafe_found(m.abi.extern_token.span, "`extern` block");
        syn::visit::visit_item_foreign_mod(self, m);
    }

    fn visit_expr_unsafe(&mut self, e: &'ast syn::ExprUnsafe) {
        self.unsafe_found(e.unsafe_token.span, "`unsafe` block");
        syn::visit::visit_expr_unsafe(self, e);
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        // csm-pty's `lib.rs` declares the one exception (`#[allow(unsafe_code)] mod conpty;`).
        if a.path().is_ident("allow") && quote_attr(a).contains("unsafe_code") && !self.unsafe_ok && !self.file.starts_with("crates/csm-pty/") {
            self.problems.push(format!("{}: `allow(unsafe_code)` outside csm-pty's ConPTY module", self.at(a.pound_token.span)));
        }
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        if has_cfg_test(&u.attrs) {
            return;
        }
        let mut out = Vec::new();
        flatten(&u.tree, &mut Vec::new(), &mut out);
        let span = u.use_token.span;
        for (_, full) in out {
            self.check_path(full, span);
        }
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segments: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if let Some(first) = p.segments.first() {
            self.check_path(segments, first.ident.span());
        }
        syn::visit::visit_path(self, p);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if self.ui_frame && m.method == "recv" && m.args.is_empty() && !self.opted_out(m.method.span()) {
            self.problems.push(format!("{}: blocking `.recv()` in UI frame code (use `try_recv`)", self.at(m.method.span())));
        }
        syn::visit::visit_expr_method_call(self, m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: &str = "crates/csm-engine/src/launch.rs";

    #[test]
    fn process_command_is_flagged_however_it_is_named() {
        for src in [
            "fn f() { std::process::Command::new(\"x\"); }",
            "use std::process::Command; fn f() { Command::new(\"x\"); }",
            "use std::process::{Command, Stdio}; fn f() {}",
            "use std::process as p; fn f() { p::Command::new(\"x\"); }",
        ] {
            assert!(!check_source(ENGINE, src).is_empty(), "{src}");
        }
        // The same text is fine where processes live, and clap's Command is not std's.
        assert!(check_source("crates/csm-engine/src/proc.rs", "use std::process::Command; fn f() { Command::new(\"x\"); }").is_empty());
        assert!(check_source("apps/csm-cli/src/args.rs", "use clap::Command; fn f() { Command::new(\"x\"); }").is_empty());
    }

    #[test]
    fn test_code_is_exempt() {
        let src = "#[cfg(test)] mod tests { use std::process::Command; #[test] fn t() { Command::new(\"x\"); } }";
        assert!(check_source(ENGINE, src).is_empty());
    }

    #[test]
    fn ui_frame_code_may_not_block() {
        let ui = "crates/csm-ui/src/screens/panes.rs";
        for src in [
            "fn f() { let _ = std::fs::read(\"x\"); }",
            "use std::fs; fn f() { fs::read(\"x\").ok(); }",
            "fn f() { std::thread::sleep(d); }",
            "fn f(r: R) { r.recv(); }",
        ] {
            assert!(!check_source(ui, src).is_empty(), "{src}");
        }
        assert!(check_source(ui, "fn f(r: R) { r.try_recv(); }").is_empty());
        assert!(check_source("crates/csm-ui/src/engine_backend.rs", "fn f() { std::fs::read(\"x\").ok(); }").is_empty(), "not frame code");
        assert!(check_source(ui, "fn f() { let _ = std::fs::read(\"x\"); } // policy-ok: startup only").is_empty());
    }

    #[test]
    fn unsafe_is_confined_to_the_conpty_module() {
        let src = "fn f() { unsafe { g() } } unsafe fn h() {} unsafe impl Send for X {} extern \"C\" { fn x(); }";
        assert_eq!(check_source("crates/csm-engine/src/lib.rs", src).iter().filter(|p| p.contains("outside csm-pty")).count(), 4);
        assert!(check_source("crates/csm-pty/src/conpty.rs", src).is_empty());
        assert!(!check_source(ENGINE, "#![allow(unsafe_code)]").is_empty());
    }

    #[test]
    fn crate_roots_forbid_unsafe_and_production_roots_deny_unwrap() {
        let bad = check_source("crates/csm-mcp/src/lib.rs", "fn f() {}");
        assert!(bad.iter().any(|p| p.contains("forbid(unsafe_code)")) && bad.iter().any(|p| p.contains("unwrap_used")), "{bad:?}");
        let good = "#![forbid(unsafe_code)]\n#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]\n";
        assert!(check_source("crates/csm-mcp/src/lib.rs", good).is_empty());
        // csm-pty's root may deny rather than forbid.
        assert!(check_source("crates/csm-pty/src/lib.rs", &format!("#![deny(unsafe_code)]\n{good}")).is_empty());
        // A crate not yet on the deny list only needs the unsafe attribute.
        assert!(check_source("crates/csm-engine/src/lib.rs", "#![forbid(unsafe_code)]").is_empty());
    }
}
