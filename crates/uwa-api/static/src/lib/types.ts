export interface HealthResp { status: string; }
export interface ReadyResp { status: string; providers_loaded: boolean; models: number; }

export interface ModelObject { id: string; object: string; created: number; owned_by: string; }
export interface ModelList  { object: string; data: ModelObject[]; }

export interface TabInfo { id: string; url: string; }
export interface PoolStatus { total_tabs: number; tabs: TabInfo[]; }

export interface ProviderSelectors {
  input: string | null;
  send_button: string | null;
  assistant_message: string | null;
}
export interface ProviderCaps {
  streams: boolean; tool_calls: boolean; vision: boolean;
  max_context_tokens: number | null;
}
export interface ProviderInfo {
  name: string; enabled: boolean; url_patterns: string[];
  selectors: ProviderSelectors; capabilities: ProviderCaps;
  extraction: string; backend: string | null;
}
export type ProviderMap = Record<string, ProviderInfo>;

export interface SelectorTestResp {
  matched: number; first_text: string | null;
  duration_ms: number; error: string | null;
}

export interface HistoryRecord {
  id: string;
  provider: string;
  model: string;
  status: 'success' | 'error' | 'pending' | string;
  started_at: { secs_since_epoch: number; nanos_since_epoch: number } | null;
  error: string | null;
  response: { text_preview: string; finish_reason: string; tool_calls: number } | null;
  request: { user_preview: string; messages: number; tools: number; stream: boolean };
  timing: {
    total_ms: number; send_ms: number; wait_ms: number; acquisition_ms: number;
    ttft_ms?: number;
    rounds?: Array<{ round: number; send_ms: number; wait_ms: number; tool_calls: number }>;
  };
}
export interface HistoryResp { count: number; buffer_size: number; records: HistoryRecord[]; }

export interface ProviderStats {
  provider: string; total: number; success: number; error: number; pending: number;
  error_rate: number;
  avg_total_ms: number; p50_total_ms: number; p95_total_ms: number;
  avg_send_ms?: number; avg_wait_ms?: number;
  avg_ttft_ms?: number; p50_ttft_ms?: number; p95_ttft_ms?: number;
  tabs_used?: number; last_seen?: unknown;
  network_count?: number; dom_count?: number;
}
export interface StatsResp {
  window: string;
  total: number; success: number; error: number; pending: number;
  avg_total_ms: number; p50_total_ms: number; p95_total_ms: number;
  avg_ttft_ms?: number; p50_ttft_ms?: number; p95_ttft_ms?: number;
  finish_reasons: Record<string, number>;
  providers: ProviderStats[];
  tab_utilization: Record<string, number>;
  requests_per_minute: number[];
}

export interface SessionInfo {
  conversation: string; tab: string;
  age_secs: number; idle_secs: number; generation: number;
}
export interface SessionsResp { sessions: SessionInfo[]; }

export interface LogEvent {
  level: string; target: string; message: string; fields?: string;
}

export type Role = 'system' | 'user' | 'assistant' | 'tool';
export interface ChatMessage {
  role: Role;
  content: string | null;
  tool_calls?: ToolCallRef[] | null;
  tool_call_id?: string | null;
}
export interface ToolCallRef {
  id: string;
  type: 'function';
  function: { name: string; arguments: string };
}
export interface ChatRequest {
  model: string;
  messages: ChatMessage[];
  stream?: boolean;
  tools?: unknown[];
  tool_choice?: unknown;
}
export interface ChatResponse {
  id: string;
  object: string;
  created: number;
  model: string;
  choices: Array<{ index: number; message: ChatMessage; finish_reason: string | null }>;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number };
}
