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
  fallbacks?: FallbackCandidate[];
}

export interface FallbackCandidate {
  selector: string;
  matched: number;
  first_text: string | null;
}

export interface RegistryInfo {
  manifest: {
    registry_version: string;
    generated_at: string;
    min_uwa_version: string;
  } | null;
  cache_dir: string | null;
  entries: Record<string, { version: string; updated_at: string }>;
}

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

export interface HistoryRecord {
  id: string;
  provider: string;
  model: string;
  profile?: string | null;
  status: 'success' | 'error' | 'pending' | string;
  started_at: { secs_since_epoch: number; nanos_since_epoch: number } | null;
  error: string | null;
  response: {
    text_preview: string;
    finish_reason: string;
    tool_calls: number;
    pin_mode?: string | null;
    usage?: Usage | null;
  } | null;
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
  tokens_in?: number; tokens_out?: number;
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
  tokens_in?: number;
  tokens_out?: number;
  tokens_in_per_minute?: number[];
  tokens_out_per_minute?: number[];
  profiles?: ProfileStats[];
}

export interface ProfileStats {
  profile: string;
  total: number;
  success: number;
  error: number;
  tokens_in: number;
  tokens_out: number;
}

export interface ProfileSummary {
  name: string;
  label: string;
  notes: string;
  tabs: string[];
  active: boolean;
  named: boolean;
}

export interface ProfileListResp {
  supports_profiles: boolean;
  profiles: ProfileSummary[];
}

export interface SessionInfo {
  conversation: string; profile?: string; tab: string;
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
export type ResponseFormat =
  | { type: 'text' }
  | { type: 'json_object' }
  | {
      type: 'json_schema';
      json_schema: { name: string; description?: string; schema: unknown; strict?: boolean };
    };

export interface ChatRequest {
  model: string;
  messages: ChatMessage[];
  stream?: boolean;
  tools?: unknown[];
  tool_choice?: unknown;
  response_format?: ResponseFormat;
}
export interface ChatResponse {
  id: string;
  object: string;
  created: number;
  model: string;
  choices: Array<{ index: number; message: ChatMessage; finish_reason: string | null }>;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number };
}


export interface RecorderStatus {
  active: boolean;
  id?: string;
  provider?: string;
  tab?: string;
  started_at_ms?: number;
  elapsed_ms?: number;
  events?: number;
  installed?: boolean;
  poll_errors?: number;
}

export interface RecorderStopResponse {
  provider: string;
  event_count: number;
  warnings: string[];
  workflow: { steps: unknown[] };
  toml_snippet: string;
}
