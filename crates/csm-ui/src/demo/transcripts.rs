//! Terminal content for the demo's Claude sessions, as ANSI bytes, after the mock-up's
//! transcripts. Fed into [`crate::terminal::StaticTerminal`]s: the review can see what a busy
//! pane looks like without running `claude`.

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
/// Box width in cells: narrow enough for a side-by-side pane.
const BOX_W: usize = 56;
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
/// Claude's orange.
const CLAUDE: &str = "\x1b[38;2;232;149;111m";
/// Row backgrounds for the user line and diffs: (user, add, del), dark and light.
const BG_DARK: [&str; 3] = ["\x1b[48;2;42;43;61m", "\x1b[48;2;36;58;48m", "\x1b[48;2;64;40;52m"];
const BG_LIGHT: [&str; 3] = ["\x1b[48;2;230;233;239m", "\x1b[48;2;214;236;208m", "\x1b[48;2;248;212;219m"];

struct T(String, [&'static str; 3]);

impl T {
    fn new(dark: bool) -> T {
        T(String::new(), if dark { BG_DARK } else { BG_LIGHT })
    }
    fn line(mut self, s: &str) -> T {
        self.0.push_str(s);
        self.0.push_str("\r\n");
        self
    }
    fn br(self) -> T {
        self.line("")
    }
    fn user(self, s: &str) -> T {
        let bg = self.1[0];
        self.line(&format!("{bg}> {s}{RESET}"))
    }
    fn say(self, s: &str) -> T {
        self.line(&format!("● {s}"))
    }
    fn tool(self, name: &str, arg: &str, ok: bool) -> T {
        let dot = if ok { GREEN } else { RED };
        self.line(&format!("{dot}●{RESET} {BOLD}{name}{RESET}({arg})"))
    }
    fn res(self, s: &str) -> T {
        self.line(&format!("  {DIM}⎿{RESET}  {s}"))
    }
    fn r2(self, s: &str) -> T {
        self.line(&format!("     {s}"))
    }
    fn add(self, n: u32, s: &str) -> T {
        let bg = self.1[1];
        self.line(&format!("     {bg}{DIM}{n:>4}{RESET}{bg} +{s}{RESET}"))
    }
    fn del(self, n: u32, s: &str) -> T {
        let bg = self.1[2];
        self.line(&format!("     {bg}{DIM}{n:>4}{RESET}{bg} -{s}{RESET}"))
    }
    fn spin(self, what: &str, meta: &str) -> T {
        self.line(&format!("{CLAUDE}✻ {what}{RESET} {DIM}({meta}){RESET}"))
    }
    fn input(self, hint: &str) -> T {
        let bar = "─".repeat(BOX_W);
        self.line(&format!("{DIM}╭{bar}╮{RESET}"))
            .line(&format!("{DIM}│{RESET} > {}{DIM}│{RESET}", " ".repeat(BOX_W - 3)))
            .line(&format!("{DIM}╰{bar}╯{RESET}"))
            .line(&format!("  {hint}"))
    }
    fn boxed(mut self, color: &str, lines: &[&str]) -> T {
        let bar = "─".repeat(BOX_W);
        self = self.line(&format!("{color}╭{bar}╮{RESET}"));
        for l in lines {
            self = self.line(&format!(" {l}"));
        }
        self.line(&format!("{color}╰{bar}╯{RESET}"))
    }
    /// Bottom-aligned in a terminal of `rows`: blank lines first, so the end of the transcript is
    /// what a pane shows, as in the mock-up.
    fn done(self, rows: usize) -> Vec<u8> {
        let n = self.0.matches("\r\n").count();
        let pad = rows.saturating_sub(n + 1);
        let mut out = "\r\n".repeat(pad);
        out.push_str(&self.0);
        out.into_bytes()
    }
}

fn accept() -> String {
    format!("{MAGENTA}⏵⏵ accept edits on (shift+tab to cycle){RESET}")
}

/// The transcript for a demo session id at its status, or `None` (a live shell, or nothing).
pub fn for_session(id: &str, status: crate::model::Status, rows: usize, dark: bool) -> Option<Vec<u8>> {
    use crate::model::Status as S;
    let t = match id {
        "odulodo" => {
            let head = T::new(dark)
                .user("the OrderTotals refactor is done, run the tests before you commit")
                .br()
                .tool("Update", "src/Odulodo.Core/Orders/OrderTotals.cs", true)
                .res(&format!("Updated {BOLD}src/Odulodo.Core/Orders/OrderTotals.cs{RESET} with 31 additions and 44 removals"))
                .del(112, "    var total = lines.Sum(l => l.Price * l.Quantity);")
                .add(112, "    var total = lines.Sum(l => l.LineTotal);")
                .add(113, "    total = _discounts.Apply(total, order.Customer);")
                .br()
                .say("I'll run the unit tests for the solution.")
                .br();
            if status == S::Approval {
                head.boxed(
                    "\x1b[38;2;250;179;135m",
                    &[
                        &format!("{BOLD}Bash command{RESET}"),
                        "",
                        "  dotnet test tests/Odulodo.Tests --no-build",
                        &format!("  {DIM}Run the unit tests after the OrderTotals refactor{RESET}"),
                        "",
                        "Do you want to proceed?",
                        &format!("{BLUE}❯ 1. Yes{RESET}"),
                        &format!("  2. Yes, and don't ask again for {BOLD}dotnet test{RESET} commands in C:\\GitHub\\Odulodo"),
                        &format!("  3. No, and tell Claude what to do differently {DIM}(esc){RESET}"),
                    ],
                )
            } else {
                head.tool("Bash", "dotnet test tests/Odulodo.Tests --no-build", true)
                    .res(&format!("{DIM}Determining projects to restore...{RESET}"))
                    .r2(&format!("{DIM}Starting test execution, please wait...{RESET}"))
                    .spin("Testing…", "9s · ↓ 1.2k tokens · esc to interrupt")
                    .input(&accept())
            }
        }
        "web" => T::new(dark)
            .user("add a brand facet to the search page, same pattern as category")
            .br()
            .say("I'll look at how the category facet is wired first.")
            .br()
            .tool("Search", "pattern: \"category\", path: \"src/search\"", true)
            .res(&format!("Found {BOLD}7{RESET} files"))
            .tool("Read", "src/search/facets.ts", true)
            .res(&format!("Read {BOLD}182{RESET} lines"))
            .tool("Update", "src/search/facets.ts", true)
            .res(&format!("Updated {BOLD}src/search/facets.ts{RESET} with 9 additions"))
            .add(58, "  brand: {")
            .add(59, "    field: \"brand.keyword\",")
            .add(60, "    label: \"Brand\",")
            .add(61, "    size: 20,")
            .add(62, "  },")
            .tool("Update", "src/search/FacetPanel.tsx", true)
            .res(&format!("Updated {BOLD}src/search/FacetPanel.tsx{RESET} with 3 additions and 1 removal"))
            .br()
            .tool("Update Todos", "", true)
            .res(&format!("{DIM}☒ \x1b[9mAdd brand to the facet registry{RESET}"))
            .r2(&format!("{DIM}☒ \x1b[9mRender the brand facet in FacetPanel{RESET}"))
            .r2(&format!("{BOLD}☐ Map the brand facet in the search API query{RESET}"))
            .r2("☐ Update the facet snapshot tests")
            .br()
            .spin("Mapping…", "48s · ↑ 3.4k tokens · esc to interrupt")
            .input(&accept()),
        "wt" => {
            let head = T::new(dark)
                .user("make the search results sortable")
                .br()
                .tool("Read", "src/search/api/query.ts", true)
                .res(&format!("Read {BOLD}240{RESET} lines"))
                .tool("Search", "pattern: \"sort\", path: \"src/cms\"", true)
                .res(&format!("Found {BOLD}3{RESET} files"))
                .br()
                .say("The CMS listing pages sort by publish date, but the search API only knows relevance.")
                .line("  Before I change the API, one decision.")
                .br();
            if status == S::Question {
                head.boxed(
                    BLUE,
                    &[
                        &format!("{BLUE}{BOLD}Sort order{RESET}"),
                        "Which sort options should the results page offer?",
                        "",
                        &format!("{BLUE}❯ 1. Relevance, price and newest{RESET}"),
                        "  2. Relevance and price only",
                        "  3. Whatever the CMS listing pages use",
                        "  4. Type something else",
                        "",
                        &format!("{DIM}Enter to select · ↑/↓ to navigate · Esc to cancel{RESET}"),
                    ],
                )
            } else {
                head.say(&format!("Going with relevance, price and newest. Adding a {BOLD}sort{RESET} parameter."))
                    .spin("Wiring…", "31s · ↑ 2.2k tokens · esc to interrupt")
                    .input(&accept())
            }
        }
        "csm" => T::new(dark)
            .user("bump to v0.11.1 and add the Recent menu focus fix to the release notes")
            .br()
            .tool("Update", "src/CodeShellManager/CodeShellManager.csproj", true)
            .res("Updated with 1 addition and 1 removal")
            .del(7, "    <Version>0.11.0</Version>")
            .add(7, "    <Version>0.11.1</Version>")
            .tool("Update", "CHANGELOG.md", true)
            .res(&format!("Updated {BOLD}CHANGELOG.md{RESET} with 6 additions"))
            .tool("Bash", "dotnet build src/CodeShellManager/CodeShellManager.csproj -c Release", true)
            .res(&format!("{GREEN}Build succeeded.{RESET}"))
            .r2("0 Warning(s)")
            .r2("0 Error(s)")
            .br()
            .say("The version is 0.11.1 and the changelog has an entry for the Recent menu focus fix (#196).")
            .line("  The Release build is clean. I did not tag it: the tag push starts CI, so that one is yours.")
            .br()
            .input(&format!("{DIM}? for shortcuts{RESET}")),
        "rs" => T::new(dark)
            .user("phase 0: render the alacritty_terminal grid in an egui widget, dirty rows only")
            .br()
            .tool("Read", "crates/csm-term/src/view.rs", true)
            .res(&format!("Read {BOLD}96{RESET} lines"))
            .tool("Write", "crates/csm-term/src/atlas.rs", true)
            .res(&format!("Wrote {BOLD}143{RESET} lines to crates/csm-term/src/atlas.rs"))
            .tool("Bash", "cargo run -p csm-term --example spike --release", true)
            .res(&format!("{GREEN}Compiling{RESET} csm-pty v0.1.0 (crates/csm-pty)"))
            .r2(&format!("{GREEN}Compiling{RESET} csm-term v0.1.0 (crates/csm-term)"))
            .r2(&format!("{GREEN}Finished{RESET} `release` profile [optimized] in 41.2s"))
            .r2(&format!("{DIM}spike: 20 panes, 60.0 fps, 3 dirty rows/frame{RESET}"))
            .br()
            .spin("Measuring…", "2m 14s · ↓ 6.1k tokens · esc to interrupt")
            .input(&accept()),
        "craft" => T::new(dark)
            .user("wire the new focus ring into the dropdown rows")
            .br()
            .tool("Update", "crates/ui-egui/src/widgets.rs", true)
            .res("Updated with 12 additions and 3 removals")
            .tool("Bash", "cargo test -p craft-anything-ui-egui", false)
            .res(&format!("{RED}error[E0425]: cannot find function `text_focus_ring` in module `theme`{RESET}"))
            .r2(&format!("{DIM}--> crates/ui-egui/src/widgets.rs:412:9{RESET}"))
            .br()
            .line(&format!("{RED}⎿  API Error (529 Overloaded) · Retried 10 times. The turn ended before Claude could fix the build.{RESET}"))
            .br()
            .input(&format!("{DIM}? for shortcuts{RESET}")),
        "gw" => T::new(dark)
            .user("the rate limiter drops requests under burst, find out why")
            .br()
            .tool("Read", "src/limiter/token_bucket.rs", true)
            .res(&format!("Read {BOLD}118{RESET} lines"))
            .tool("Bash", "cargo test limiter -- --nocapture", true)
            .res(&format!("test limiter::burst_of_50 ... {RED}FAILED{RESET}"))
            .br()
            .say("The refill uses whole seconds, so a burst inside one second sees no refill at all.")
            .spin("Patching…", "1m 02s · ↑ 1.9k tokens")
            .input(&accept()),
        "build" => T::new(dark)
            .line(&format!("{DIM}Last login: Thu Nov 19 08:12:44 2026 from 10.0.4.21{RESET}"))
            .line(&format!("{GREEN}deploy@build-01{RESET}:{BLUE}~{RESET}$ cd /srv/odulodo && docker compose ps"))
            .line("NAME             IMAGE                  STATUS")
            .line("odulodo-web      odulodo/web:2.14.0     Up 3 days (healthy)")
            .line("odulodo-api      odulodo/api:2.14.0     Up 3 days (healthy)")
            .line("odulodo-search   opensearch:2.17.1      Up 3 days")
            .line(&format!("{GREEN}deploy@build-01{RESET}:{BLUE}/srv/odulodo{RESET}$ ")),
        "webps" => T::new(dark)
            .line("PowerShell 7.5.4")
            .line(&format!("PS C:\\GitHub\\Odulodo.Web> {BOLD}git status -sb{RESET}"))
            .line(&format!("{GREEN}## feat/brand-facet...origin/feat/brand-facet{RESET} [ahead 2]"))
            .line(&format!(" {RED}M{RESET} src/search/FacetPanel.tsx"))
            .line(&format!(" {RED}M{RESET} src/search/facets.ts"))
            .line(&format!(" {RED}M{RESET} src/search/api/query.ts"))
            .line(&format!("PS C:\\GitHub\\Odulodo.Web> {BOLD}npm run lint{RESET}"))
            .br()
            .line(&format!("{DIM}> odulodo-web@2.14.0 lint{RESET}"))
            .line(&format!("{DIM}> eslint src --max-warnings 0{RESET}"))
            .br()
            .line("PS C:\\GitHub\\Odulodo.Web> "),
        _ => return None,
    };
    let mut bytes = t.done(rows);
    // Leave the cursor where the input sits, not after a trailing newline.
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    }
    Some(bytes)
}

/// A freshly started Claude session (quick-open with `claude` in the demo).
pub fn welcome(folder: &str, rows: usize, dark: bool) -> Vec<u8> {
    T::new(dark)
        .boxed(
            CLAUDE,
            &[
                &format!("{CLAUDE}✻{RESET} Welcome to {BOLD}Claude Code{RESET}!"),
                "",
                &format!("{DIM}/help for help, /status for your current setup{RESET}"),
                "",
                &format!("{DIM}cwd: {folder}{RESET}"),
            ],
        )
        .br()
        .line(&format!("{DIM}Tip: run /init to create a CLAUDE.md file with instructions for Claude{RESET}"))
        .input(&format!("{DIM}? for shortcuts{RESET}"))
        .done(rows)
}
