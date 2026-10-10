import { UwaClient, getApiKey, setApiKey, onKeyChange, notifyKeyChange, ApiError } from "./api";
import { Router, type Route } from "./router";
import { dialog, h } from "./ui";
import { renderDashboard } from "./views/dashboard";
import { renderSetup } from "./views/setup";
import { renderPlayground } from "./views/playground";
import { renderHistory } from "./views/history";
import { renderSessions } from "./views/sessions";
import { renderConfig } from "./views/config";
import { renderLogs } from "./views/logs";

const api = new UwaClient();

// ---------- API key handling ----------

function updateKeyPill(): void {
  const pill = document.getElementById("key-status");
  if (!pill) return;
  const key = getApiKey();
  if (key) {
    pill.className = "pill pill-ok";
    pill.textContent = "key: set";
  } else {
    pill.className = "pill pill-warn";
    pill.textContent = "key: none";
  }
}

async function promptForKey(): Promise<void> {
  const input = h("input", { type: "password", placeholder: "API key", value: getApiKey() }) as HTMLInputElement;
  const body = h("div", {},
    h("p", { class: "muted", style: { marginTop: 0 } },
      "Your key is stored locally in this browser. It's the same value as [server].api_key in uwa.toml."),
    h("div", { class: "field" }, h("label", {}, "API key"), input),
  );
  const r = await dialog({ title: "Set API key", body, primaryLabel: "Save", secondaryLabel: "Clear" });
  if (r === "primary") {
    setApiKey(input.value.trim());
    notifyKeyChange();
  } else if (r === "secondary") {
    setApiKey("");
    notifyKeyChange();
  }
}

// ---------- Server health pill ----------

async function updateServerPill(): Promise<void> {
  const pill = document.getElementById("server-status");
  if (!pill) return;
  try {
    const r = await api.health();
    if (r.status === "ok") {
      pill.className = "pill pill-ok";
      pill.textContent = "server: ok";
    } else {
      pill.className = "pill pill-warn";
      pill.textContent = `server: ${r.status}`;
    }
  } catch {
    pill.className = "pill pill-err";
    pill.textContent = "server: down";
  }
}

// ---------- Global 401 handler ----------
// Any 401 from the API prompts the key dialog once and retries nothing —
// the user re-runs their action.

let prompted = false;
window.addEventListener("unhandledrejection", (e) => {
  const reason = e.reason;
  if (reason instanceof ApiError && reason.status === 401 && !prompted) {
    prompted = true;
    void promptForKey().finally(() => { prompted = false; });
  }
});

// ---------- Routes ----------

const routes: Route[] = [
  { path: "/setup",      label: "Setup",      render: (r) => renderSetup(r, api) },
  { path: "/",           label: "Dashboard",  render: (r) => renderDashboard(r, api) },
  { path: "/playground", label: "Playground", render: (r) => renderPlayground(r, api) },
  { path: "/history",    label: "History",    render: (r) => renderHistory(r, api) },
  { path: "/sessions",   label: "Sessions",   render: (r) => renderSessions(r, api) },
  { path: "/config",     label: "Config",     render: (r) => renderConfig(r, api) },
  { path: "/logs",       label: "Logs",       render: (r) => renderLogs(r, api) },
];

// ---------- Boot ----------

function boot(): void {
  updateKeyPill();
  void updateServerPill();
  onKeyChange(updateKeyPill);

  document.getElementById("key-btn")?.addEventListener("click", () => void promptForKey());

  // Landing route: if setup was never finished, go there first.
  const done = localStorage.getItem("uwa:setup:done") === "1";
  const initial = done || location.hash ? undefined : "/setup";

  const router = new Router(routes, "/");
  if (initial) {
    // Set hash without triggering the initial handle twice.
    history.replaceState(null, "", `#${initial}`);
  }
  router.start();

  setInterval(() => void updateServerPill(), 5000);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot, { once: true });
} else {
  boot();
}
