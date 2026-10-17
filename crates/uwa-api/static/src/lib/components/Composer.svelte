<script lang="ts">
  import type { ModelObject, ResponseFormat } from '../types';

  let {
    models = [] as ModelObject[],
    model = '',
    sending = false,
    streamMode = true,
    disabled = false,
    onSend,
    onCancel,
    onModelChange,
    onStreamChange,
    onToolsChange,
    onResponseFormatChange,
  }: {
    models?: ModelObject[];
    model?: string;
    sending?: boolean;
    streamMode?: boolean;
    disabled?: boolean;
    onSend?: (text: string, toolsJson: string) => void;
    onCancel?: () => void;
    onModelChange?: (model: string) => void;
    onStreamChange?: (on: boolean) => void;
    onToolsChange?: (raw: string) => void;
    onResponseFormatChange?: (fmt: ResponseFormat | undefined) => void;
  } = $props();

  let text = $state('');
  let textareaRef: HTMLTextAreaElement | undefined;
  let toolsOpen = $state(false);
  let toolsRaw = $state('');
  let jsonMode = $state<'off' | 'object' | 'schema'>('off');
  let schemaText = $state('');
  let schemaName = $state('response');

  export function focus() {
    textareaRef?.focus();
  }
  export function setText(v: string) {
    text = v;
  }

  function submit() {
    const t = text.trim();
    if (!t || sending || disabled) return;
    onSend?.(t, toolsRaw.trim());
    text = '';
  }

  // Structured output: emit the chosen format to the parent on every
  // change, same push pattern as onToolsChange.
  function emitResponseFormat() {
    if (jsonMode === 'off') {
      onResponseFormatChange?.(undefined);
      return;
    }
    if (jsonMode === 'object') {
      onResponseFormatChange?.({ type: 'json_object' });
      return;
    }
    let parsed: unknown = null;
    try {
      parsed = schemaText.trim() ? JSON.parse(schemaText) : null;
    } catch (e) {
      // Don't push a broken schema — keep the last valid selection.
      return;
    }
    if (!parsed) {
      onResponseFormatChange?.({ type: 'json_object' });
      return;
    }
    onResponseFormatChange?.({
      type: 'json_schema',
      json_schema: { name: schemaName || 'response', schema: parsed, strict: true },
    });
  }

  function onKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault();
      submit();
    } else if (e.key === 'Escape' && sending) {
      e.preventDefault();
      onCancel?.();
    }
  }

  // Auto-grow textarea (re-run on every keystroke).
  $effect(() => {
    void text;
    if (!textareaRef) return;
    textareaRef.style.height = 'auto';
    textareaRef.style.height = `${Math.min(260, textareaRef.scrollHeight)}px`;
  });
</script>

<div class="composer">
  <div class="composer-row">
    <select
      class="composer-model"
      value={model}
      disabled={sending}
      onchange={(e) => onModelChange?.((e.currentTarget as HTMLSelectElement).value)}
    >
      {#if models.length === 0}
        <option value={model}>{model || '(no models)'}</option>
      {:else}
        {#each models as m (m.id)}
          <option value={m.id}>{m.id} · {m.owned_by}</option>
        {/each}
      {/if}
    </select>

    <label class="composer-toggle">
      <input
        type="checkbox"
        checked={streamMode}
        disabled={sending}
        onchange={(e) => onStreamChange?.((e.currentTarget as HTMLInputElement).checked)}
      />
      stream
    </label>

    <label class="composer-toggle">
      <input
        type="radio"
        name="jsonMode"
        checked={jsonMode === 'off'}
        disabled={sending}
        onchange={() => {
          jsonMode = 'off';
          emitResponseFormat();
        }}
      />
      no json
    </label>
    <label class="composer-toggle">
      <input
        type="radio"
        name="jsonMode"
        checked={jsonMode === 'object'}
        disabled={sending}
        onchange={() => {
          jsonMode = 'object';
          emitResponseFormat();
        }}
      />
      json
    </label>
    <label class="composer-toggle">
      <input
        type="radio"
        name="jsonMode"
        checked={jsonMode === 'schema'}
        disabled={sending}
        onchange={() => {
          jsonMode = 'schema';
          emitResponseFormat();
        }}
      />
      schema
    </label>

    <button class="ghost-btn" onclick={() => (toolsOpen = !toolsOpen)}>
      {toolsOpen ? 'hide tools' : 'tools'}
    </button>
  </div>

  {#if jsonMode === 'schema'}
    <div class="composer-tools">
      <span class="text-[11px] muted">Schema name</span>
      <input
        class="input"
        value={schemaName}
        disabled={sending}
        oninput={(e) => {
          schemaName = (e.currentTarget as HTMLInputElement).value;
          emitResponseFormat();
        }}
      />
      <span class="text-[11px] muted mt-2">Schema (JSON Schema)</span>
      <textarea
        class="input font-mono"
        rows={5}
        placeholder={`{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}`}
        bind:value={schemaText}
        oninput={() => emitResponseFormat()}
      ></textarea>
    </div>
  {/if}

  {#if toolsOpen}
    <div class="composer-tools">
      <label class="text-[11px] muted">Tools (OpenAI JSON array, or empty)</label>
      <textarea
        class="input font-mono"
        rows={5}
        placeholder={'[{"type":"function","function":{"name":"get_weather","description":"...","parameters":{"type":"object"}}}]'}
        bind:value={toolsRaw}
        oninput={() => onToolsChange?.(toolsRaw)}
      ></textarea>
    </div>
  {/if}

  <div class="composer-input">
    <textarea
      class="composer-textarea"
      bind:this={textareaRef}
      bind:value={text}
      onkeydown={onKeydown}
      placeholder="Type a message… (Ctrl/Cmd + Enter to send)"
      rows={2}
      disabled={disabled}
    ></textarea>
    <div class="composer-actions">
      {#if sending}
        <button class="btn-secondary" onclick={() => onCancel?.()}>Cancel</button>
      {:else}
        <button class="btn" disabled={!text.trim() || disabled} onclick={submit}>Send</button>
      {/if}
    </div>
  </div>
</div>
