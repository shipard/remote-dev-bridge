# CLAUDE.md — remote-dev-bridge

Working notes for Claude Code on the devserver checkout.

## Current task

`docs/utf8-panic-and-stall-hardening.md` — read it first.

Work on the current fix branch; never commit to `stable`.

**This repository is public.** Internal tracker ids mean nothing to anyone
outside the team, so keep them out of anything that ships: code comments, doc
prose, commit messages, tag and release notes. Describe the bug instead of
citing the ticket. Internal project names, hostnames and absolute paths from our
own machines stay out too. Branch history is squash-merged onto `stable`, so
branch commit messages never reach the public remote — but the tree does.

State as of 2026-08-17: the UTF-8, quoting, deadline and Dock work is
implemented, and v0.1.3 is built, installed and verified on the Mac — clippy
clean, 8/8 unit tests green, Dock behaviour confirmed with no double icon.
Outstanding: the idle probe after a real sleep or network change, and atomic
remote writes (see the follow-ups in the brief).

## Environment constraints

- This checkout lives on the **devserver**: `~/sw/remote-dev-bridge`.
- **No Rust toolchain here.** `cargo` is not installed. You cannot build, test,
  clippy or run anything. Edit files only.
- All verification happens on the **Mac** (Xcode CLT + Rust + `cargo tauri`):
  `git pull && cargo clippy --all-targets && cargo test && cargo tauri build`.
- Because nothing is compile-checked here, prefer small, obviously-correct edits
  over refactors. If a change needs a design decision, stop and ask.

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
   ids — see the note under "Current task".

## Editing this repo through the bridge itself

As of the installed v0.1.3 this is no longer a hazard: `patch_file` on Czech
content is safe, and the edits in this repo were themselves applied through the
bridge using it. The old rules — keep Czech lines under ~110 bytes, rewrite whole
files with `write_file`, pre-scan content for unsafe byte lengths — are obsolete.

The one remaining caveat is F10: a connection drop mid-write can truncate a file,
because remote writes are not atomic yet. After any transport error or timeout,
read the file back before trusting it.
