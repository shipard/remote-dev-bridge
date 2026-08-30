# CLAUDE.md — remote-dev-bridge

Working notes for Claude Code.

**This repository is public.** Keep internal tracker ids out of anything that
ships: code comments, doc prose, commit messages, tag and release notes.
Describe the bug instead of citing the ticket. Internal project names,
hostnames and absolute paths from private machines stay out too.

## Repository layout

```
src-tauri/src/
  main.rs          binary entry: --mcp-stdio (JSON-RPC server) vs tray app
  tray.rs          system tray menu, config window show/hide
  commands.rs      Tauri IPC commands used by the frontend
  config.rs        config.json load/save
  error.rs         SshError
  mcp/
    protocol.rs    JSON-RPC 2.0 stdio loop, tools/call dispatch
    tools.rs       tool definitions + all 12 handlers  ← most bugs live here
    types.rs       JsonRpc*/ToolResult
  ssh/
    session.rs     russh session + SshSessionManager
    sftp.rs        SFTP operations, exec helper, connection test
src/                frontend (plain HTML/JS, no bundler)
src-tauri/Info.plist  macOS bundle plist (Dock behaviour)
```

Same binary serves both modes; Claude Desktop spawns it with `--mcp-stdio`.
Config is re-read from disk on every `tools/call`, so tray-UI edits apply
without restarting Claude Desktop. Changing the **binary** does require a
rebuild plus a Claude Desktop restart.

## Invariants

1. **Never slice a `&str` by byte index.** Use `preview()` / `floor_char_boundary()`.
   A byte slice inside a multi-byte character panics; with Czech content that is
   routine, not an edge case. This was the original production crash.
2. **A tool must never be able to kill the process.** Handlers return
   `ToolResult::error(...)`; panics are caught in `protocol.rs`. Do not
   reintroduce `panic = "abort"`.
3. **Every remote operation needs a deadline.** SFTP calls have no built-in
   timeout; the outer per-call timeout in `protocol.rs` is the backstop.
4. **Quote everything that goes into a shell command** with `shq()`.
   Paths and patterns come from the model and can contain anything.
5. **Paths stay inside the project root.** `resolve_safe_path()` is the only
   gate — do not bypass it.
6. **stdout belongs to the JSON-RPC transport** in `--mcp-stdio` mode. Logs go
   to stderr. Never `println!` in that path.
7. Prose, comments and commit messages in English, and free of internal tracker
   ids — see the public-repository note at the top.

## Verification

`cargo clippy --all-targets && cargo test && cargo tauri build` must pass
before anything lands on `stable`.
