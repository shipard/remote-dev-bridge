// Remote Dev Bridge — Config UI
// Uses window.__TAURI__.core.invoke (withGlobalTauri: true)

const invoke = window.__TAURI__.core.invoke;

// ── State ──────────────────────────────────────────────────────────────────

let config = { version: 1, servers: {}, projects: {}, settings: {} };
let editingServerId = null;   // null = new, string = existing
let editingProjectId = null;

// ── Boot ───────────────────────────────────────────────────────────────────

document.addEventListener("DOMContentLoaded", async () => {
  setupNav();
  await loadConfig();
  showSection("servers");
  pollConnectionStatus();
  checkFirstRun();
});

// ── Navigation ─────────────────────────────────────────────────────────────

function setupNav() {
  document.querySelectorAll(".nav-item").forEach(item => {
    item.addEventListener("click", () => showSection(item.dataset.section));
  });
}

function showSection(name) {
  document.querySelectorAll(".nav-item").forEach(el => {
    el.classList.toggle("active", el.dataset.section === name);
  });
  document.querySelectorAll(".section").forEach(el => {
    el.classList.toggle("hidden", el.id !== `section-${name}`);
  });
  if (name === "settings") renderSettings();
}

// ── Config load/save ───────────────────────────────────────────────────────

async function loadConfig() {
  try {
    config = await invoke("get_config");
    renderServers();
    renderProjects();
  } catch (e) {
    showToast("Failed to load config: " + e, true);
  }
}

async function persistConfig() {
  try {
    await invoke("save_config_cmd", { config });
    showToast("Configuration saved");
  } catch (e) {
    showToast("Save failed: " + e, true);
    throw e;
  }
}

// ── Servers section ────────────────────────────────────────────────────────

function renderServers() {
  const list = document.getElementById("server-list");
  const ids = Object.keys(config.servers);

  if (ids.length === 0) {
    list.innerHTML = `<div class="empty-state">
      <div style="font-size:32px">🖥️</div>
      <p>No servers configured yet.<br>Add one to get started.</p>
    </div>`;
    return;
  }

  list.innerHTML = ids.map(id => {
    const s = config.servers[id];
    return `<div class="card" id="server-card-${id}">
      <div class="status-dot checking" id="dot-${id}"></div>
      <div class="card-body">
        <div class="card-title">${esc(s.label)}</div>
        <div class="card-subtitle">${esc(s.username)}@${esc(s.host)}:${s.port}</div>
      </div>
      <div class="card-actions">
        <button class="btn-secondary" onclick="testServer('${id}')">Test</button>
        <button class="btn-icon" onclick="openServerModal('${id}')" title="Edit">✏️</button>
        <button class="btn-icon" onclick="deleteServer('${id}')" title="Delete">🗑️</button>
      </div>
    </div>`;
  }).join("");
}

async function testServer(id) {
  const dot = document.getElementById(`dot-${id}`);
  dot.className = "status-dot checking";
  try {
    const info = await invoke("test_ssh_connection", { serverId: id });
    dot.className = "status-dot connected";
    showToast("Connected: " + info);
  } catch (e) {
    dot.className = "status-dot error";
    showToast("Connection failed: " + e, true);
  }
}

async function pollConnectionStatus() {
  try {
    const status = await invoke("get_connection_status");
    Object.entries(status).forEach(([id, connected]) => {
      const dot = document.getElementById(`dot-${id}`);
      if (dot && !dot.classList.contains("checking")) {
        dot.className = "status-dot " + (connected ? "connected" : "");
      }
    });
  } catch (_) {}
  setTimeout(pollConnectionStatus, 10000);
}

async function deleteServer(id) {
  const s = config.servers[id];
  if (!confirm(`Delete server "${s.label}"?\n\nProjects using this server will also be removed.`)) return;

  // Remove dependent projects
  Object.keys(config.projects).forEach(pid => {
    if (config.projects[pid].server === id) delete config.projects[pid];
  });
  delete config.servers[id];

  try {
    await persistConfig();
    renderServers();
    renderProjects();
  } catch (_) {
    await loadConfig(); // revert on failure
  }
}

// ── Server modal ───────────────────────────────────────────────────────────

function openServerModal(id = null) {
  editingServerId = id;
  const isEdit = id !== null;
  document.getElementById("server-modal-title").textContent = isEdit ? "Edit Server" : "Add Server";

  const s = isEdit ? config.servers[id] : {
    label: "", host: "", port: 22, username: "",
    auth: { auth_type: "key", key_path: "~/.ssh/id_rsa" }
  };

  document.getElementById("server-label").value = s.label;
  document.getElementById("server-host").value = s.host;
  document.getElementById("server-port").value = s.port;
  document.getElementById("server-username").value = s.username;
  document.getElementById("server-auth-type").value = s.auth.auth_type;
  document.getElementById("server-key-path").value = s.auth.key_path || "";

  updateAuthFields();
  clearFieldErrors("server-form");
  document.getElementById("server-modal").classList.remove("hidden");
}

function closeServerModal() {
  document.getElementById("server-modal").classList.add("hidden");
}

function updateAuthFields() {
  const type = document.getElementById("server-auth-type").value;
  document.getElementById("auth-key-fields").classList.toggle("hidden", type !== "key");
}

async function saveServer() {
  const label = document.getElementById("server-label").value.trim();
  const host = document.getElementById("server-host").value.trim();
  const port = parseInt(document.getElementById("server-port").value, 10) || 22;
  const username = document.getElementById("server-username").value.trim();
  const authType = document.getElementById("server-auth-type").value;
  const keyPath = document.getElementById("server-key-path").value.trim();

  clearFieldErrors("server-form");
  let valid = true;

  if (!label) { setFieldError("server-label", "Required"); valid = false; }
  if (!host) { setFieldError("server-host", "Required"); valid = false; }
  if (!username) { setFieldError("server-username", "Required"); valid = false; }
  if (authType === "key" && !keyPath) { setFieldError("server-key-path", "Required"); valid = false; }
  if (!valid) return;

  const id = editingServerId || slugify(label);
  config.servers[id] = { label, host, port, username, auth: { auth_type: authType, key_path: keyPath } };

  try {
    await persistConfig();
    closeServerModal();
    renderServers();
  } catch (_) {}
}

// ── Projects section ───────────────────────────────────────────────────────

function renderProjects() {
  const list = document.getElementById("project-list");
  const ids = Object.keys(config.projects);

  if (ids.length === 0) {
    list.innerHTML = `<div class="empty-state">
      <div style="font-size:32px">📁</div>
      <p>No projects configured yet.<br>Add a server first, then create a project.</p>
    </div>`;
    return;
  }

  list.innerHTML = ids.map(id => {
    const p = config.projects[id];
    const server = config.servers[p.server];
    const serverLabel = server ? server.label : `(unknown: ${p.server})`;
    const cmds = p.allow_commands ? (p.allowed_commands?.length ? `Commands: ${p.allowed_commands.join(", ")}` : "All commands allowed") : "Commands disabled";
    return `<div class="card">
      <div class="card-body">
        <div class="card-title">${esc(p.label)}</div>
        <div class="card-subtitle">${esc(serverLabel)} · ${esc(p.root_path)}</div>
        <div class="card-subtitle" style="margin-top:2px">${esc(cmds)}</div>
      </div>
      <div class="card-actions">
        <button class="btn-icon" onclick="openProjectModal('${id}')" title="Edit">✏️</button>
        <button class="btn-icon" onclick="deleteProject('${id}')" title="Delete">🗑️</button>
      </div>
    </div>`;
  }).join("");
}

async function deleteProject(id) {
  const p = config.projects[id];
  if (!confirm(`Delete project "${p.label}"?`)) return;
  delete config.projects[id];
  try {
    await persistConfig();
    renderProjects();
  } catch (_) {
    await loadConfig();
  }
}

// ── Project modal ──────────────────────────────────────────────────────────

function openProjectModal(id = null) {
  editingProjectId = id;
  const isEdit = id !== null;
  document.getElementById("project-modal-title").textContent = isEdit ? "Edit Project" : "Add Project";

  const p = isEdit ? config.projects[id] : {
    label: "", server: "", root_path: "", description: "",
    allow_commands: false, allowed_commands: null
  };

  // Populate server dropdown
  const sel = document.getElementById("project-server");
  const serverIds = Object.keys(config.servers);
  if (serverIds.length === 0) {
    showToast("Add a server first", true);
    return;
  }
  sel.innerHTML = serverIds.map(sid =>
    `<option value="${sid}"${p.server === sid ? " selected" : ""}>${esc(config.servers[sid].label)}</option>`
  ).join("");

  document.getElementById("project-label").value = p.label;
  document.getElementById("project-root").value = p.root_path;
  document.getElementById("project-desc").value = p.description;
  document.getElementById("project-allow-commands").checked = p.allow_commands;
  document.getElementById("project-allowed-commands").value = (p.allowed_commands || []).join("\n");

  updateAllowedCommandsVisibility();
  clearFieldErrors("project-form");
  document.getElementById("project-modal").classList.remove("hidden");
}

function closeProjectModal() {
  document.getElementById("project-modal").classList.add("hidden");
}

function updateAllowedCommandsVisibility() {
  const checked = document.getElementById("project-allow-commands").checked;
  document.getElementById("allowed-commands-group").classList.toggle("hidden", !checked);
}

async function saveProject() {
  const label = document.getElementById("project-label").value.trim();
  const server = document.getElementById("project-server").value;
  const root_path = document.getElementById("project-root").value.trim();
  const description = document.getElementById("project-desc").value.trim();
  const allow_commands = document.getElementById("project-allow-commands").checked;
  const rawCmds = document.getElementById("project-allowed-commands").value.trim();
  const allowed_commands = allow_commands && rawCmds
    ? rawCmds.split("\n").map(s => s.trim()).filter(Boolean)
    : null;

  clearFieldErrors("project-form");
  let valid = true;
  if (!label) { setFieldError("project-label", "Required"); valid = false; }
  if (!root_path) { setFieldError("project-root", "Required"); valid = false; }
  if (!valid) return;

  const id = editingProjectId || slugify(label);
  config.projects[id] = { label, server, root_path, description, allow_commands, allowed_commands };

  try {
    await persistConfig();
    closeProjectModal();
    renderProjects();
  } catch (_) {}
}

// ── First-run ──────────────────────────────────────────────────────────────

async function checkFirstRun() {
  try {
    const firstRun = await invoke("is_first_run");
    if (firstRun) {
      document.getElementById("welcome-banner").classList.remove("hidden");
      // Pre-fill the Add Server modal with example values.
      document.getElementById("server-label").value = "My Dev Server";
      document.getElementById("server-host").value = "192.168.1.10";
      document.getElementById("server-username").value = "ubuntu";
      document.getElementById("server-key-path").value = "~/.ssh/id_rsa";
    }
  } catch (_) {}
}

function dismissWelcome() {
  document.getElementById("welcome-banner").classList.add("hidden");
}

// ── Copy Claude Desktop config ─────────────────────────────────────────────

async function copyClaudeConfig() {
  try {
    const exePath = await invoke("get_binary_path");
    const snippet = JSON.stringify({
      mcpServers: {
        "remote-dev-bridge": {
          command: exePath,
          args: ["--mcp-stdio"]
        }
      }
    }, null, 2);
    await navigator.clipboard.writeText(snippet);
    showToast("Claude Desktop config copied to clipboard");
  } catch (e) {
    showToast("Copy failed: " + e, true);
  }
}

// ── Settings section ───────────────────────────────────────────────────────

function renderSettings() {
  const s = config.settings;
  document.getElementById("setting-max-file-size").value = s.max_file_size_kb ?? 1024;
  document.getElementById("setting-ssh-timeout").value = s.ssh_connection_timeout_sec ?? 10;
  document.getElementById("setting-ssh-keepalive").value = s.ssh_keepalive_interval_sec ?? 30;
  document.getElementById("setting-confirm-destructive").checked = s.confirm_destructive !== false;
}

async function saveSettings() {
  config.settings = {
    max_file_size_kb: parseInt(document.getElementById("setting-max-file-size").value, 10) || 1024,
    ssh_connection_timeout_sec: parseInt(document.getElementById("setting-ssh-timeout").value, 10) || 10,
    ssh_keepalive_interval_sec: parseInt(document.getElementById("setting-ssh-keepalive").value, 10) || 30,
    confirm_destructive: document.getElementById("setting-confirm-destructive").checked,
  };
  try {
    await persistConfig();
  } catch (_) {}
}

// ── Helpers ────────────────────────────────────────────────────────────────

function esc(str) {
  return String(str ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function slugify(str) {
  return str.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_|_$/g, "") || Date.now().toString();
}

function setFieldError(inputId, msg) {
  const el = document.getElementById(inputId);
  if (!el) return;
  el.style.borderColor = "var(--danger)";
  const err = document.createElement("div");
  err.className = "field-error";
  err.textContent = msg;
  el.parentNode.appendChild(err);
}

function clearFieldErrors(formId) {
  const form = document.getElementById(formId);
  if (!form) return;
  form.querySelectorAll(".field-error").forEach(e => e.remove());
  form.querySelectorAll("input, select, textarea").forEach(e => e.style.borderColor = "");
}

let toastTimer = null;
function showToast(msg, isError = false) {
  const el = document.getElementById("toast");
  el.textContent = msg;
  el.className = "show" + (isError ? " error" : "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.className = ""; }, 3000);
}
