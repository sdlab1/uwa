// Client config snippets. Values interpolate `{{BASE}}` (http://host:port)
// and `{{MODEL}}` (first model from /v1/models). Snippets are deliberately
// plain strings — no templating engine, no eval.
//
// We refuse to *write* client configs. Cursor / Continue / Codex rewrite
// their own settings and any patch we apply is lost on the next save. The
// user pastes; that's the honest UX.

export interface ClientSnippet {
  id: string;
  name: string;
  /** Short "where does this go" line. */
  where: string;
  /** Language hint for the copy block. */
  lang: string;
  /** Render the snippet. */
  render: (ctx: { base: string; model: string; key: string }) => string;
  /** Optional external docs link. */
  docs?: string;
}

export const CLIENTS: ClientSnippet[] = [
  {
    id: 'cursor',
    name: 'Cursor',
    where: 'Settings → Models → OpenAI API Key  ·  add a custom model',
    lang: 'text',
    render: ({ base, model, key }) => `Base URL:   ${base}/v1
API key:    ${key}
Model name: ${model}

Settings → Models:
  · Override OpenAI Base URL:  ${base}/v1
  · OpenAI API Key:            ${key}
  · Add model:                 ${model}`,
    docs: 'https://docs.cursor.com/settings/models',
  },
  {
    id: 'continue',
    name: 'Continue (VS Code / JetBrains)',
    where: '~/.continue/config.json → models[]',
    lang: 'json',
    render: ({ base, model, key }) => JSON.stringify({
      models: [{
        title: 'uwa',
        provider: 'openai',
        model,
        apiBase: `${base}/v1`,
        apiKey: key,
      }],
    }, null, 2),
    docs: 'https://docs.continue.dev',
  },
  {
    id: 'codex',
    name: 'Codex CLI',
    where: '~/.codex/config.toml',
    lang: 'toml',
    render: ({ base, model, key }) => `model = "${model}"
model_provider = "uwa"

[model_providers.uwa]
name = "uwa"
base_url = "${base}/v1"
env_key = "UWA_API_KEY"
wire_api = "chat"

# export UWA_API_KEY=${key}
`,
    docs: 'https://github.com/openai/codex',
  },
  {
    id: 'claude-code',
    name: 'Claude Code',
    where: 'Shell env (once per session)',
    lang: 'bash',
    render: ({ base, model, key }) => `export ANTHROPIC_BASE_URL=${base}
export ANTHROPIC_API_KEY=${key}
export ANTHROPIC_MODEL=${model}

# Claude Code picks up ANTHROPIC_* automatically.`,
    docs: 'https://docs.anthropic.com/en/docs/claude-code',
  },
  {
    id: 'aider',
    name: 'Aider',
    where: 'CLI flags or ~/.aider.conf.yml',
    lang: 'bash',
    render: ({ base, model, key }) => `export OPENAI_API_BASE=${base}/v1
export OPENAI_API_KEY=${key}

aider --model openai/${model}`,
    docs: 'https://aider.chat/docs/llms/openai-compat.html',
  },
  {
    id: 'zcode',
    name: 'ZCode',
    where: 'Settings → Provider → OpenAI-compatible',
    lang: 'text',
    render: ({ base, model, key }) => `Base URL:   ${base}/v1
API key:    ${key}
Model:      ${model}`,
  },
  {
    id: 'autoclaw',
    name: 'AutoClaw',
    where: 'Custom provider JSON (per AutoClaw docs)',
    lang: 'json',
    render: ({ base, model, key }) => JSON.stringify({
      name: 'uwa',
      baseUrl: `${base}/v1`,
      apiKey: key,
      models: [{ id: model, name: `uwa · ${model}` }],
      capabilities: ['chat', 'tools'],
    }, null, 2),
  },
];
