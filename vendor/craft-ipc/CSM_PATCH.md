# craft-ipc, vendored with one fix

Upstream: `storytold/craft-libs`, `crates/ipc`, rev `692a7fe5c95947e11850eeefc4e071623d4bbf47`
(MIT OR Apache-2.0, licence files alongside). Wired in through `[patch]` in the workspace
`Cargo.toml`.

## The fix (search `CSM_PATCH` in `src/server/listener.rs`)

The server's listener is nonblocking (`ListenerNonblockingMode::Accept`, and a nonblocking
`TcpListener`). On macOS and the BSDs an accepted socket **inherits O_NONBLOCK** from its
listener (Linux's `accept4` does not; Windows TCP sockets inherit it too). The connection's
reply writes (`write_all`) then fail with `WouldBlock` as soon as the socket send buffer is full,
8 KiB by default on macOS, and the connection is dropped mid-reply.

Seen on GitHub's macos-latest runner: `csm --help` against a running bridge got
`EOF while parsing an object at line 1 column 8192`, so every reply over 8 KiB (the tool list,
session lists, read_output) was lost on macOS. The fix sets each accepted connection back to
blocking; the existing send/recv timeouts then bound it, as they were meant to.

## Dropping this

Report upstream; when a craft-libs rev contains the fix, bump the rev in `crates/csm-mcp`,
remove the `[patch]` entry and delete this folder.

## Second patch: owner-only ACL on the Windows runtime folder (SEC-64)

`RuntimeDir::resolve` on Windows creates `%LOCALAPPDATA%\Craft` and `\run`, whose only protection
was the inherited ACL. When `resolve` creates the `Craft` folder it now replaces the inherited
permissions with one entry for the process's own SID (`whoami /user`, then `icacls ... /inheritance:r
/grant:r *SID:(OI)(CI)F`, both by their System32 path, no shell, no console window). Best effort: on
any failure the inherited ACL stays. A folder that already exists is left as it is. Search
`CSM_PATCH (SEC-64)` in `src/discovery/dir.rs`. Drop it together with the first patch.
