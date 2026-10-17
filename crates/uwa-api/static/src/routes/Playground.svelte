<script lang="ts">
  import { onMount } from 'svelte';
  import type { ChatMessage, ModelObject, ToolCallRef } from '../lib/types';
  import { chat, chatStream, getModels, getProfiles, ApiError } from '../lib/api';
  import type { ProfileSummary } from '../lib/types';
  import {
    toast, streamStart, streamFirstToken, streamDelta, streamDone, streamError,
  } from '../lib/stores';
  import {
    loadSessions, saveSessions, newSession, deriveTitle, type ChatSession,
  } from '../lib/chatStore';
  import ChatBubble from '../lib/components/ChatBubble.svelte';
  import Composer from '../lib/components/Composer.svelte';
  import StreamBadge, { type Phase } from '../lib/components/StreamBadge.svelte';

  // ---- Session state ----
  let sessions: ChatSession[] = $state(loadSessions());
  let activeId: string = $state(sessions[0]?.id ?? '');
  let active: ChatSession | undefined = $derived(sessions.find((s) => s.id === activeId));

  // ---- Models ----
  let models: ModelObject[] = $state([]);
  let profiles: ProfileSummary[] = $state([]);
  let profile: string = $state('');
  let model: string = $state(active?.model ?? '');
  let streamMode: boolean = $state(true);
  let toolsRaw: string = $state('');

  // ---- Streaming state ----
  type LiveTool = { id?: string; name?: string; args: string; done: boolean };
  let phase: Phase = $state('idle');
  let streamText: string = $state('');
  let streamToolCalls: Record<number, LiveTool> = $state({});
  let startedAt = $state<number | null>(null);
  let firstTokenAt = $state<number | null>(null);
  let finishedAt = $state<number | null>(null);
  let abort: AbortController | null = null;
  let charsStreamed = $state(0);

  let ttftMs = $derived(firstTokenAt != null && startedAt != null ? firstTokenAt - startedAt : undefined);
  let totalMs = $derived(finishedAt != null && startedAt != null ? finishedAt - startedAt : undefined);

  // ---- DOM ----
  let scrollRef: HTMLDivElement | undefined;
  let composerRef: Composer | undefined;
  let responseFormat: import('../lib/types').ResponseFormat | undefined = $state(undefined);
  let pinned = $state(true);

  // Auto-scroll when the user is already at the bottom.
  $effect(() => {
    // depend on: messages count, stream text, phase
    void active?.messages.length;
    void streamText.length;
    void phase;
    if (!scrollRef || !pinned) return;
    requestAnimationFrame(() => {
      if (scrollRef) scrollRef.scrollTop = scrollRef.scrollHeight;
    });
  });

  function onScroll() {
    if (!scrollRef) return;
    const gap = scrollRef.scrollHeight - scrollRef.scrollTop - scrollRef.clientHeight;
    pinned = gap < 60;
  }

  // Persist on any change (debounced).
  let saveTimer: number | undefined;
  function scheduleSave() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => {
      saveSessions($state.snapshot(sessions));
    }, 250);
  }

  // ---- Actions ----
  function createNew() {
    const s = newSession(model || models[0]?.id || 'gpt-4o');
    sessions = [s, ...sessions];
    activeId = s.id;
    scheduleSave();
    queueMicrotask(() => composerRef?.focus());
  }

  function selectSession(id: string) {
    activeId = id;
    const s = sessions.find((x) => x.id === id);
    if (s) model = s.model;
  }

  function deleteSession(id: string) {
    sessions = sessions.filter((s) => s.id !== id);
    if (activeId === id) activeId = sessions[0]?.id ?? '';
    scheduleSave();
  }

  function ensureActive(): ChatSession {
    let s = active;
    if (!s) {
      const created = newSession(model || models[0]?.id || 'gpt-4o');
      sessions = [created, ...sessions];
      activeId = created.id;
      // Re-read through the $state proxy: `created` is a plain object and
      // mutations through it would not trigger reactive updates.
      s = sessions[0]!;
    }
    return s;
  }

  function parseTools(raw: string): unknown[] | undefined {
    const t = raw.trim();
    if (!t) return undefined;
    try {
      const v = JSON.parse(t);
      if (Array.isArray(v)) return v;
      toast('Tools must be a JSON array', 'warn');
      return undefined;
    } catch (e) {
      toast(`Tools JSON invalid: ${String(e)}`, 'warn');
      return undefined;
    }
  }

  function resetStreamState() {
    streamText = '';
    streamToolCalls = {};
    charsStreamed = 0;
    firstTokenAt = null;
    finishedAt = null;
    startedAt = performance.now();
    streamStart();
  }

  async function send(text: string, toolsJson: string) {
    const s = ensureActive();
    const userMsg: ChatMessage = { role: 'user', content: text };
    const placeholder: ChatMessage = { role: 'assistant', content: '' };
    s.messages.push(userMsg, placeholder);
    s.model = model || s.model;
    if (s.messages.filter((m) => m.role === 'user').length === 1) {
      s.title = deriveTitle(text);
    }
    s.updatedAt = Date.now();
    scheduleSave();

    const assistantIndex = s.messages.length - 1;

    resetStreamState();
    phase = streamMode ? 'connecting' : 'waiting';

    const reqMessages = s.messages.slice(0, -1); // exclude the placeholder
    const tools = parseTools(toolsJson);
    const req = {
      model: s.model,
      messages: reqMessages,
      tools,
      ...(responseFormat ? { response_format: responseFormat } : {}),
    };

    try {
      if (streamMode) {
        abort = new AbortController();
        phase = 'waiting';
        const acc = await chatStream(req, {
          signal: abort.signal,
          profile: profile || undefined,
          onFirstToken: () => {
            firstTokenAt = performance.now();
            phase = 'first-token';
            streamFirstToken();
            queueMicrotask(() => (phase = 'streaming'));
          },
          onDelta: (chunk) => {
            streamText += chunk;
            charsStreamed += chunk.length;
            streamDelta(chunk.length);
            s.messages[assistantIndex]!.content = streamText;
            s.updatedAt = Date.now();
            scheduleSave();
          },
          onToolCall: (tc) => {
            const slot = streamToolCalls[tc.index] ?? { args: '', done: false };
            if (tc.id) slot.id = tc.id;
            if (tc.name) slot.name = tc.name;
            if (tc.arguments) slot.args += tc.arguments;
            streamToolCalls[tc.index] = slot;
            // Mirror into the message
            const list: ToolCallRef[] = Object.entries(streamToolCalls).map(([, v]) => ({
              id: v.id ?? '',
              type: 'function',
              function: { name: v.name ?? '', arguments: v.args },
            }));
            s.messages[assistantIndex]!.tool_calls = list.length ? list : undefined;
          },
        });
        // Ensure final text landed (in case onDelta missed a trailing chunk)
        s.messages[assistantIndex]!.content = acc || streamText;
        for (const k of Object.keys(streamToolCalls)) streamToolCalls[+k]!.done = true;
      } else {
        phase = 'waiting';
        const r = await chat(req, { profile: profile || undefined });
        const m = r.choices[0]?.message;
        if (m) {
          s.messages[assistantIndex]!.content = m.content ?? '';
          if (m.tool_calls?.length) s.messages[assistantIndex]!.tool_calls = m.tool_calls;
          charsStreamed = (m.content ?? '').length;
        }
        firstTokenAt = performance.now();
      }
      finishedAt = performance.now();
      phase = 'done';
      streamDone();
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        phase = 'idle';
        toast('Cancelled', 'warn');
      } else if (e instanceof ApiError) {
        phase = 'error';
        streamError(`${e.status} ${e.code}: ${e.message}`);
        s.messages[assistantIndex]!.content = `⚠️ ${e.status} ${e.code}: ${e.message}`;
        toast(e.message, 'err');
      } else {
        phase = 'error';
        streamError(String(e));
        s.messages[assistantIndex]!.content = `⚠️ ${String(e)}`;
        toast(String(e), 'err');
      }
    } finally {
      abort = null;
      s.updatedAt = Date.now();
      // Save synchronously: a page reload right after the turn must not
      // lose it to the 250ms debounce.
      saveSessions($state.snapshot(sessions));
      pinned = true;
    }
  }

  function cancel() {
    abort?.abort();
  }

  function clearActive() {
    const s = active;
    if (!s) return;
    if (!confirm('Clear this conversation?')) return;
    s.messages = [];
    s.title = 'New chat';
    s.updatedAt = Date.now();
    scheduleSave();
  }

  onMount(async () => {
    try {
      const pr = await getProfiles();
      profiles = pr.profiles;
      profile = profiles[0]?.name ?? '';
    } catch {
      profiles = [];
    }
    try {
      const list = await getModels();
      models = list.data;
      if (!model && models[0]) model = models[0].id;
    } catch (e) {
      toast(`Models load failed: ${String(e)}`, 'err');
    }
    if (!activeId && sessions[0]) activeId = sessions[0].id;
    queueMicrotask(() => composerRef?.focus());
  });

  let sending = $derived(phase !== 'idle' && phase !== 'done' && phase !== 'error');
</script>

<div class="playground">
  <!-- Sidebar -->
  <aside class="chat-sidebar">
    <button class="btn w-full mb-2" onclick={createNew}>+ New chat</button>
    {#if sessions.length === 0}
      <p class="muted text-[12px]">No conversations yet.</p>
    {:else}
      <ul class="chat-list">
        {#each sessions as s (s.id)}
          <li class="chat-item" class:active={s.id === activeId}>
            <button class="chat-item-btn" onclick={() => selectSession(s.id)}>
              <div class="chat-item-title">{s.title}</div>
              <div class="chat-item-sub">{s.model} · {s.messages.length} msg</div>
            </button>
            <button class="chat-item-del" title="Delete" onclick={() => deleteSession(s.id)}>×</button>
          </li>
        {/each}
      </ul>
    {/if}
  </aside>

  <!-- Main -->
  <section class="chat-main">
    <div class="chat-head">
      <h2 class="m-0">{active?.title ?? 'New chat'}</h2>
      <div class="chat-head-actions">
        {#if profiles.length > 1}
          <label class="text-[11px] muted" for="pg-profile">profile</label>
          <select id="pg-profile" class="input max-w-[140px]" bind:value={profile}>
            {#each profiles as p (p.name)}
              <option value={p.name}>{p.label}</option>
            {/each}
          </select>
        {/if}
        <StreamBadge {phase} {ttftMs} {totalMs} chars={charsStreamed} />
        <button class="ghost-btn" onclick={clearActive}>clear</button>
      </div>
    </div>

    <div class="chat-scroll" bind:this={scrollRef} onscroll={onScroll}>
      {#if !active || active.messages.length === 0}
        <div class="chat-empty">
          <p class="lead">Send a message through uwa.</p>
          <p class="muted text-[12px]">
            Toggle <b>stream</b> for token-by-token output with live TTFT.
            Add tools with the <b>tools</b> button to test the bridge.
          </p>
        </div>
      {:else}
        {#each active.messages as m, i (i)}
          <ChatBubble
            message={m}
            streaming={sending && i === active.messages.length - 1 && m.role === 'assistant'}
          />
        {/each}
      {/if}
    </div>

    <Composer
      bind:this={composerRef}
      {models}
      {model}
      {sending}
      {streamMode}
      onSend={send}
      onCancel={cancel}
      onModelChange={(m) => { model = m; if (active) active.model = m; scheduleSave(); }}
      onStreamChange={(on) => (streamMode = on)}
      onToolsChange={(raw) => (toolsRaw = raw)}
      onResponseFormatChange={(f) => (responseFormat = f)}
    />
  </section>
</div>
