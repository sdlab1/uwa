import type {
  ChatRequest, ChatResponse, HealthResp, HistoryResp, ModelList, PoolStatus,
  ProviderMap, ReadyResp, SelectorTestResp, SessionsResp, StatsResp,
} from './types';

const KEY_STORAGE = 'uwa:apiKey';

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

let currentKey = localStorage.getItem(KEY_STORAGE) ?? '';
const keyListeners = new Set<(key: string) => void>();

export const getApiKey = (): string => currentKey;
export function setApiKey(key: string): void {
  currentKey = key;
  if (key) localStorage.setItem(KEY_STORAGE, key);
  else localStorage.removeItem(KEY_STORAGE);
  for (const fn of keyListeners) fn(key);
}
export function onKeyChange(fn: (key: string) => void): () => void {
  keyListeners.add(fn);
  return () => keyListeners.delete(fn);
}

function headers(extra: Record<string, string> = {}): Record<string, string> {
  const h: Record<string, string> = { 'content-type': 'application/json', ...extra };
  if (currentKey) h.authorization = `Bearer ${currentKey}`;
  return h;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, {
    ...init,
    headers: { ...headers(), ...((init?.headers as Record<string, string>) ?? {}) },
  });
  if (!r.ok) {
    let code = 'http_error';
    let message = `${r.status} ${r.statusText}`;
    try {
      const body = (await r.json()) as { error?: { message?: string; code?: string } };
      if (body?.error) {
        message = body.error.message ?? message;
        code = body.error.code ?? code;
      }
    } catch { /* non-JSON */ }
    throw new ApiError(r.status, code, message);
  }
  if (r.status === 204) return undefined as T;
  return (await r.json()) as T;
}

// === public ===
export const getHealth   = () => request<HealthResp>('/healthz');
export const getReady    = () => request<ReadyResp>('/readyz');

// === key-gated ===
export const getModels   = () => request<ModelList>('/v1/models');
export const getProviders= () => request<ProviderMap>('/v1/provider/status');
export const getPool     = () => request<PoolStatus>('/api/pool/status');

export const getStats = (window?: 'last50' | 'last200' | 'all') =>
  request<StatsResp>(`/admin/stats${window ? `?window=${window}` : ''}`);

export function getHistory(limit = 100, provider?: string, status?: string): Promise<HistoryResp> {
  const p = new URLSearchParams({ limit: String(limit) });
  if (provider) p.set('provider', provider);
  if (status) p.set('status', status);
  return request<HistoryResp>(`/admin/history?${p}`);
}

export const getHistoryRecord = (id: string) =>
  request<unknown>(`/admin/history/${encodeURIComponent(id)}`);

export const getSessions = () => request<SessionsResp>('/admin/sessions');

export const dropSession = (id: string) =>
  request<{ removed: boolean }>(`/admin/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const recoverSessions = () =>
  request<{ dropped_tabs: string[] }>('/admin/sessions/recover', { method: 'POST' });

export const selectorTest = (b: { provider: string; selector: string; tab_id?: string }) =>
  request<SelectorTestResp>('/admin/selector-test', { method: 'POST', body: JSON.stringify(b) });

export const selectorGenerate = (b: { provider: string; tab_id?: string }) =>
  request<unknown>('/admin/selector-generate', { method: 'POST', body: JSON.stringify(b) });

export const selectorApply = (b: {
  provider: string;
  input?: string;
  send_button?: string;
  assistant_message?: string;
  persist?: boolean;
}) => request<unknown>('/admin/selector-apply', { method: 'POST', body: JSON.stringify(b) });

// === chat ===
export async function chat(req: ChatRequest): Promise<ChatResponse> {
  return request<ChatResponse>('/v1/chat/completions', {
    method: 'POST',
    body: JSON.stringify({ ...req, stream: false }),
  });
}

export interface StreamHandlers {
  onDelta?:     (content: string) => void;
  onToolCall?:  (tc: { index: number; id?: string; name?: string; arguments?: string }) => void;
  onFirstToken?: () => void;
  onChunk?:     (raw: unknown) => void;
  signal?:      AbortSignal;
}

/** Real SSE: emits per-chunk, aggregates and returns the final text. */
export async function chatStream(req: ChatRequest, h: StreamHandlers = {}): Promise<string> {
  const r = await fetch('/v1/chat/completions', {
    method: 'POST',
    headers: headers({ accept: 'text/event-stream' }),
    body: JSON.stringify({ ...req, stream: true }),
    signal: h.signal,
  });
  if (!r.ok || !r.body) {
    let msg = `${r.status} ${r.statusText}`;
    try {
      const j = (await r.json()) as { error?: { message?: string } };
      msg = j?.error?.message ?? msg;
    } catch { /* ignore */ }
    throw new ApiError(r.status, 'stream_failed', msg);
  }

  const reader = r.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  let acc = '';
  let seenFirst = false;

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });

    let idx: number;
    while ((idx = buf.indexOf('\n\n')) >= 0) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);

      const dataLines = frame
        .split('\n')
        .filter((l) => l.startsWith('data:'))
        .map((l) => l.slice(5).trimStart());
      if (dataLines.length === 0) continue;

      const payload = dataLines.join('\n');
      if (payload === '[DONE]') return acc;

      let ev: { choices?: Array<{ delta?: { content?: string; tool_calls?: unknown[] } }> };
      try { ev = JSON.parse(payload); } catch { continue; }
      h.onChunk?.(ev);

      const delta = ev.choices?.[0]?.delta;
      if (!delta) continue;

      if (typeof delta.content === 'string' && delta.content.length > 0) {
        if (!seenFirst) { seenFirst = true; h.onFirstToken?.(); }
        acc += delta.content;
        h.onDelta?.(delta.content);
      }
      if (Array.isArray(delta.tool_calls)) {
        for (const raw of delta.tool_calls) {
          const tc = raw as Record<string, unknown>;
          const fn = (tc['function'] ?? {}) as Record<string, unknown>;
          h.onToolCall?.({
            index: Number(tc['index'] ?? 0),
            id: typeof tc['id'] === 'string' ? tc['id'] : undefined,
            name: typeof fn['name'] === 'string' ? fn['name'] : undefined,
            arguments: typeof fn['arguments'] === 'string' ? fn['arguments'] : undefined,
          });
        }
      }
    }
  }
  return acc;
}

export function streamLogs(
  onEvent: (ev: unknown) => void,
  onError?: (e: Event) => void,
): () => void {
  const key = currentKey;
  const url = `/admin/logs/stream${key ? `?api_key=${encodeURIComponent(key)}` : ''}`;
  const src = new EventSource(url);
  src.onmessage = (e) => {
    try { onEvent(JSON.parse(e.data)); } catch { /* ignore */ }
  };
  src.onerror = (e) => onError?.(e);
  return () => src.close();
}
