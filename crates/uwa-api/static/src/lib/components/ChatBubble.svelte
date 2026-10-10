<script lang="ts">
  import type { ChatMessage } from '../types';
  import Markdown from './Markdown.svelte';
  import ToolCallChip from './ToolCallChip.svelte';
  import { toast } from '../stores';

  let {
    message,
    streaming = false,
  }: { message: ChatMessage; streaming?: boolean } = $props();

  let role = $derived(message.role);

  async function copyContent() {
    const text = message.content ?? '';
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      toast('Copied', 'ok');
    } catch {
      toast('Copy failed', 'err');
    }
  }
</script>

<div class="bubble bubble-{role}" class:streaming>
  <div class="bubble-head">
    <span class="bubble-role">{role}</span>
    {#if role === 'tool' && message.tool_call_id}
      <code class="bubble-role-id">{message.tool_call_id}</code>
    {/if}
    {#if message.content}
      <button class="ghost-btn" onclick={copyContent}>copy</button>
    {/if}
  </div>

  {#if role === 'tool'}
    <pre class="bubble-tool-content"><code>{message.content ?? ''}</code></pre>
  {:else if message.content}
    <Markdown source={message.content} />
    {#if streaming}<span class="caret"></span>{/if}
  {:else if !message.tool_calls?.length}
    {#if streaming}
      <div class="bubble-thinking"><span class="dots"></span> thinking…</div>
    {:else}
      <div class="muted">(empty)</div>
    {/if}
  {/if}

  {#if message.tool_calls?.length}
    <div class="tool-list">
      {#each message.tool_calls as tc (tc.id)}
        <ToolCallChip id={tc.id} name={tc.function.name} args={tc.function.arguments} />
      {/each}
    </div>
  {/if}
</div>
