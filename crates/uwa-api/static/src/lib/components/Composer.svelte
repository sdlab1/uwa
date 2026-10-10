<script lang="ts">
  import type { ModelObject } from '../types';

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
  } = $props();

  let text = $state('');
  let textareaRef: HTMLTextAreaElement | undefined;
  let toolsOpen = $state(false);
  let toolsRaw = $state('');

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

    <button class="ghost-btn" onclick={() => (toolsOpen = !toolsOpen)}>
      {toolsOpen ? 'hide tools' : 'tools'}
    </button>
  </div>

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
