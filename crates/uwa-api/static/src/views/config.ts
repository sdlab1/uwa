import { UwaClient } from "../api";
import type { ProviderInfo } from "../types";
import { h } from "../ui";

export function renderConfig(root: HTMLElement, api: UwaClient): () => void {
  const body = h("div", { class: "card" }, h("p", { class: "muted" }, "Loading…"));

  const load = async () => {
    try {
      const p = await api.providers();
      const entries = Object.entries(p);
      if (entries.length === 0) {
        body.replaceChildren(h("p", { class: "muted" }, "No providers configured."));
        return;
      }
      body.replaceChildren(
        ...entries.map(([name, info]) => providerCard(name, info)),
      );
    } catch (e) {
      body.replaceChildren(h("div", { class: "banner err" }, String(e)));
    }
  };

  root.append(
    h("h2", {}, "Config"),
    h("p", { class: "lead" }, "Read-only view of the loaded provider config. Edit uwa.toml and restart to change."),
    body,
  );

  void load();
  return () => {};
}

function providerCard(name: string, info: ProviderInfo): HTMLElement {
  return h("div", { style: { marginBottom: "16px" } },
    h("h3", { style: { marginBottom: "6px", color: "var(--text)", textTransform: "none", letterSpacing: 0, fontSize: "15px" } },
      name,
      h("span", { class: "muted", style: { marginLeft: "8px", fontSize: "12px", fontWeight: "400" } },
        `${info.extraction} — ${info.backend ?? "default backend"}`),
    ),
    h("dl", { class: "kv" },
      h("dt", {}, "URL patterns"),
      h("dd", {}, info.url_patterns.join(", ")),
      h("dt", {}, "input"),
      h("dd", {}, info.selectors.input ?? "—"),
      h("dt", {}, "send_button"),
      h("dd", {}, info.selectors.send_button ?? "—"),
      h("dt", {}, "assistant_message"),
      h("dd", {}, info.selectors.assistant_message ?? "—"),
      h("dt", {}, "capabilities"),
      h("dd", {},
        `streams=${info.capabilities.streams} tool_calls=${info.capabilities.tool_calls} vision=${info.capabilities.vision}`),
    ),
  );
}
