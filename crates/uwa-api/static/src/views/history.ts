import { UwaClient } from "../api";
import type { HistoryRecord } from "../types";
import { h, fmtMs, toast } from "../ui";

export function renderHistory(root: HTMLElement, api: UwaClient): () => void {
  const providerFilter = h("select", {},
    h("option", { value: "" }, "all providers"),
  );
  const statusFilter = h("select", {},
    h("option", { value: "" }, "any status"),
    h("option", { value: "success" }, "success"),
    h("option", { value: "error" }, "error"),
    h("option", { value: "pending" }, "pending"),
  );
  const refreshBtn = h("button", { class: "btn secondary", type: "button" }, "Refresh");
  const tbody = h("tbody", {});
  const detail = h("div", { style: { marginTop: "12px" } });

  const table = h("table", {},
    h("thead", {}, h("tr", {},
      h("th", {}, "Time"), h("th", {}, "Provider"), h("th", {}, "Model"),
      h("th", {}, "Status"), h("th", {}, "Total"), h("th", {}, "Finish"), h("th", {}),
    )),
    tbody,
  );

  const showDetail = (rec: HistoryRecord) => {
    detail.replaceChildren(
      h("div", { class: "card" },
        h("div", { class: "card-head" },
          h("h3", {}, `Record ${rec.id}`),
          h("button", { class: "btn secondary", type: "button", onclick: () => detail.replaceChildren() }, "Close"),
        ),
        h("pre", { style: { whiteSpace: "pre-wrap", fontFamily: "ui-monospace, monospace", fontSize: "12px" } },
          JSON.stringify(rec, null, 2)),
      ),
    );
  };

  const load = async () => {
    try {
      const r = await api.history(100, providerFilter.value || undefined, statusFilter.value || undefined);
      if (r.records.length === 0) {
        tbody.replaceChildren(h("tr", {}, h("td", { colspan: "7", class: "muted" }, "No records yet.")));
        return;
      }
      tbody.replaceChildren(
        ...r.records.map((rec) => rowFor(rec, () => showDetail(rec))),
      );
    } catch (e) {
      toast(`history: ${String(e)}`, "err");
      tbody.replaceChildren(h("tr", {}, h("td", { colspan: "7", class: "err" }, String(e))));
    }
  };

  refreshBtn.addEventListener("click", () => void load());
  providerFilter.addEventListener("change", () => void load());
  statusFilter.addEventListener("change", () => void load());

  root.append(
    h("h2", {}, "History"),
    h("p", { class: "lead" }, "Every request uwa has served. Click a row for details."),
    h("div", { style: { display: "flex", gap: "8px", marginBottom: "12px" } },
      providerFilter, statusFilter, refreshBtn,
    ),
    h("div", { class: "card" }, table),
    detail,
  );

  // Populate provider filter from /v1/provider/status
  void (async () => {
    try {
      const providers = await api.providers();
      for (const name of Object.keys(providers)) {
        providerFilter.append(h("option", { value: name }, name));
      }
    } catch { /* ignore */ }
  })();

  void load();
  const iv = window.setInterval(() => void load(), 5000);
  return () => window.clearInterval(iv);
}

function rowFor(rec: HistoryRecord, onClick: () => void): HTMLElement {
  const t = rec.started_at
    ? new Date(rec.started_at.secs_since_epoch * 1000).toLocaleTimeString()
    : "—";
  const finish = rec.response?.finish_reason ?? "—";
  const statusClass = rec.status === "success" ? "ok" : rec.status === "error" ? "err" : "warn";
  return h("tr", { style: { cursor: "pointer" }, onclick: onClick },
    h("td", {}, t),
    h("td", {}, rec.provider),
    h("td", {}, rec.model),
    h("td", { class: statusClass }, rec.status),
    h("td", {}, fmtMs(rec.timing.total_ms)),
    h("td", {}, finish),
    h("td", {}, rec.response?.tool_calls ? `tool×${rec.response.tool_calls}` : ""),
  );
}
