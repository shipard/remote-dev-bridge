# Remote Dev Bridge

A desktop tray application that lets **Claude Desktop** access your remote development servers over SSH via the [Model Context Protocol (MCP)](https://modelcontextprotocol.io).

Claude can read and write files, run commands, search codebases, and navigate project directories — all on a remote machine, through a secure SSH tunnel.

---

## What it does

Remote Dev Bridge runs two modes from the same binary:

| Mode | Purpose |
|------|---------|
| **Desktop app** (default) | System-tray UI to configure servers and projects |
| **`--mcp-stdio`** | JSON-RPC 2.0 MCP server that Claude Desktop connects to |

Claude Desktop launches the `--mcp-stdio` process automatically when you start a conversation. The binary reads your configuration, opens SSH connections on demand, and exposes a set of filesystem tools to Claude.

Configuration changes made in the tray UI take effect immediately — the MCP server hot-reloads config from disk on every tool call, so there is no need to restart Claude Desktop after editing servers or projects.

### Available tools

| Tool | Description |
|------|-------------|
| `list_projects` | List all configured projects and their servers |
| `read_file` | Read a text file (size-limited, UTF-8) |
| `write_file` | Create or overwrite a file (creates parent directories) |
| `list_directory` | List directory contents with type/size/mtime |
| `create_directory` | Create a directory tree (`mkdir -p`) |
| `file_info` | Get metadata for any path |
| `read_multiple_files` | Batch-read several files in one SSH session |
| `patch_file` | Apply search-and-replace edits (fail-fast if text not found) |
| `search_files` | `find`-based search with glob patterns |
| `move_path` | Rename or move a file or directory |
| `delete_path` | Delete a file or directory (recursive) |
| `run_command` | Run a shell command (gated per-project, with optional allowlist) |

---

## Prerequisites

- **macOS 10.15+**, **Windows 10+**, or a modern Linux desktop
- SSH access to your remote server (key-based authentication)
- SSH key without passphrase (e.g. `~/.ssh/id_rsa` or `~/.ssh/id_ed25519`). Passphrase-protected keys are not yet supported; ssh-agent support is planned.
- [Claude Desktop](https://claude.ai/download) installed

---

## Installation

### Pre-built release (recommended)

1. Download the installer for your platform from the [Releases](../../releases) page:
   - **macOS**: `.dmg`
   - **Windows**: `.msi`
   - **Linux**: `.deb` or `.AppImage`
2. Install and launch — a bridge icon appears in your menu bar / system tray.

### Build from source

```bash
# Prerequisites
# macOS: Xcode Command Line Tools
xcode-select --install

# Rust (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node.js (any recent LTS version)
brew install node          # macOS
# or: sudo apt install nodejs npm   # Debian/Ubuntu

# Tauri CLI
cargo install tauri-cli

# Clone and build
git clone https://github.com/shipard/remote-dev-bridge
cd remote-dev-bridge
cargo tauri build
```

The built installer is placed in `src-tauri/target/release/bundle/`.

For development (faster iteration without bundling):

```bash
cargo build
# Run the tray app:
src-tauri/target/debug/remote-dev-bridge
# Or test MCP mode directly:
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' \
  | src-tauri/target/debug/remote-dev-bridge --mcp-stdio 2>/dev/null
```

---

## Configuration

### Via the UI (recommended)

1. Click the tray icon → **Open Configuration**.
2. **Servers tab** → **+ Add Server**:
   - Display name, hostname/IP, port (default 22)
   - SSH username and path to your private key
   - Click **Test** to verify the connection.
3. **Projects tab** → **+ Add Project**:
   - Project name, choose a server, and set the root path (e.g. `/home/ubuntu/my-api`)
   - Optional: enable shell commands and restrict to safe prefixes (e.g. `git`, `npm`)
4. Click **Save**.

Changes are picked up by the MCP server automatically on the next tool call.

### Manual (JSON)

Config is stored at:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/remote-dev-bridge/config.json` |
| Windows | `%APPDATA%\remote-dev-bridge\config.json` |
| Linux | `~/.config/remote-dev-bridge/config.json` |

```jsonc
{
  "version": 1,
  "servers": {
    "my-server": {
      "label": "My Dev Server",
      "host": "192.168.1.10",
      "port": 22,
      "username": "ubuntu",
      "auth": {
        "auth_type": "key",
        "key_path": "~/.ssh/id_rsa"
      }
    }
  },
  "projects": {
    "my-api": {
      "label": "My API",
      "server": "my-server",
      "root_path": "/home/ubuntu/my-api",
      "description": "Node.js REST API",
      "allow_commands": true,
      "allowed_commands": ["git", "npm", "node"]
    }
  },
  "settings": {
    "max_file_size_kb": 1024,
    "ssh_connection_timeout_sec": 10,
    "ssh_keepalive_interval_sec": 30,
    "confirm_destructive": true
  }
}
```

---

## Claude Desktop integration

1. Open the tray menu → **Copy Claude Desktop Config**

2. Open your Claude Desktop config file:

   | Platform | Path |
   |----------|------|
   | macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
   | Windows | `%APPDATA%\Claude\claude_desktop_config.json` |

3. Merge the copied snippet into the file:

   ```json
   {
     "mcpServers": {
       "remote-dev-bridge": {
         "command": "/Applications/Remote Dev Bridge.app/Contents/MacOS/remote-dev-bridge",
         "args": ["--mcp-stdio"]
       }
     }
   }
   ```

4. **Restart Claude Desktop**.

5. Start a conversation and ask Claude something like:
   > "List my projects" or "Read the file `src/main.ts` in my-api"

---

## Logs

Logs are written to:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Logs/remote-dev-bridge/` |
| Windows | `%APPDATA%\remote-dev-bridge\logs\` |
| Linux | `~/.local/share/remote-dev-bridge/logs/` |

Daily log rotation keeps the last 5 files. MCP-mode logs go to stderr (captured by Claude Desktop).

Set the `RUST_LOG` environment variable to change verbosity (e.g. `RUST_LOG=debug`).

---

## Troubleshooting

### "Connection timed out"
- Check that the host and port are reachable: `ssh -p <port> <user>@<host>`
- Increase **SSH connection timeout** in Settings if on a slow network.

### "Authentication failed"
- Confirm the key path is correct and the key is **not** passphrase-protected (passphrase support is not yet implemented).
- Verify that the key works manually: `ssh -i <key_path> <user>@<host>`
- RSA, Ed25519, and ECDSA key types are all supported.

### "Server 'X' not found" / "Config unavailable"
- If your config file was corrupt, it has been backed up as `config.json.bak` and a fresh default was created.
- Re-enter your servers and projects via the UI, or restore from the backup.

### Claude doesn't see the MCP server
- Make sure the `command` path in `claude_desktop_config.json` points to the actual installed binary.
- Use **Copy Claude Desktop Config** from the tray menu to get the correct path automatically.
- Restart Claude Desktop after editing its config (this is only needed once; subsequent config changes in Remote Dev Bridge are picked up automatically).

### No tray icon on Linux
- Ensure your desktop environment supports system tray icons (e.g. install `libappindicator` or a GNOME extension like *AppIndicator Support*).

---

## Security notes

- SSH connections use public-key authentication only — no passwords stored.
- File access is confined to each project's `root_path`; path traversal (e.g. `../../etc/passwd`) is blocked.
- Shell commands are disabled by default and must be explicitly enabled per-project.
- Optionally restrict allowed commands to a prefix allowlist (e.g. `git`, `npm`).

---

## License

MIT — see [LICENSE](LICENSE).
