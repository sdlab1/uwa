import type {
  ChatRequest, ChatResponse, HealthResp, HistoryResp, ModelList, PoolStatus,
  ProviderMap, ReadyResp, SelectorTestResp, SessionsResp, StatsResp,
} from "./types";

const KEY_STORAGE = "uwa:apiKey";

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export function getApiKey(): string {
  return localStorage.getItem(KEY_STORAGE) ?? "";
}

export function setApiKey(key: string): void {
  if (key) localStorage.setItem(KEY_STORAGE, key);
  else localStorage.removeItem(KEY_STORAGE);
}

/** Listeners fire when the key changes, so the header pill can update. */
const keyListeners: Array<() => void> = [];
export function onKeyChange(fn: () => void): () => void {
  keyListeners.push(fn);
  return () => {
    const i = keyListeners.indexOf(fn);
    if (i >= 0) keyListeners.splice(i, 1);
  };
}
export function notifyKeyChange(): void {
  for (const fn of keyListeners) fn();
}

export class UwaClient {
  constructor(private readonly base: string = "") {}

  private headers(extra: Record<string, string> = {}): Record<string, string> {
    const key = getApiKey();
    const h: Record<string, string> = { "content-type": "application/json", ...extra };
    if (key) h["authorization"] = `Bearer ${key}`;
    return h;
  }

  private url(path: string): string {
    return `${this.base}${path}`;
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const r = await fetch(this.url(path), { ...init, headers: this.headers(init?.headers as Record<string, string>) });
    if (!r.ok) {
      let code = "http_error";
      let message = `${r.status} ${r.statusText}`;
      try {
        const body = (await r.json()) as { error?: { message?: string; code?: string } };
        if (body?.error) {
          message = body.error.message ?? message;
          code = body.error.code ?? code;
        }
      } catch {
        // non-JSON body — keep the status text
      }
      throw new ApiError(r.status, code, message);
    }
    if (r.status === 204) return undefined as unknown as T;
    return (await r.json()) as T;
  }

  // ---------- public (no key) ----------
  health(): Promise<HealthResp> { return this.request("/healthz"); }
  ready(): Promise<ReadyResp> { return this.request("/readyz"); }

  // ---------- key-gated ----------
  models(): Promise<ModelList> { return this.request("/v1/models"); }
  providers(): Promise<ProviderMap> { return this.request("/v1/provider/status"); }
  pool(): Promise<PoolStatus> { return this.request("/api/pool/status"); }

  selectorTest(body: { provider: string; selector: string; tab_id?: string }): Promise<SelectorTestResp> {
    return this.request("/admin/selector-test", { method: "POST", body: JSON.stringify(body) });
  }

  history(limit = 50, provider?: string, status?: string): Promise<HistoryResp> {
    const p = new URLSearchParams({ limit: String(limit) });
    if (provider) p.set("provider", provider);
    if (status) p.set("status", status);
    return this.request(`/admin/history?${p.toString()}`);
  }

  stats(): Promise<StatsResp> { return this.request("/admin/stats"); }

  sessions(): Promise<SessionsResp> { return this.request("/admin/sessions"); }

  dropSession(id: string): Promise<unknown> {
    return this.request(`/admin/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
  }

  recoverSessions(): Promise<unknown> {
    return this.request("/admin/sessions/recover", { method: "POST" });
  }

  // ---------- chat ----------
  async chat(req: ChatRequest): Promise<ChatResponse> {
    return this.request("/v1/chat/completions", {
      method: "POST",
      body: JSON.stringify({ ...req, stream: false }),
    });
  }

  /**
   * Streaming chat. Calls `onDelta(content, raw)` for every chunk and
   * `onToolCall(partial)` for function-call fragments. Returns the final
   * aggregated text.
   */
  async chatStream(
    req: ChatRequest,
    handlers: {
      onDelta?: (content: string, raw: unknown) => void;
      onToolCall?: (partial: { index: number; id?: string; name?: string; arguments?: string }) => void;
      signal?: AbortSignal;
    } = {},
  ): Promise<string> {
    const r = await fetch(this.url("/v1/chat/completions"), {
      method: "POST",
      headers: this.headers({ accept: "text/event-stream" }),
      body: JSON.stringify({ ...req, stream: true }),
      signal: handlers.signal,
    });
    if (!r.ok || !r.body) {
      let msg = `${r.status} ${r.statusText}`;
      try {
        const j = (await r.json()) as { error?: { message?: string } };
        msg = j?.error?.message ?? msg;
      } catch { /* ignore */ }
      throw new ApiError(r.status, "stream_failed", msg);
    }

    const reader = r.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    let accumulated = "";

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });

      let idx: number;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, idx);
        buf = buf.slice(idx + 2);

        // SSE frames: one or more `data: ` lines, ending with a blank line.
        const dataLines = frame
          .split("\n")
          .filter((l) => l.startsWith("data:"))
          .map((l) => l.slice(5).trimStart());
        if (dataLines.length === 0) continue;
        const payload = dataLines.join("\n");
        if (payload === "[DONE]") {
          return accumulated;
        }
        let ev: { choices?: Array<{ delta?: { content?: string; tool_calls?: unknown[] } }> };
        try {
          ev = JSON.parse(payload);
        } catch {
          continue;
        }
        const delta = ev.choices?.[0]?.delta;
        if (!delta) continue;
        if (typeof delta.content === "string" && delta.content.length > 0) {
          accumulated += delta.content;
          handlers.onDelta?.(delta.content, ev);
        }
        if (Array.isArray(delta.tool_calls)) {
          for (const raw of delta.tool_calls) {
            const tc = raw as Record<string, unknown>;
            const fn = (tc["function"] ?? {}) as Record<string, unknown>;
            handlers.onToolCall?.({
              index: Number(tc["index"] ?? 0),
              id: typeof tc["id"] === "string" ? tc["id"] : undefined,
              name: typeof fn["name"] === "string" ? fn["name"] : undefined,
              arguments: typeof fn["arguments"] === "string" ? fn["arguments"] : undefined,
            });
          }
        }
      }
    }
    return accumulated;
  }

  /** Subscribe to the live log stream. Returns a closer. */
  streamLogs(onEvent: (ev: unknown) => void): () => void {
    const url = this.url("/admin/logs/stream");
    const key = getApiKey();
    const src = new EventSource(key ? `${url}?api_key=${encodeURIComponent(key)}` : url);
    src.onmessage = (e) => {
      try { onEvent(JSON.parse(e.data)); } catch { /* ignore */ }
    };
    src.onerror = () => {
      // EventSource auto-reconnects; nothing to do.
    };
    return () => src.close();
  }
}
