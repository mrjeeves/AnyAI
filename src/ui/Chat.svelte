<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import ModeBar from "./ModeBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import type { HardwareProfile, Mode } from "../types";

  let {
    activeModel,
    activeMode,
    supportedModes,
    hardware,
    onModeChange,
    onProviderChange,
  } = $props<{
    activeModel: string;
    activeMode: Mode;
    supportedModes: Set<Mode>;
    hardware: HardwareProfile | null;
    onModeChange: (mode: Mode) => void;
    onProviderChange: () => void;
  }>();

  interface Message { role: "user" | "assistant"; content: string }

  let messages = $state<Message[]>([]);
  let input = $state("");
  let thinking = $state(false);
  let settingsTab = $state<"providers" | "models" | null>(null);
  let messagesEl: HTMLElement;

  $effect(() => {
    // Scroll to bottom when messages change
    if (messagesEl) {
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }
  });

  async function send() {
    const text = input.trim();
    if (!text || thinking) return;
    input = "";
    messages = [...messages, { role: "user", content: text }];
    thinking = true;

    try {
      // Going through the Rust-side ollama_chat command instead of
      // tauri-plugin-http: Ollama's CORS allowlist on Windows rejects
      // requests originating from the Tauri WebView (`http://tauri.localhost`)
      // with HTTP 403 even after the model is downloaded. reqwest from Rust
      // doesn't set Origin, so the daemon accepts the call.
      const content = await invoke<string>("ollama_chat", {
        model: activeModel,
        messages: messages.map((m) => ({ role: m.role, content: m.content })),
      });
      if (!content) {
        messages = [
          ...messages,
          { role: "assistant", content: "(empty response)" },
        ];
        return;
      }
      messages = [...messages, { role: "assistant", content }];
    } catch (e) {
      messages = [...messages, { role: "assistant", content: `(error: ${e})` }];
    } finally {
      thinking = false;
    }
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
    onOpenSettings={(tab) => (settingsTab = tab)}
  />

  <div class="messages" bind:this={messagesEl}>
    {#if messages.length === 0}
      <div class="empty">
        <span class="model-badge">{activeModel}</span>
        <p>Ready. Start typing below.</p>
      </div>
    {/if}
    {#each messages as msg (msg)}
      <div class="message {msg.role}">
        <div class="bubble">{msg.content}</div>
      </div>
    {/each}
    {#if thinking}
      <div class="message assistant">
        <div class="bubble thinking"><span></span><span></span><span></span></div>
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
    <button onclick={send} disabled={thinking || !input.trim()}>Send</button>
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
  .thinking { display: flex; gap: 4px; align-items: center; padding: .75rem; }
  .thinking span {
    width: 7px; height: 7px; border-radius: 50%; background: #444;
    animation: blink 1.2s infinite;
  }
  .thinking span:nth-child(2) { animation-delay: .2s; }
  .thinking span:nth-child(3) { animation-delay: .4s; }
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
</style>
