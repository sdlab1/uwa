import { UwaClient } from "../api";
import type { ReadyResp, StatsResp } from "../types";
import { h, fmtMs } from "../ui";

export function renderDashboard(root: HTMLElement, api: UwaClient): () => void {
  const statsCard = h("div", { class: "card" });
  const readyCard = h("div", { class: "card" });
  const perProvider = h("div", { class: "card" });
  root.append(
    h("h2", {}, "Dashboard"),
    h("p", { class: "lead" }, "Live view of requests, providers and readiness."),
    readyCard,
    statsCard,
    perProvider,
  );

  let timer: number | undefined;

  const refresh = async () => {
    try {
      const ready = await api.ready();
      renderReady(readyCard, ready);
    } catch (e) {
      readyCard.replaceChildren(h("div", { class: "banner err" }, `readyz failed: ${String(e)}`));
    }
    try {
      const stats = await api.stats();
      renderStats(statsCard, perProvider, stats);
    } catch (e) {
      // history might not be configured — show a muted note, not an error
      statsCard.replaceChildren(h("h3", {}, "Stats"), h("p", { class: "muted" }, `not available: ${String(e)}`));
      perProvider.replaceChildren();
    }
  };

  refresh();
  timer = window.setInterval(refresh, 3000);

  return () => {
    if (timer !== undefined) window.clearInterval(timer);
  };
}

function renderReady(node: HTMLElement, r: ReadyResp): void {
  node.replaceChildren(
    h("div", { class: "card-head" },
      h("h3", {}, "Readiness"),
      h("span", { class: r.providers_loaded ? "pill pill-ok" : "pill pill-warn" }, r.status),
    ),
    h("dl", { class: "kv" },
      h("dt", {}, "providers"), h("dd", {}, r.providers_loaded ? "loaded" : "none"),
      h("dt", {}, "models"),    h("dd", {}, String(r.models)),
    ),
  );
}

function renderStats(card: HTMLElement, perProviderCard: HTMLElement, s: StatsResp): void {
  const cards: Array<[string, string]> = [
    ["Total", String(s.total)],
    ["Success", String(s.success)],
    ["Error", String(s.error)],
    ["Avg", fmtMs(s.avg_total_ms)],
    ["p50", fmtMs(s.p50_total_ms)],
    ["p95", fmtMs(s.p95_total_ms)],
  ];
  const grid = h(
    "div",
    { class: "grid-2" },
    ...cards.map(([k, v]) =>
      h("div", { style: { background: "var(--panel-2)", border: "1px solid var(--border)", padding: "10px 12px", borderRadius: "var(--radius-sm)" } },
        h("div", { class: "muted", style: { fontSize: "11px", textTransform: "uppercase", letterSpacing: "0.5px" } }, k),
        h("div", { style: { fontSize: "18px", fontWeight: "600", marginTop: "2px" } }, v),
      ),
    ),
  );

  const finish = Object.entries(s.finish_reasons ?? {}).map(([k, v]) =>
    h("li", {}, `${k}: ${v}`),
  );

  card.replaceChildren(
    h("div", { class: "card-head" },
      h("h3", {}, "Request stats"),
      h("span", { class: "muted", style: { fontSize: "12px" } }, `window: ${s.window}`),
    ),
    grid,
    finish.length > 0
      ? h("div", { style: { marginTop: "12px" } },
          h("div", { class: "muted", style: { marginBottom: "6px" } }, "Finish reasons"),
          h("ul", { style: { margin: 0, paddingLeft: "18px" } }, ...finish),
        )
      : h("div"),
  );

  if (!s.providers || s.providers.length === 0) {
    perProviderCard.replaceChildren(h("h3", {}, "Per provider"), h("p", { class: "muted" }, "no data yet"));
    return;
  }

  const tbody = h("tbody", {});
  for (const p of s.providers) {
    tbody.append(h("tr", {},
      h("td", {}, p.provider),
      h("td", {}, String(p.total)),
      h("td", { class: p.error > 0 ? "err" : "" }, `${(p.error_rate * 100).toFixed(1)}%`),
      h("td", {}, fmtMs(p.avg_total_ms)),
      h("td", {}, fmtMs(p.p95_total_ms)),
    ));
  }
  perProviderCard.replaceChildren(
    h("div", { class: "card-head" }, h("h3", {}, "Per provider")),
    h("table", {},
      h("thead", {}, h("tr", {},
        h("th", {}, "Provider"), h("th", {}, "Total"),
        h("th", {}, "Err %"), h("th", {}, "Avg"), h("th", {}, "p95"),
      )),
      tbody,
    ),
  );
}
