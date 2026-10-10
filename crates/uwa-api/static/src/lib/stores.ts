import { writable } from 'svelte/store';

export type ToastKind = 'info' | 'ok' | 'warn' | 'err';
export type Toast = { id: number; msg: string; kind: ToastKind };

export const toasts = writable<Toast[]>([]);
let toastId = 0;

export function toast(msg: string, kind: ToastKind = 'info', ms = 4000): void {
  const id = ++toastId;
  toasts.update((t) => [...t, { id, msg, kind }]);
  setTimeout(() => toasts.update((t) => t.filter((x) => x.id !== id)), ms);
}

// --- auth ---
export const apiKey = writable<string>(localStorage.getItem('uwa:apiKey') ?? '');

// --- server ---
export type ServerStatus = 'unknown' | 'ok' | 'degraded' | 'down';
export const serverStatus = writable<ServerStatus>('unknown');

// --- live stream status (used by Dashboard + Playground) ---
export type StreamPhase =
  | 'idle' | 'connecting' | 'waiting' | 'first-token'
  | 'streaming' | 'done' | 'error';

export type StreamState = {
  phase: StreamPhase;
  startedAt?: number;
  firstTokenAt?: number;
  finishedAt?: number;
  tokens: number;
  chars: number;
  error?: string;
  ttftMs?: number;
  totalMs?: number;
};

export const emptyStream: StreamState = { phase: 'idle', tokens: 0, chars: 0 };
export const stream = writable<StreamState>({ ...emptyStream });

export function streamStart(): void {
  stream.set({ phase: 'connecting', startedAt: performance.now(), tokens: 0, chars: 0 });
}
export function streamFirstToken(): void {
  stream.update((s) => {
    if (s.firstTokenAt) return s;
    const now = performance.now();
    return { ...s, phase: 'first-token', firstTokenAt: now, ttftMs: now - (s.startedAt ?? now) };
  });
}
export function streamDelta(nChars: number): void {
  stream.update((s) => ({ ...s, phase: 'streaming', chars: s.chars + nChars, tokens: s.tokens + 1 }));
}
export function streamDone(): void {
  stream.update((s) => {
    const now = performance.now();
    return { ...s, phase: 'done', finishedAt: now, totalMs: now - (s.startedAt ?? now) };
  });
}
export function streamError(msg: string): void {
  stream.update((s) => ({ ...s, phase: 'error', error: msg }));
}
