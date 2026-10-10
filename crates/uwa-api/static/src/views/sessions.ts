import { UwaClient } from "../api";
import type { SessionInfo } from "../types";
import { h, fmtRel, toast } from "../ui";

export function renderSessions(root: HTMLElement, api: UwaClient): () => void {
  const tbody = h("tbody", {});
  const refreshBtn = h("button", { class: "btn secondary", type: "button" }, "Refresh");
  const recoverBtn = h("button", { class: "btn secondary", type: "button" }, "Recover unhealthy");
  const table = h("table", {},
    h("thead", {}, h("tr", {},
      h("th", {}, "Conversation"), h("th", {}, "Tab"),
      h("th", {}, "Age"), h("th", {}, "Idle"), h("th", {}, "Gen"), h("th", {}),
    )),
    tbody,
  );

  const load = async () => {
    try {
      const r = await api.sessions();
      if (r.sessions.length === 0) {
        tbody.replaceChildren(h("tr", {}, h("td", { colspan: "6", class: "muted" }, "No sessions yet.")));
        return;
      }
      tbody.replaceChildren(
        ...r.sessions.map((s) => rowFor(s, () => void (async () => {
          try {
            await api.dropSession(s.conversation);
            await load();
          } catch (e) { toast(`drop: ${String(e)}`, "err"); }
        })())),
      );
    } catch (e) {
      toast(`sessions: ${String(e)}`, "err");
    }
  };

  refreshBtn.addEventListener("click", () => void load());
  recoverBtn.addEventListener("click", () => void (async () => {
    try {
      await api.recoverSessions();
      toast("recover done", "ok");
      await load();
    } catch (e) { toast(`recover: ${String(e)}`, "err"); }
  })());

  root.append(
    h("h2", {}, "Sessions"),
    h("p", { class: "lead" }, "Each conversation is pinned to a tab. Idle sessions are reaped automatically."),
    h("div", { style: { display: "flex", gap: "8px", marginBottom: "12px" } }, refreshBtn, recoverBtn),
    h("div", { class: "card" }, table),
  );

  void load();
  const iv = window.setInterval(() => void load(), 4000);
  return () => window.clearInterval(iv);
}

function rowFor(s: SessionInfo, onDrop: () => void): HTMLElement {
  return h("tr", {},
    h("td", { class: "mono" }, s.conversation),
    h("td", { class: "mono" }, s.tab),
    h("td", {}, fmtRel(s.age_secs)),
    h("td", {}, fmtRel(s.idle_secs)),
    h("td", {}, String(s.generation)),
    h("td", {}, h("button", { class: "btn secondary", type: "button", onclick: onDrop }, "Drop")),
  );
}
