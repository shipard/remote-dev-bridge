# UTF-8 panic and stall hardening in `patch_file` + macOS Dock UX

**Baseline:** `stable` must not be touched. All work lands on a fix branch.

> **Status: implemented and partially verified.** F1-F9 and the Dock change were
> written blind — nothing was compiled while they were being written. v0.1.3 has
> since been built and installed on the Mac and the core fix is confirmed working
> against the real binary; see section 6a for the verification log and what is
> still outstanding.
>
> **Watch out for a known regression risk.** Git history contains
> `84f58ac "Fix double dock icon on macOS"` and `f5ecec4 "Fix macOS dock icon via
> TransformProcessType before Tauri init"`. `LSUIElement` plus the startup-time
> transform were how that was solved. Removing them (section 5) may bring the
> double icon back - most likely only when running the bare binary rather than
> the installed `.app`. Verify with the real `.dmg` build. If the double icon
> reappears in the bundle, the fallback is to keep `LSUIElement = true` and call
> `set_dock_icon_visible(true)` from `setup()` instead of removing the plist key.
>
> **Resolved 2026-08-17.** Checked on the installed `.app`: no double icon, the
> icon leaves the Dock when the config window is closed, and a pinned icon only
> toggles its running dot. `LSUIElement` stays removed; the fallback above was
> not needed. See the verification log in section 6a.

---

## 0. Ground rules for this task

- Do **not** commit to `stable`, do **not** force-push, do **not** rebase `stable`.
- `cargo` is **not installed on the devserver** — you cannot build or run tests here.
  Edit only. Compilation, `cargo test`, `cargo clippy` and `cargo tauri build`
  happen on the Mac after `git pull`.
- Therefore: be conservative. No speculative refactors, no dependency additions,
  no API changes beyond what is specified below. Every change must be
  compile-checkable by reading it.
- Commit in the order of the fix sections (F1 … F9), one commit per section,
  English commit messages.
- After all commits: bump version to `0.1.3` in both `src-tauri/Cargo.toml`
  and `src-tauri/tauri.conf.json` as the final commit.

---

## 1. Symptoms as observed in production

Two failure modes were reported. They look unrelated. They are the same bug.

### 1a. Hard crash — "Server disconnected"

Bridge log, session of 2026-08-03, while editing Czech e-shop policy files:

```
panicked at src/mcp/tools.rs:743:24:
byte index 120 is not a char boundary; it is inside '“' (bytes 119..122)
```

Followed on the Claude Desktop side by:

```
Server transport closed
Server disconnected
```

Trigger file: `policies/complaints-cs.md` (Czech text, typographic
quotes `„ “` = 3 bytes each in UTF-8). Reproduced twice the same day.

Diagnostic pattern that made this confusing: `write_file` always succeeded, and
the crash arrived on the *next* tool call. `write_file` returns no content
preview, so it never hits the faulty code path.

### 1b. Hang — 4-minute timeouts, zombie process (as filed)

Verbatim from the report:

> The bridge process (MCP inside Claude Desktop) wedges mid-session: every
> subsequent call, including a trivial `list_projects`, ends in a 4-minute
> timeout ("No result received from the Claude Desktop app"). The remote server
> itself is fine. Only killing and restarting the bridge process helps;
> restarting Claude Desktop alone did not work the first time (zombie process).

Observations recorded in the issue:

- Crash #1: `patch_file` with **7 edits** on `policies/complaints-cs.md` → hang
- After kill/restart: `list_projects` OK, `write_file` 10.8 kB OK
- Crash #2: immediately next call, `patch_file` with **3 edits** on `terms-cs.md` → hang
- Earlier `patch_file` calls the same day (1–6 edits, same kind of Czech content
  with diacritics and `„ “`) went through fine
- Both failures were **multi-edit** `patch_file`. `write_file`, `read_file`,
  `list_directory`, `read_multiple_files` never failed.

The issue's own hypothesis ("hang instead of error on a not-found search string,
or a payload size/encoding problem") is close. The precise mechanism is below.

---

## 2. Root cause

### The panic

`src-tauri/src/mcp/tools.rs`, `handle_patch_file`, the branch taken when an edit's
`search` string is **not found** in the file:

```rust
if !content.contains(search) {
    return ToolResult::error(format!(
        "Edit #{i}: search string not found in file — aborting to prevent partial edits.\n\
         Search string (first 120 chars): {:?}",
        &search[..search.len().min(120)]   // <-- BYTE slice
    ));
}
```

`&search[..120]` slices by **bytes**. Rust panics if byte 120 is not a UTF-8
character boundary. Czech text hits this constantly: `„ “` are 3 bytes,
`á č ě í ř š ž ů ú ý` are 2 bytes each.

### Why it looked non-deterministic

Three conditions must hold simultaneously:

1. At least one edit's `search` string is **not found** in the file
   (single-edit patches on freshly-read content usually match → no panic).
2. That `search` string is **longer than 120 bytes** (otherwise
   `search.len().min(120) == search.len()`, which is always a valid boundary → no panic).
3. Byte 120 of that string lands **inside** a multi-byte character.

This explains the whole observed pattern exactly:

- **Multi-edit only** — more edits means a higher chance that one of them
  misses, and multi-edit patches on prose use long context strings (>120 B).
- **Same file, same content type, sometimes fine** — those calls had all their
  search strings found, so the faulty line was never reached.
- **`write_file` never fails** — no preview, no slicing.

### Why the same bug shows up as a hang and not always as a crash

`src-tauri/Cargo.toml` has:

```toml
[profile.release]
panic = "abort"
```

The panic therefore `abort()`s the process instead of unwinding. On macOS an
aborting process is captured by the crash reporter (`ReportCrash`) before it is
fully reaped; during that window the process is a zombie and its stdio pipes are
**not closed**. Claude Desktop is still waiting on a pipe that will never
produce a byte, so instead of a clean transport-closed error you get 4-minute
timeouts on every subsequent call and a process that needs an explicit `kill`.
Whether you observe 1a or 1b is a race with the crash reporter — that is the
"non-determinism" in the issue.

**One bug, both symptoms.** Fixing F1 removes the trigger; F2 and F3 make sure a
future panic or a stalled SSH channel degrades into an error response instead of
taking the bridge down.

---

## 3. Secondary defects found during the audit

These were found while reading the code. They are real, cheap to fix, and
adjacent to the same code paths.

| ID | File | Defect |
|----|------|--------|
| B4 | `ssh/session.rs` | Dead sessions are not detected. `ensure_connected()` reconnects only when `is_closed() == true`. After the Mac sleeps or the network changes (Wi-Fi ↔ LTE) the TCP socket is half-open, the russh handle still reports alive, so every call fails on `open channel:` until Claude Desktop is restarted. |
| B5 | `mcp/tools.rs`, `ssh/sftp.rs` | Naive shell quoting. `find '{path}'`, `mkdir -p '{path}'`, `rm -rf -- '{path}'`, `cd '{root}' && …` break (or inject) on any path containing a single quote, e.g. `Patrik's notes.md`. |
| B6 | `mcp/tools.rs` | `list_directory` with `recursive=true` returns `find` output with no size cap. Only `run_command` uses `truncate_output`. |
| B7 | `mcp/tools.rs` | `allowed_commands` gate uses bare `command.starts_with(prefix)`, so with `git` allowed, `gitfoo; rm -rf ~` passes. |
| B8 | `mcp/tools.rs` | `truncate_output` relies on `str::floor_char_boundary`, which is an unstable/recently-stabilised std API — needless MSRV coupling for four lines of code. |
| B9 | `mcp/protocol.rs` | No per-call deadline. SFTP operations have no timeout at all (only `run_command` does), so a stalled SFTP channel hangs the whole stdio loop — an independent second route to the same symptom. |

---

## 4. Fixes

### F1 — Never slice strings by byte index (the actual fix)

`src-tauri/src/mcp/tools.rs` — replace the not-found branch in `handle_patch_file`:

```rust
        if !content.contains(search) {
            return ToolResult::error(format!(
                "Edit #{i}: search string not found in file — aborting to prevent partial edits.\n\
                 Search string (first 120 chars): {:?}",
                preview(search, 120)
            ));
        }
```

Add to the shared-helpers section at the bottom of the file:

```rust
/// Take at most `max_chars` *characters* (never bytes) from `s`.
///
/// Byte slicing (`&s[..120]`) panics when the cut lands inside a multi-byte
/// UTF-8 character — Czech diacritics, typographic quotes, em dashes, emoji.
/// This never panics.
fn preview(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (n, ch) in s.chars().enumerate() {
        if n == max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
```

Then grep the whole crate for any remaining byte-index slicing and convert it:

```
grep -rn --include='*.rs' -E '\[\.\.[^]]*\]|\[[0-9a-z_.]+\.\.\]' src-tauri/src
```

Report anything found that is not covered by F1/F8 rather than silently changing it.

### F2 — Contain panics instead of aborting the process

`src-tauri/Cargo.toml` — drop `panic = "abort"` so unwinding is possible:

```toml
[profile.release]
# No `panic = "abort"`: a panic inside a tool handler must be catchable and
# returned as a JSON-RPC error, not kill the bridge.
codegen-units = 1
lto = true
opt-level = "s"
strip = true
```

`src-tauri/src/mcp/protocol.rs` — in `handle_tools_call`, run the dispatch on its
own task so a panic becomes a `JoinError` instead of a dead process:

```rust
        // The tool call runs in its own task so that a panic inside a handler
        // is caught here and returned as an error, instead of aborting the
        // whole bridge. Requires panic = "unwind" (see Cargo.toml).
        let sessions = self.sessions.clone();
        let name = tool_name.to_owned();
        let join = tokio::spawn(async move { dispatch(&name, &args, &config, &sessions).await });

        let tool_result: ToolResult = match join.await {
            Ok(r) => r,
            Err(e) if e.is_panic() => {
                error!("tool handler panicked: {e}");
                ToolResult::error(
                    "Internal error: the tool handler panicked. The bridge is still \
                     alive — retry, and please report this.",
                )
            }
            Err(e) => ToolResult::error(format!("Internal error: task failed ({e})")),
        };
```

`args` and `config` are already owned values in that function; move them into the
task. `sessions` is an `Arc`. No new dependency is needed.

Also install a panic hook in `run_mcp_stdio()` so a panic is always visible in
Claude Desktop's stderr log even when it is caught:

```rust
    std::panic::set_hook(Box::new(|info| {
        eprintln!("PANIC in remote-dev-bridge: {info}");
    }));
```

### F3 — Per-call deadline (B9, closes the "watchdog" checkbox)

`src-tauri/src/mcp/protocol.rs` — wrap the join above in a timeout so no tool call
can wedge the stdio loop, whatever the cause:

```rust
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(120);
```

```rust
        let tool_result: ToolResult = match tokio::time::timeout(TOOL_CALL_TIMEOUT, join).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) if e.is_panic() => { /* as in F2 */ }
            Ok(Err(e)) => ToolResult::error(format!("Internal error: task failed ({e})")),
            Err(_) => ToolResult::error(format!(
                "Tool '{tool_name}' exceeded the {}s bridge deadline and was abandoned. \
                 The SSH session will be re-established on the next call.",
                TOOL_CALL_TIMEOUT.as_secs()
            )),
        };
```

Note: `run_command` already has its own (shorter, caller-supplied) timeout; this
is the outer backstop for SFTP operations that have none.

### F4 — Detect dead SSH sessions (B4)

`src-tauri/src/ssh/session.rs`:

```rust
use std::time::Instant;

const IDLE_PROBE_AFTER: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct SshSessionManager {
    sessions: HashMap<String, Arc<SshSession>>,
    last_used: HashMap<String, Instant>,
    pub settings: Settings,
}
```

```rust
    /// Like `connect`, but also verifies the session is genuinely alive.
    ///
    /// `is_closed()` does not catch a half-open socket after the laptop sleeps
    /// or the network changes: russh still reports the handle as alive while
    /// every subsequent call fails. So after an idle period we send a cheap
    /// `true` and rebuild the session if it does not come back.
    pub async fn ensure_connected(
        &mut self,
        server_id: &str,
        config: &ServerConfig,
    ) -> Result<(), SshError> {
        let stale = match self.sessions.get(server_id) {
            None => true,
            Some(s) if s.is_closed() => true,
            Some(s) => {
                let idle = self
                    .last_used
                    .get(server_id)
                    .map(|t| t.elapsed())
                    .unwrap_or(IDLE_PROBE_AFTER);

                if idle < IDLE_PROBE_AFTER {
                    false
                } else {
                    match tokio::time::timeout(PROBE_TIMEOUT, s.exec("true")).await {
                        Ok(Ok(_)) => false,
                        Ok(Err(e)) => {
                            info!(server_id, "liveness probe failed ({e}) — reconnecting");
                            true
                        }
                        Err(_) => {
                            info!(server_id, "liveness probe timed out — reconnecting");
                            true
                        }
                    }
                }
            }
        };

        if stale {
            if let Some(old) = self.sessions.remove(server_id) {
                // A half-open socket can block on disconnect — hence the timeout.
                let _ = tokio::time::timeout(Duration::from_secs(2), old.disconnect()).await;
            }
            self.connect(server_id, config).await?;
        }

        self.last_used.insert(server_id.to_owned(), Instant::now());
        Ok(())
    }
```

Also update `new()` (`last_used: HashMap::new()`) and remove the `last_used`
entry in `disconnect()` and `disconnect_all()`.

### F5 — POSIX shell quoting (B5)

`src-tauri/src/mcp/tools.rs`, shared helpers:

```rust
/// POSIX-quote a string for safe interpolation into a shell command.
/// `it's` → `'it'\''s'`
pub fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
```

Apply at every interpolation site:

- `tools.rs` / `handle_list_directory`: `find {} -maxdepth {}` with `shq(&abs_path)`
- `tools.rs` / `handle_search_files`: `find {} -maxdepth 5 -name {}` with
  `shq(&search_root)` and `shq(pattern)`; **remove** the now-redundant
  "pattern must not contain single quotes or backslashes" rejection
- `tools.rs` / `handle_run_command`: `format!("cd {} && {}", shq(root), command)`
  (the user command itself stays unquoted by design)
- `sftp.rs` / `sftp_write_file` parent mkdir, `sftp_mkdir`, `sftp_remove`:
  `mkdir -p {}` / `rm -rf -- {}` with `shq(path)`

Export it from the `mcp` module (`pub use tools::shq;` or keep `tools` public) so
`ssh/sftp.rs` can call it.

### F6 — Cap recursive listing output (B6)

Move `const MAX_OUTPUT_BYTES: usize = 100 * 1024;` to the top of `tools.rs` and in
`handle_list_directory`:

```rust
            Ok((stdout, _, 0)) => {
                let output = if stdout.trim().is_empty() {
                    "(empty directory)".to_owned()
                } else {
                    truncate_output(stdout, MAX_OUTPUT_BYTES)
                };
                ToolResult::text(output)
            }
```

(This also removes the currently unused `stderr` binding in that arm.)

### F7 — Tighten the command allowlist (B7)

```rust
            let permitted = allowed.iter().any(|prefix| {
                command == prefix.as_str()
                    || command
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            });
```

### F8 — Drop the unstable std dependency (B8)

```rust
/// Largest index <= `max_bytes` that is a valid UTF-8 character boundary.
/// Local implementation instead of `str::floor_char_boundary` so the crate
/// does not depend on the toolchain version.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn truncate_output(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let cut = floor_char_boundary(&s, max_bytes);
    s.truncate(cut);
    s.push_str("\n[... output truncated ...]");
    s
}
```

### F9 — Unit tests (must be added; they are the only automated proof)

Add a `#[cfg(test)] mod tests` at the bottom of `tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard: the exact shape that used to panic.
    #[test]
    fn preview_does_not_panic_on_multibyte_at_the_cut() {
        // 119 ASCII bytes, then a 3-byte typographic quote straddling byte 120.
        let s = format!("{}„český text“ pokračuje dál", "a".repeat(119));
        let p = preview(&s, 120);
        assert!(p.chars().count() <= 121); // 120 chars + ellipsis
    }

    #[test]
    fn preview_short_input_is_unchanged() {
        assert_eq!(preview("žluťoučký kůň", 120), "žluťoučký kůň");
    }

    #[test]
    fn floor_char_boundary_never_splits_a_char() {
        let s = "ááááá"; // 2 bytes each
        for n in 0..=s.len() {
            let i = floor_char_boundary(s, n);
            assert!(s.is_char_boundary(i));
        }
    }

    #[test]
    fn shq_escapes_single_quotes() {
        assert_eq!(shq("Patrik's notes.md"), r"'Patrik'\''s notes.md'");
        assert_eq!(shq("plain"), "'plain'");
    }

    #[test]
    fn allowlist_requires_a_word_boundary() {
        // guard against `git` allowing `gitfoo; rm -rf ~`
        let allowed = ["git".to_string()];
        let check = |command: &str| {
            allowed.iter().any(|p| {
                command == p.as_str()
                    || command
                        .strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            })
        };
        assert!(check("git status"));
        assert!(check("git"));
        assert!(!check("gitfoo; rm -rf ~"));
    }
}
```

---

## 5. macOS Dock / config-window behaviour

Separate from the hang, requested in the same pass.

### Current behaviour (wrong)

Clicking the app icon in the Dock does nothing visible — the app only appears in
the menu bar, and the config window has to be opened from the tray menu via
*Open Configuration*.

Two causes:

1. `src-tauri/Info.plist` sets `LSUIElement = true`, and `main.rs` additionally
   calls `macos_hide_from_dock()` at startup. The process is therefore a
   permanent UI-element app: never in the Dock, never receives normal activation.
2. `.setup()` only shows the window when `first_run == true`.

### Required behaviour

- Launching the app (Dock / Finder / Spotlight) opens the configuration window
  immediately.
- Clicking the Dock icon of an already-running instance opens/focuses the window.
- Closing the window hides it and removes the Dock icon, but the tray icon and
  the MCP bridge keep running. Quit is still only via the tray menu.

### Implementation

**`src-tauri/Info.plist`** — remove `LSUIElement`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- Deliberately NO LSUIElement. Dock visibility is toggled at runtime via
         TransformProcessType (see src/platform.rs): the icon is in the Dock
         while the config window is open and disappears when it is closed.
         A static LSUIElement=true makes clicking the Dock icon a no-op. -->
</dict>
</plist>
```

**New `src-tauri/src/platform.rs`:**

```rust
//! Platform-specific Dock / activation behaviour.

/// Show or hide the Dock icon at runtime.
///
/// MUST be called on the main thread — Cocoa guarantees nothing otherwise.
#[cfg(target_os = "macos")]
pub fn set_dock_icon_visible(visible: bool) {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn TransformProcessType(psn: *const [u32; 2], r#type: u32) -> i32;
    }
    const K_CURRENT_PROCESS: [u32; 2] = [0, 2]; // kCurrentProcess
    const TO_FOREGROUND: u32 = 1; // kProcessTransformToForegroundApplication
    const TO_UI_ELEMENT: u32 = 4; // kProcessTransformToUIElementApplication

    let transform = if visible { TO_FOREGROUND } else { TO_UI_ELEMENT };
    unsafe {
        TransformProcessType(&K_CURRENT_PROCESS, transform);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon_visible(_visible: bool) {}
```

**`src-tauri/src/tray.rs`** — make `show_config_window` public and Dock-aware;
hide the Dock icon when the window is closed:

```rust
/// Show the configuration window and put the icon back in the Dock.
pub fn show_config_window<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    let _ = app.run_on_main_thread(move || {
        // Dock first, then the window — otherwise macOS will not activate it.
        crate::platform::set_dock_icon_visible(true);
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    });
}

/// Closing the window only hides it and drops the Dock icon. The tray icon and
/// the MCP bridge keep running.
pub fn attach_close_handler(window: &WebviewWindow) {
    let win = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = win.hide();
            let app = win.app_handle().clone();
            // Hide the window before transforming, or a ghost stays in the Dock.
            let _ = app.run_on_main_thread(|| {
                crate::platform::set_dock_icon_visible(false);
            });
        }
    });
}
```

**`src-tauri/src/main.rs`** — add `mod platform;`, delete `macos_hide_from_dock()`
and its call site, always open the window on launch, and handle `Reopen`:

```rust
        .setup(move |app| {
            use tauri::Manager;
            tray::setup(app.handle())?;
            if let Some(window) = app.get_webview_window("main") {
                tray::attach_close_handler(&window);
            }
            // When the user launches the app (Dock / Finder / Spotlight) they
            // want the configuration. `first_run` no longer gates this; it is
            // still exposed to the frontend through IsFirstRun.
            tray::show_config_window(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building Tauri app")
        .run(|app, event| match event {
            // macOS: the user clicked the Dock icon of a running instance.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => tray::show_config_window(app),

            // Never quit just because the last window went away — the tray icon
            // and the bridge must stay alive. Tray "Quit" goes through
            // app.exit(0), where code = Some(0) and the exit proceeds.
            tauri::RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
                api.prevent_exit();
            }
            _ => {}
        });
```

Note for the release notes: once the window is closed the Dock icon disappears,
so the app must be pinned (right-click → Options → Keep in Dock) for the
click-to-open flow to have something to click.

---

## 6. Acceptance criteria

On the Mac, on the fix branch:

1. `cargo clippy --all-targets` — no new warnings.
2. `cargo test` — all F9 tests pass.
3. `cargo tauri build` succeeds and produces the `.dmg`.

Manual, after installing the new build and restarting Claude Desktop:

4. **Regression check.** `patch_file` on a Czech `.md` with an intentionally
   wrong `search` string longer than 120 bytes containing `„ “` returns a normal
   error message with a readable preview. The bridge stays up: a following
   `list_projects` answers immediately.
5. Same call with 7 edits where edit #5 does not match → error naming edit #4
   (zero-indexed), file unchanged, bridge alive.
6. Idle the session > 60 s, sleep the Mac or switch Wi-Fi → LTE, then run any
   tool: it reconnects transparently instead of failing on `open channel:`.
7. `write_file` to a path containing an apostrophe, then `read_file` it back.
8. `list_directory recursive=true max_depth=6` on a large tree returns truncated
   output, not a multi-megabyte blob.
9. With `allowed_commands: ["git"]`, `run_command "gitfoo; echo x"` is rejected.
10. Dock: click the pinned icon → config window opens. Close the window → icon
    leaves the Dock, tray icon remains, a tool call from Claude Desktop still
    works. Click the pinned icon again → window reopens.
11. No zombie `remote-dev-bridge` process after any of the above
    (`pgrep -fl remote-dev-bridge`).

---

## 6a. Verification log

### 2026-08-17 - v0.1.3 built and installed on the Mac

Run through the MCP session against the installed binary, a configured project, in a scratch
directory removed afterwards.

| # | Check | Result |
|---|-------|--------|
| 4 | `patch_file`, missing `search` of 111 ASCII bytes + 10x `a-acute` + typographic quotes, so that byte 120 falls inside a 2-byte character | **PASS** - clean `Edit #0: search string not found` with a readable char-truncated preview ending in an ellipsis. No panic. |
| - | `list_projects` immediately after the above | **PASS** - instant reply, process alive. This is the pair that used to end the session. |
| 5 | 3 edits where edit #2 has a >120-byte Czech search string that is not present | **PASS** - `Edit #2` reported; `read_file` confirms the file is bit-identical, both markers untouched. Fail-fast holds, no partial write. |
| - | Valid 3-edit patch on the same Czech file | **PASS** - 466 bytes written. |
| 7 | `write_file` to `scratch/O'Brien's dir/O'Brien's notes.md` (apostrophe in both the directory and the file name) | **PASS** - previously failed in the parent `mkdir -p`. |
| 7b | `search_files` with pattern `*'s notes.md` | **PASS** - found the file. Previously hard-rejected by the removed quote/backslash validation. |
| 7c | `delete_path` recursive on the apostrophe directory | **PASS** - removed the target only, nothing beside it. |
| 8 | `list_directory` recursive, `max_depth=3` | **PASS** - correct listing, `find` quoting intact. |
| - | Path traversal `../../../etc/passwd` via `read_file` | **PASS** - rejected by `resolve_safe_path` before any SSH traffic. |
| 10 | Dock / config window behaviour, checked on the installed `.app` | **PASS** - and, importantly, **no double Dock icon**. Closing the config window removes the icon from the Dock; the pinned icon only gains and loses the running dot. The regression risk flagged at the top of this document did not materialise, so `LSUIElement` stays removed and the fallback is not needed. |

### Toolchain results (acceptance criteria 1-3)

Mac: macOS 26.5.2, arm64, Xcode CLT (Apple clang 21.0.0), Rust 1.97.1 stable.
No Rust toolchain was present before this run; `rustup` and `cargo tauri`
(v2) were installed for it.

| # | Check | Result |
|---|-------|--------|
| 1 | `cargo clippy --all-targets` | **PASS** - no errors, 9 warnings, none of them new. `ssh_exec`, `disconnect_all` and `EntryType::Other` were already dead code on `stable`, and `main.rs` `files.sort_by` is byte-identical to `stable`. |
| 2 | `cargo test` | **PASS** - 8 passed, 0 failed, including `floor_char_boundary_never_splits_a_char`, `preview_does_not_panic_on_multibyte_at_the_cut`, `truncate_output_is_utf8_safe` and `shq_escapes_single_quotes`. |
| 3 | `cargo tauri build` | **PASS** - unsigned `.dmg` produced and installed by hand. Signing and notarization happen only in the release workflow. |

Two defects had to be fixed before anything compiled (commit `00bda09`), both
found by review rather than by the compiler:

- `tray.rs` `show_config_window` - **E0505**. The cloned `AppHandle` shadowed
  the parameter, so the receiver borrow taken by `run_on_main_thread` collided
  with the `move` closure that consumed it.
- `protocol.rs` - the F3 deadline did not cancel anything. `timeout()` took the
  `JoinHandle` by value and dropped it on expiry, and **dropping a `JoinHandle`
  detaches the task rather than cancelling it**. The stalled task kept holding
  the `SshSessionManager` mutex, so every later call blocked on the lock until
  its own deadline expired - reproducing the original hang with a slow
  error instead of silence. Now polled as `&mut join` with `join.abort()` on
  expiry.

The `Send + 'static` bound that `tokio::spawn` newly imposes on the whole
handler path was the main remaining build risk; it compiled without complaint,
so every type held across an await in `tools.rs` / `sftp.rs` is `Send`.

### Not covered by this run

| # | Check | Why |
|---|-------|-----|
| 6 | F4 idle probe after sleep / network change | Needs >60 s idle plus a real suspend or Wi-Fi to LTE switch. |
| 9 | F7 allowlist word boundary | All four configured projects have `allow_commands: true` with no `allowed_commands`, so the branch is never reached. Covered by unit test only. |
| - | F3 120 s deadline | No reliable way to stall an SFTP channel on demand. |
| - | F6 truncation at 100 kB | Would dump 100 kB into the session. Try `list_directory recursive max_depth=8` on a large tree and look for the truncation notice. |
| 11 | Zombie process check | `pgrep -fl remote-dev-bridge` on the Mac. |

### What this changes for day-to-day use

The workarounds are retired. Czech markdown no longer needs lines
kept under ~110 bytes, edits no longer need to go through a full-file
`write_file`, and pre-scanning content for unsafe byte lengths is unnecessary.
`patch_file` on Czech prose is an ordinary tool again.

What still needs care: an **interrupted connection**. An interrupted call now
returns a readable error instead of killing the bridge, but the retry and the
verification are on the caller - and because of F10 below, a file written when
the link dropped mid-write may be truncated. After any transport error or
deadline, read the file back before assuming anything.

---

## 7. Follow-ups (do NOT do in this branch)

- **F10 - atomic remote writes.** `sftp_write_file` uses `create()`
  (CREATE | TRUNCATE | WRITE) directly on the target path, so a connection drop
  part-way through leaves a truncated file. Local `save_config()` already does
  this correctly - temp file plus `rename`. The remote path should do the same:
  write to `<path>.rdb-tmp`, then `sftp.rename()` over the target. More pressing
  now that `patch_file` is back in regular use.
- Transparent retry-once-on-transport-error around every SFTP/exec operation
  (a `with_retry` wrapper in `tools.rs`); F4 covers the common case only.
- `known_hosts` verification — `ClientHandler::check_server_key` currently
  accepts every host key.
- ssh-agent / passphrase-protected key support.
- Honour the client's requested `protocolVersion` in `initialize` instead of
  hardcoding `2024-11-05`.
- Streaming/paged `read_file` for files above `max_file_size_kb`.
