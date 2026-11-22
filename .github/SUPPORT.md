# Getting Help

- **Usage questions, ideas, "how do I…?"** → [Discussions](https://github.com/umage-ai/CodeShellManager/discussions)
- **Bugs and crashes** → [open a bug report](https://github.com/umage-ai/CodeShellManager/issues/new?template=bug_report.yml)
- **Feature requests** → [open a feature request](https://github.com/umage-ai/CodeShellManager/issues/new?template=feature_request.yml)
- **Security issues** → see [SECURITY.md](SECURITY.md); please do not file a public issue
- **Code of Conduct concerns** → see [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

When filing a bug, the most useful things to include are:

- The output of `csm doctor` (it checks the install, the bundled ConPTY and the MCP server, and
  prints a fix for anything wrong; add `--json` for the full detail).
- The **Diagnostics** section of Settings → About (version, platform, which ConPTY is in use).
- Whether it still happens with `codeshell --clean` (no saved sessions, nothing saved).
