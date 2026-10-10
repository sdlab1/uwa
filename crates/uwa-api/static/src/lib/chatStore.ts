import type { ChatMessage } from './types';

const KEY = 'uwa:chat:v1';

export interface ChatSession {
  id: string;
  title: string;
  model: string;
  createdAt: number;
  updatedAt: number;
  messages: ChatMessage[];
}

export function loadSessions(): ChatSession[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as ChatSession[];
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((s) => s && typeof s.id === 'string' && Array.isArray(s.messages));
  } catch {
    return [];
  }
}

export function saveSessions(sessions: ChatSession[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(sessions.slice(0, 50)));
  } catch {
    /* quota exceeded — ignore */
  }
}

export function newSession(model: string): ChatSession {
  const now = Date.now();
  return {
    id: `chat_${now.toString(36)}_${Math.random().toString(36).slice(2, 8)}`,
    title: 'New chat',
    model,
    createdAt: now,
    updatedAt: now,
    messages: [],
  };
}

export function deriveTitle(firstUserText: string): string {
  const line = firstUserText.trim().split('\n')[0] ?? '';
  if (!line) return 'New chat';
  return line.length > 48 ? `${line.slice(0, 45)}…` : line;
}
