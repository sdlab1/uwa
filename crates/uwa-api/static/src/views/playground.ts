import { UwaClient, ApiError } from "../api";
import type { ChatMessage, ModelObject } from "../types";
import { h, toast } from "../ui";

export function renderPlayground(root: HTMLElement, api: UwaClient): () => void {
  const modelSelect = h("select", {});
  const streamToggle = h("input", { type: "checkbox", checked: true }) as HTMLInputElement;
  const userInput = h("textarea", { rows: 4, placeholder: "Type a message…" }) as HTMLTextAreaElement;
  const sendBtn = h("button", { class: "btn", type: "button" }, "Send") as HTMLButtonElement;
  const cancelBtn = h("button", { class: "btn secondary", type: "button", disabled: true }, "Cancel") as HTMLButtonElement;
  const clearBtn = h("button", { class: "btn secondary", type: "button" }, "Clear") as HTMLButtonElement;
  const responseBox = h("div", { class: "response-box" }, "Response will appear here…");
  const meta = h("div", { class: "muted", style: { fontSize: "12px", minHeight: "1.5em" } });

  let abort: AbortController | null = null;

  const refresh = async () => {
    try {
      const r = await api.models();
      modelSelect.replaceChildren(...r.data.map((m: ModelObject) => h("option", { value: m.id }, `${m.id} (${m.owned_by})`)));
    } catch (e) {
      toast(`Failed to load models: ${String(e)}`, "err");
    }
  };

  const send = async () => {
    const model = modelSelect.value;
    const text = userInput.value.trim();
    if (!model || !text) return;

    responseBox.replaceChildren();
    responseBox.classList.toggle("streaming", streamToggle.checked);
    meta.textContent = "";
    abort = new AbortController();
    sendBtn.disabled = true;
    cancelBtn.disabled = false;

    const messages: ChatMessage[] = [{ role: "user", content: text }];
    const started = performance.now();

    try {
      if (streamToggle.checked) {
        let acc = "";
        const toolCalls: Record<number, { id?: string; name?: string; arguments: string }> = {};
        const toolCallBox = h("div", {});
        responseBox.replaceChildren(h("div", { style: { whiteSpace: "pre-wrap" } }, ""));
        const textNode = responseBox.firstChild as HTMLElement;
        await api.chatStream(
          { model, messages, stream: true },
          {
            signal: abort.signal,
            onDelta: (chunk) => {
              acc += chunk;
              textNode.textContent = acc;
              responseBox.scrollTop = responseBox.scrollHeight;
            },
            onToolCall: (tc) => {
              const slot = (toolCalls[tc.index] ??= { arguments: "" });
              if (tc.id) slot.id = tc.id;
              if (tc.name) slot.name = tc.name;
              if (tc.arguments) slot.arguments += tc.arguments;
              // Render/refresh tool calls
              toolCallBox.replaceChildren(
                ...Object.entries(toolCalls).map(([i, c]) =>
                  h("div", { class: "tool-call" },
                    h("div", {}, `#${i} ${c.name ?? "?"} (${c.id ?? "no id"})`),
                    h("div", { style: { marginTop: "4px" } }, c.arguments || "(no args yet)"),
                  ),
                ),
              );
              if (!toolCallBox.parentElement) responseBox.append(toolCallBox);
            },
          },
        );
      } else {
        const r = await api.chat({ model, messages });
        const msg = r.choices[0]?.message;
        const nodes: Node[] = [];
        if (msg?.content) nodes.push(h("div", { style: { whiteSpace: "pre-wrap" } }, msg.content));
        if (msg?.tool_calls && msg.tool_calls.length > 0) {
          for (const tc of msg.tool_calls) {
            nodes.push(
              h("div", { class: "tool-call" },
                h("div", {}, `${tc.function.name} (${tc.id})`),
                h("div", { style: { marginTop: "4px" } }, tc.function.arguments),
              ),
            );
          }
        }
        if (nodes.length === 0) nodes.push(h("div", { class: "muted" }, "(empty response)"));
        responseBox.replaceChildren(...nodes);
      }
      const elapsed = performance.now() - started;
      meta.textContent = `${model} — ${elapsed.toFixed(0)} ms`;
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") {
        responseBox.append(h("div", { class: "muted" }, "\n(cancelled)"));
      } else if (e instanceof ApiError) {
        responseBox.replaceChildren(
          h("div", { class: "err" }, `${e.status} ${e.code}`),
          h("div", { style: { marginTop: "6px" } }, e.message),
        );
      } else {
        responseBox.replaceChildren(h("div", { class: "err" }, String(e)));
      }
    } finally {
      responseBox.classList.remove("streaming");
      sendBtn.disabled = false;
      cancelBtn.disabled = true;
      abort = null;
    }
  };

  cancelBtn.addEventListener("click", () => abort?.abort());
  clearBtn.addEventListener("click", () => {
    responseBox.replaceChildren(h("div", { class: "muted" }, "Response will appear here…"));
    meta.textContent = "";
  });
  sendBtn.addEventListener("click", () => void send());
  userInput.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      void send();
    }
  });

  root.append(
    h("h2", {}, "Playground"),
    h("p", { class: "lead" }, "Send messages through uwa. Streaming works end-to-end."),
    h("div", { class: "playground-grid" },
      h("div", {},
        h("div", { class: "field" }, h("label", {}, "Model"), modelSelect),
        h("div", { class: "field" }, h("label", {}, "Message"), userInput),
        h("div", { class: "field", style: { flexDirection: "row", alignItems: "center", gap: "8px" } },
          streamToggle,
          h("label", { style: { margin: 0 } }, "stream (SSE)"),
        ),
        h("div", { style: { display: "flex", gap: "8px" } }, sendBtn, cancelBtn, clearBtn),
        h("p", { class: "muted", style: { fontSize: "12px", marginTop: "6px" } },
          "⌘/Ctrl + Enter to send"),
      ),
      h("div", {},
        h("div", { class: "card-head" },
          h("h3", {}, "Response"),
          meta,
        ),
        responseBox,
      ),
    ),
  );

  void refresh();
  return () => {
    abort?.abort();
  };
}
