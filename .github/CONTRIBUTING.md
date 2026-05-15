# Contributing to CodeShellManager

Thanks for considering a contribution. CodeShellManager is a small, opinionated WPF host for terminal CLIs (Claude Code, Codex, pwsh, ssh, etc.) — bug fixes and focused features are welcome.

## Before you start

- For bug fixes: open an issue first only if you want to confirm the diagnosis. Otherwise, just send the PR.
- For features: open an issue (or a Discussion) and get a thumbs-up before investing time. The bar for "is this in scope" is "would the maintainers use it daily?".
- Check [CLAUDE.md](../CLAUDE.md) for the architecture overview, conventions, and lifecycle invariants. It's the most useful single document for getting oriented.

## Build and run

Requires .NET 10 SDK and Windows 10/11 (the app uses ConPTY and WebView2).

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
dotnet run   --project src/CodeShellManager/CodeShellManager.csproj
```

Or open `CodeShellManager.slnx` in Visual Studio / Rider.

### Testing without touching real state

The `--clean` flag boots with no restored sessions and skips writing to `state.json` for the entire run. Use it whenever you're poking at lifecycle, restore, or settings code so your real session list isn't disturbed:

```bash
dotnet run --project src/CodeShellManager/CodeShellManager.csproj -- --clean
```

## Tests

```bash
dotnet test tests/CodeShellManager.Tests/
```

Unit tests cover model/service logic and run headless. UI tests live in `tests/CodeShellManager.UITests/` and require a live Windows desktop session — they're optional locally but run in CI.

If your change touches a model, service, or anything with a clean unit boundary, add a test. UI-only tweaks don't need one.

## Branch naming

- `feat/<short-name>` — new feature
- `fix/<short-name>` — bug fix
- `chore/<short-name>` — tooling, repo hygiene, dependency bumps
- `docs/<short-name>` — documentation only

## Commit messages

Conventional-commit-ish: `type(scope): subject` in lowercase, no trailing period.

```
feat(sidebar): group bulk actions, empty-area quick menu, sort
fix(shutdown): guard OnClosing against re-entry during async cleanup
perf: precompute Sessions dict in RebuildSidebarOrder to drop O(n^2)
docs: document OSC 9001 shell integration
```

The body should explain the *why* — the diff already shows the *what*.

## Pull requests

- Target `main`.
- Keep the PR small and focused. If you find an unrelated bug along the way, ship it in its own PR.
- Use the PR template — it asks for a summary, motivation, and a test plan checklist.
- CI must be green. The `build` workflow compiles and runs unit + UI tests.
- Expect a code review pass from the bot reviewer (Copilot) and a maintainer. Address the substantive points; nitpicks are negotiable.
- Don't squash commits manually before merge — the maintainer will pick the right merge strategy when landing.

## Issues and labels

Two labels are in active use:

- `bug` — auto-applied by the bug report template
- `enhancement` — auto-applied by the feature request template

Dependabot PRs get `dependencies` automatically.

Please don't open issues for usage questions — use [Discussions](https://github.com/umage-ai/CodeShellManager/discussions) instead.

## Code style

- Match what's already there. There's no autoformatter beyond `dotnet format` defaults.
- WPF code-behind for sidebar/terminal UI is intentional (see CLAUDE.md "Known Conventions"); don't move it to XAML templates.
- Catppuccin Mocha hex values only — no system colors.
- Comments explain *why*, not *what*. If a comment would just restate the code, leave it out.

## License

By contributing, you agree your contribution is licensed under the same license as the project (see [LICENSE](../LICENSE)).
