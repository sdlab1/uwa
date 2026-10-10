import { UwaClient, getApiKey } from "../api";
import type { LogEvent } from "../types";
import { h } from "../ui";

export function renderLogs(root: HTMLElement, api: UwaClient): () => void {
  const stream = h("div", { class: "log-stream" });
  const toggle = h("button", { class: "btn", type: "button" }, "Start stream") as HTMLButtonElement;
  const clear = h("button", { class: "btn secondary", type: "button" }, "Clear");
  const note = h("span", { class: "muted", style: { fontSize: "12px" } });

  let closeFn: (() => void) | null = null;

  const start = () => {
    if (closeFn) return;
    stream.replaceChildren();
    note.textContent = "connected";
    void getApiKey(); // event source sets its own query param inside api.streamLogs
    closeFn = api.streamLogs((ev) => {
      const e = ev as LogEvent;
      const line = h("div", { class: `log-line log-${e.level}` },
        `[${e.level}] ${e.target}: ${e.message}${e.fields ? " " + e.fields : ""}`);
      stream.append(line);
      if (stream.childElementCount > 2000) stream.firstChild?.remove();
      stream.scrollTop = stream.scrollHeight;
    });
    toggle.textContent = "Stop stream";
  };

  const stop = () => {
    if (!closeFn) return;
    closeFn();
    closeFn = null;
    note.textContent = "stopped";
    toggle.textContent = "Start stream";
  };

  toggle.addEventListener("click", () => (closeFn ? stop() : start()));
  clear.addEventListener("click", () => stream.replaceChildren());

  root.append(
    h("h2", {}, "Logs"),
    h("p", { class: "lead" }, "Live tracing output from uwa. Streamed over SSE from /admin/logs/stream."),
    h("div", { style: { display: "flex", gap: "8px", marginBottom: "8px", alignItems: "center" } },
      toggle, clear, note),
    stream,
  );

  // Autostart
  start();
  return () => stop();
}
