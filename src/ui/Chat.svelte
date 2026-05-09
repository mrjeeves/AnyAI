<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import ModeBar from "./ModeBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import type { HardwareProfile, Mode } from "../types";

  let {
    activeModel,
    activeMode,
    activeFamily,
    supportedModes,
    hardware,
    onModeChange,
    onProviderChange,
  } = $props<{
    activeModel: string;
    activeMode: Mode;
    activeFamily: string;
    supportedModes: Set<Mode>;
    hardware: HardwareProfile | null;
    onModeChange: (mode: Mode) => void;
    onProviderChange: () => void;
  }>();

  interface Message {
    role: "user" | "assistant";
    content: string;
    thinking?: string;
    streaming?: boolean;
  }

  // Per-stream payload from ollama_chat_stream (one of these fields is set per frame).
  interface StreamFrame {
    delta?: string;
    thinking_delta?: string;
    done?: boolean;
    cancelled?: boolean;
  }

  let messages = $state<Message[]>([]);
  let input = $state("");
  let streaming = $state(false);
  let activeStreamId = $state<string | null>(null);
  let settingsTab = $state<"providers" | "families" | "models" | "storage" | null>(null);
  let messagesEl: HTMLElement;

  $effect(() => {
    // Scroll to bottom when messages change
    if (messagesEl) {
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }
  });

  /** Append `delta` to either `content` or `thinking` on the assistant
   *  message at `idx`, returning a fresh array (so Svelte detects the change). */
  function appendTo(idx: number, field: "content" | "thinking", delta: string) {
    const next = messages.slice();
    const prev = next[idx];
    next[idx] = { ...prev, [field]: (prev[field] ?? "") + delta };
    messages = next;
  }

  function ensureAssistantBubble(): number {
    // Reuse the trailing assistant placeholder if there is one. Otherwise
    // append a fresh streaming bubble. Returns its index.
    const last = messages.length - 1;
    if (last >= 0 && messages[last].role === "assistant" && messages[last].streaming) {
      return last;
    }
    messages = [...messages, { role: "assistant", content: "", streaming: true }];
    return messages.length - 1;
  }

  async function send() {
    const text = input.trim();
    if (!text || streaming) return;
    input = "";
    const history = [...messages, { role: "user" as const, content: text }];
    messages = history;
    streaming = true;

    // Per-call channel so concurrent (or rapidly retried) streams can't
    // crosstalk. crypto.randomUUID is available in the Tauri WebView.
    const streamId = crypto.randomUUID();
    activeStreamId = streamId;
    let unlisten: UnlistenFn | null = null;
    let assistantIdx = -1;
    try {
      unlisten = await listen<StreamFrame>(
        `anyai://chat-stream/${streamId}`,
        (e) => {
          const frame = e.payload;
          if (frame.thinking_delta) {
            if (assistantIdx === -1) assistantIdx = ensureAssistantBubble();
            appendTo(assistantIdx, "thinking", frame.thinking_delta);
          } else if (frame.delta) {
            if (assistantIdx === -1) assistantIdx = ensureAssistantBubble();
            appendTo(assistantIdx, "content", frame.delta);
          }
          // `done` is handled in the finally block (which also fires on cancel
          // and on the invoke promise rejecting), so we don't need to act here.
        },
      );

      // Going through the Rust-side ollama_chat_stream command instead of
      // tauri-plugin-http: Ollama's CORS allowlist on Windows rejects
      // requests originating from the Tauri WebView (`http://tauri.localhost`)
      // with HTTP 403 even after the model is downloaded. reqwest from Rust
      // doesn't set Origin, so the daemon accepts the call.
      await invoke("ollama_chat_stream", {
        streamId,
        model: activeModel,
        messages: history.map((m) => ({ role: m.role, content: m.content })),
      });

      if (assistantIdx === -1) {
        messages = [...messages, { role: "assistant", content: "(empty response)" }];
      }
    } catch (e) {
      messages = [...messages, { role: "assistant", content: `(error: ${e})` }];
    } finally {
      streaming = false;
      activeStreamId = null;
      // Drop the streaming flag on the last assistant bubble so its
      // <details> can collapse cleanly once the answer is in.
      if (assistantIdx !== -1) {
        const next = messages.slice();
        const prev = next[assistantIdx];
        next[assistantIdx] = { ...prev, streaming: false };
        messages = next;
      }
      unlisten?.();
    }
  }

  async function stop() {
    if (!activeStreamId) return;
    // Fire-and-forget: the cancel command itself is fast, and the in-flight
    // invoke() in send() will resolve naturally once the Rust side observes
    // the notify and unwinds.
    try {
      await invoke("ollama_chat_cancel", { streamId: activeStreamId });
    } catch {}
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  async function handleModeChange(mode: Mode) {
    messages = []; // Clear history on mode switch
    await onModeChange(mode);
  }

  async function handleProviderChange() {
    messages = [];
    settingsTab = null;
    await onProviderChange();
  }
</script>

<div class="chat-shell">
  <StatusBar
    model={activeModel}
    mode={activeMode}
    family={activeFamily}
    onOpenSettings={(tab) => (settingsTab = tab)}
  />

  <div class="messages" bind:this={messagesEl}>
    {#if messages.length === 0}
      <div class="empty">
        <span class="model-badge">{activeModel}</span>
        <p>Ready. Start typing below.</p>
      </div>
    {/if}
    {#each messages as msg, i (i)}
      <div class="message {msg.role}">
        <div class="bubble">
          {#if msg.thinking}
            <details class="thinking-block" open={msg.streaming}>
              <summary>{msg.streaming && !msg.content ? "Thinking…" : "Thoughts"}</summary>
              <div class="thinking-content">{msg.thinking}</div>
            </details>
          {/if}
          {#if msg.content}
            <span class="content">{msg.content}</span>
          {:else if msg.streaming && !msg.thinking}
            <span class="dots"><span></span><span></span><span></span></span>
          {/if}
        </div>
      </div>
    {/each}
    {#if streaming && (messages.length === 0 || messages[messages.length - 1].role !== "assistant")}
      <div class="message assistant">
        <div class="bubble"><span class="dots"><span></span><span></span><span></span></span></div>
      </div>
    {/if}
  </div>

  <ModeBar current={activeMode} supported={supportedModes} onChange={handleModeChange} />

  <div class="input-row">
    <textarea
      bind:value={input}
      onkeydown={onKeydown}
      placeholder="Message…"
      rows="1"
    ></textarea>
    {#if streaming}
      <button class="stop" onclick={stop} title="Stop generating">Stop</button>
    {:else}
      <button onclick={send} disabled={!input.trim()}>Send</button>
    {/if}
  </div>

  {#if settingsTab}
    <SettingsPanel
      initialTab={settingsTab}
      onClose={() => (settingsTab = null)}
      onChanged={handleProviderChange}
    />
  {/if}
</div>

<style>
  .chat-shell {
    height: 100vh;
    display: flex;
    flex-direction: column;
    position: relative;
  }
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: .75rem;
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: .5rem;
    color: #555;
    font-size: .9rem;
  }
  .model-badge {
    background: #1a1a1a;
    padding: .25rem .65rem;
    border-radius: 20px;
    font-size: .75rem;
    font-family: monospace;
    color: #6e6ef7;
  }
  .message { display: flex; }
  .message.user { justify-content: flex-end; }
  .bubble {
    max-width: 72%;
    padding: .6rem .85rem;
    border-radius: 14px;
    font-size: .9rem;
    line-height: 1.5;
    white-space: pre-wrap;
  }
  .user .bubble { background: #6e6ef7; color: #fff; border-bottom-right-radius: 4px; }
  .assistant .bubble { background: #1e1e1e; color: #e8e8e8; border-bottom-left-radius: 4px; }
  .thinking-block {
    margin-bottom: .5rem;
    border-left: 2px solid #444;
    padding-left: .6rem;
  }
  .thinking-block summary {
    cursor: pointer;
    color: #888;
    font-size: .75rem;
    font-style: italic;
    user-select: none;
    list-style: none;
  }
  .thinking-block summary::-webkit-details-marker { display: none; }
  .thinking-block summary::before {
    content: "▸ ";
    display: inline-block;
    width: .8em;
  }
  .thinking-block[open] summary::before { content: "▾ "; }
  .thinking-content {
    margin-top: .35rem;
    color: #888;
    font-size: .8rem;
    font-style: italic;
    white-space: pre-wrap;
  }
  .dots { display: inline-flex; gap: 4px; align-items: center; }
  .dots span {
    width: 7px; height: 7px; border-radius: 50%; background: #444;
    animation: blink 1.2s infinite;
  }
  .dots span:nth-child(2) { animation-delay: .2s; }
  .dots span:nth-child(3) { animation-delay: .4s; }
  @keyframes blink { 0%,80%,100% { opacity: .3; } 40% { opacity: 1; } }
  .input-row {
    display: flex;
    gap: .5rem;
    padding: .75rem;
    border-top: 1px solid #1e1e1e;
    background: #0f0f0f;
  }
  textarea {
    flex: 1;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 8px;
    color: #e8e8e8;
    padding: .6rem .75rem;
    font-size: .9rem;
    font-family: inherit;
    resize: none;
    min-height: 38px;
    max-height: 140px;
    overflow-y: auto;
  }
  textarea:focus { outline: none; border-color: #6e6ef7; }
  button {
    padding: 0 1rem;
    background: #6e6ef7;
    color: #fff;
    border: none;
    border-radius: 8px;
    cursor: pointer;
    font-size: .875rem;
    font-weight: 500;
  }
  button:hover:not(:disabled) { background: #5a5ae0; }
  button:disabled { opacity: .4; cursor: default; }
  button.stop { background: #b04444; }
  button.stop:hover { background: #c25050; }
</style>
