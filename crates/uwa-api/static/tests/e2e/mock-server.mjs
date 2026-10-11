// Mock backend + static host for E2E tests. Serves the built UI from
// `static/`, mocks every API endpoint the dashboard touches, and exposes
// a small `/__test__/*` control surface for state manipulation.
//
// Run with: node tests/e2e/mock-server.mjs   (Playwright starts it)

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const STATIC_DIR = path.resolve(__dirname, '..', '..'); // crates/uwa-api/static
const PORT = Number(process.env.MOCK_PORT ?? 9876);
const HOST = '127.0.0.1';

// ---------- default state ----------
const DEFAULTS = () => ({
  stats: {
    window: 'last200',
    total: 42, success: 40, error: 2, pending: 0,
    avg_total_ms: 820, p50_total_ms: 700, p95_total_ms: 2100,
    avg_ttft_ms: 320, p50_ttft_ms: 280, p95_ttft_ms: 900,
    finish_reasons: { stop: 38, tool_calls: 2, length: 2 },
    providers: [
      {
        provider: 'chatgpt', total: 30, success: 29, error: 1, pending: 0,
        error_rate: 1 / 30, avg_total_ms: 800, p50_total_ms: 700, p95_total_ms: 1900,
        avg_ttft_ms: 300, p50_ttft_ms: 260, p95_ttft_ms: 800,
        network_count: 28, dom_count: 2, tabs_used: 1,
      },
      {
        provider: 'claude', total: 12, success: 11, error: 1, pending: 0,
        error_rate: 1 / 12, avg_total_ms: 870, p50_total_ms: 700, p95_total_ms: 2400,
        avg_ttft_ms: 380, p50_ttft_ms: 350, p95_ttft_ms: 1000,
        network_count: 12, dom_count: 0, tabs_used: 1,
      },
    ],
    tab_utilization: { tab_abc: 30, tab_def: 12 },
    requests_per_minute: Array.from({ length: 60 }, (_, i) =>
      i % 7 === 0 ? 3 : i % 3 === 0 ? 1 : 0,
    ),
  },
  history: {
    count: 2, buffer_size: 2,
    records: [
      {
        id: 'req_aaa', provider: 'chatgpt', model: 'gpt-4o', status: 'success',
        started_at: { secs_since_epoch: Math.floor(Date.now() / 1000) - 30, nanos_since_epoch: 0 },
        error: null,
        response: { text_preview: 'Hello there', finish_reason: 'stop', tool_calls: 0 },
        request: { user_preview: 'hi', messages: 1, tools: 0, stream: false },
        timing: { total_ms: 800, send_ms: 150, wait_ms: 600, acquisition_ms: 50, ttft_ms: 300 },
      },
      {
        id: 'req_bbb', provider: 'claude', model: 'claude-3-5-sonnet', status: 'success',
        started_at: { secs_since_epoch: Math.floor(Date.now() / 1000) - 10, nanos_since_epoch: 0 },
        error: null,
        response: { text_preview: 'Sure thing', finish_reason: 'tool_calls', tool_calls: 1 },
        request: { user_preview: 'weather?', messages: 1, tools: 1, stream: true },
        timing: { total_ms: 1400, send_ms: 200, wait_ms: 1100, acquisition_ms: 100, ttft_ms: 400 },
      },
    ],
  },
  models: {
    object: 'list',
    data: [
      { id: 'gpt-4o', object: 'model', created: 0, owned_by: 'chatgpt' },
      { id: 'claude-3-5-sonnet', object: 'model', created: 0, owned_by: 'claude' },
    ],
  },
  providers: {
    chatgpt: {
      name: 'chatgpt', enabled: true,
      url_patterns: ['https://chatgpt.com/*'],
      selectors: {
        input: '#prompt-textarea',
        send_button: '[data-testid=send-button]',
        assistant_message: '[data-message-author-role=assistant]',
      },
      capabilities: { streams: true, tool_calls: true, vision: false, max_context_tokens: 128000 },
      extraction: 'network_first', backend: null,
    },
    claude: {
      name: 'claude', enabled: true,
      url_patterns: ['https://claude.ai/*'],
      selectors: {
        input: 'div[contenteditable=true]',
        send_button: "button[aria-label='Send message']",
        assistant_message: '[data-testid=assistant-message]',
      },
      capabilities: { streams: true, tool_calls: true, vision: true, max_context_tokens: 200000 },
      extraction: 'network_first', backend: null,
    },
  },
  pool: {
    total_tabs: 2,
    tabs: [
      { id: 'tab_abc', url: 'https://chatgpt.com/' },
      { id: 'tab_def', url: 'https://claude.ai/' },
    ],
  },
  sessions: {
    sessions: [
      { conversation: 'conv_aaa', tab: 'tab_abc', age_secs: 120, idle_secs: 5, generation: 3 },
      { conversation: 'conv_bbb', tab: 'tab_def', age_secs: 30, idle_secs: 10, generation: 1 },
    ],
  },
  selectorTest: { matched: 3, first_text: 'hello world', duration_ms: 12, error: null },
});

let state = DEFAULTS();
const reset = () => { state = DEFAULTS(); };

// ---------- helpers ----------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function json(res, code, body) {
  res.writeHead(code, {
    'content-type': 'application/json',
    'access-control-allow-origin': '*',
  });
  res.end(JSON.stringify(body));
}

function readBody(req) {
  return new Promise((resolve) => {
    let buf = '';
    req.on('data', (c) => (buf += c));
    req.on('end', () => {
      try { resolve(buf ? JSON.parse(buf) : {}); } catch { resolve({}); }
    });
  });
}

function serveStatic(res, file) {
  const resolved = path.resolve(file);
  if (!resolved.startsWith(STATIC_DIR)) {
    res.writeHead(403); res.end(); return;
  }
  fs.readFile(resolved, (err, buf) => {
    if (err) { res.writeHead(404); res.end('not found'); return; }
    const ext = path.extname(resolved);
    const ct = ext === '.html' ? 'text/html; charset=utf-8'
      : ext === '.js'   ? 'text/javascript; charset=utf-8'
      : ext === '.css'  ? 'text/css; charset=utf-8'
      : ext === '.json' ? 'application/json; charset=utf-8'
      : 'application/octet-stream';
    res.writeHead(200, { 'content-type': ct });
    res.end(buf);
  });
}

function sseHeaders(res) {
  res.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'keep-alive',
    'access-control-allow-origin': '*',
  });
}

// ---------- SSE streams ----------
async function streamChat(res, body) {
  sseHeaders(res);
  const id = 'chatcmpl_test';
  const created = Math.floor(Date.now() / 1000);
  const model = body.model ?? 'gpt-4o';

  const chunk = (delta, finish = null) => JSON.stringify({
    id,
    object: 'chat.completion.chunk',
    created,
    model,
    choices: [{ index: 0, delta, finish_reason: finish }],
  });

  res.write(`data: ${chunk({ role: 'assistant' })}\n\n`);

  // Ten parts at 80 ms each — long enough that Cancel can fire mid-flight.
  const parts = ['Hello', ' from', ' the', ' site', '.', ' This', ' is', ' a', ' complete', ' reply.'];
  for (const p of parts) {
    await sleep(80);
    if (res.writableEnded) return;
    res.write(`data: ${chunk({ content: p })}\n\n`);
  }
  res.write(`data: ${chunk({}, 'stop')}\n\n`);
  res.write('data: [DONE]\n\n');
  res.end();
}

async function streamLogs(req, res) {
  sseHeaders(res);
  // Exactly four events, one of each level that the filter UI exposes.
  // The connection stays open afterwards so the client shows "connected".
  const events = [
    { level: 'INFO',  target: 'uwa',            message: 'listening', fields: '' },
    { level: 'INFO',  target: 'uwa.providers',  message: 'registered', fields: 'chatgpt' },
    { level: 'WARN',  target: 'uwa.session',    message: 'idle sweep', fields: '3' },
    { level: 'ERROR', target: 'uwa.transport',  message: 'retry',      fields: '' },
  ];
  for (const e of events) {
    if (res.writableEnded) return;
    res.write(`data: ${JSON.stringify(e)}\n\n`);
    await sleep(60);
  }
  // Keep the connection open; the client will close it when it leaves.
  await new Promise((resolve) => req.on('close', resolve));
}

// ---------- router ----------
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url ?? '/', `http://${req.headers.host}`);
  const p = url.pathname;
  const m = req.method ?? 'GET';

  // ---- test control ----
  if (p === '/__test__/reset' && m === 'POST') { reset(); return json(res, 200, { ok: true }); }
  if (p === '/__test__/stats' && m === 'POST') { Object.assign(state.stats, await readBody(req)); return json(res, 200, { ok: true }); }
  if (p === '/__test__/history' && m === 'POST') { Object.assign(state.history, await readBody(req)); return json(res, 200, { ok: true }); }
  if (p === '/__test__/selectorTest' && m === 'POST') { Object.assign(state.selectorTest, await readBody(req)); return json(res, 200, { ok: true }); }

  // ---- static ----
  if (p === '/' || p === '/index.html') return serveStatic(res, path.join(STATIC_DIR, 'index.html'));
  if (p.startsWith('/static/')) return serveStatic(res, path.join(STATIC_DIR, p.replace('/static/', '')));

  // ---- health ----
  if (p === '/healthz') return json(res, 200, { status: 'ok' });
  if (p === '/readyz') return json(res, 200, {
    status: 'ok',
    providers_loaded: Object.keys(state.providers).length > 0,
    models: state.models.data.length,
  });

  // ---- catalog ----
  if (p === '/v1/models') return json(res, 200, state.models);
  if (p === '/v1/provider/status') return json(res, 200, state.providers);
  if (p === '/api/pool/status') return json(res, 200, state.pool);

  // ---- admin ----
  if (p === '/admin/stats') return json(res, 200, state.stats);
  if (p === '/admin/history') return json(res, 200, state.history);
  if (p.startsWith('/admin/history/')) {
    const id = p.split('/').pop();
    const rec = state.history.records.find((r) => r.id === id);
    return rec
      ? json(res, 200, rec)
      : json(res, 404, { error: { message: 'not found', code: 'not_found' } });
  }
  if (p === '/admin/sessions') return json(res, 200, state.sessions);
  if (p === '/admin/selector-test' && m === 'POST') return json(res, 200, state.selectorTest);
  if (p === '/admin/selector-generate' && m === 'POST') {
    return json(res, 200, {
      inputs: [{ selector: '#prompt-textarea', evidence: 'placeholder', score: 0.9, tag: 'textarea' }],
      send_buttons: [{ selector: '[data-testid=send-button]', evidence: 'label', score: 0.8, tag: 'button' }],
      assistant_containers: [],
      duration_ms: 12,
    });
  }
  if (p === '/admin/selector-apply' && m === 'POST') {
    return json(res, 200, { applied: false, diff: { input: { old: 'a', new: 'b' } }, hint: 'dry-run' });
  }
  if (p === '/admin/logs/stream') return streamLogs(req, res);

  // ---- chat ----
  if (p === '/v1/chat/completions' && m === 'POST') {
    const body = await readBody(req);
    if (body.stream) return streamChat(res, body);
    return json(res, 200, {
      id: 'chatcmpl_test',
      object: 'chat.completion',
      created: Math.floor(Date.now() / 1000),
      model: body.model ?? 'gpt-4o',
      choices: [{
        index: 0,
        message: { role: 'assistant', content: 'Hello from the site.' },
        finish_reason: 'stop',
      }],
      usage: { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 },
    });
  }

  json(res, 404, { error: { message: `mock: unhandled ${m} ${p}`, code: 'not_found' } });
});

server.listen(PORT, HOST, () => {
  console.log(`[mock] listening on http://${HOST}:${PORT}`);
  console.log(`[mock] static from ${STATIC_DIR}`);
});

for (const sig of ['SIGTERM', 'SIGINT']) {
  process.on(sig, () => server.close(() => process.exit(0)));
}
