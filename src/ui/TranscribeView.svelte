<script lang="ts">
  import { onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import ModeBar from "./ModeBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import { updateUi, type SettingsTab } from "../update-state.svelte";
  import { loadConfig } from "../config";
  import {
    loadConversation,
    saveConversation,
    newConversation,
    type Conversation,
  } from "../conversations";
  import type { HardwareProfile, Mode } from "../types";

  let {
    activeModel,
    activeMode,
    activeFamily,
    supportedModes,
    sidebarOpen,
    conversationId,
    newChatCounter,
    onToggleSidebar,
    onModeChange,
    onProviderChange,
    onConversationChanged,
    onNewSession,
  } = $props<{
    activeModel: string;
    activeMode: Mode;
    activeFamily: string;
    supportedModes: Set<Mode>;
    hardware: HardwareProfile | null;
    sidebarOpen: boolean;
    conversationId: string | null;
    newChatCounter: number;
    onToggleSidebar: () => void;
    onModeChange: (mode: Mode) => void;
    onProviderChange: () => void;
    onConversationChanged: (id: string) => void;
    /** Create a fresh, untitled session. App owns the active-id pointer so
     *  the sidebar list updates and the bottom-bar "+ New" mirrors the
     *  sidebar's own "New session" button. */
    onNewSession: () => void;
  }>();

  interface TranscribeFrame {
    delta: string;
    elapsed_ms: number;
    final: boolean;
  }
  interface WhisperModelInfo {
    name: string;
    approx_size_bytes: number;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  let activeConversation = $state<Conversation | null>(null);
  let transcript = $state("");
  let talkingPoints = $state<string[]>([]);
  /** Title input — bound directly to the bottom-bar field. Persisted on
   *  Enter, blur, and at record-start so a recording always lives on disk
   *  under whatever the user typed. */
  let sessionName = $state("");
  let settingsTab = $state<SettingsTab | null>(null);

  // Open the SettingsPanel on a specific tab when App requests it (e.g.
  // user clicked "yes" on the startup update prompt). Mirrors Chat.svelte —
  // see the comment there for why a nonce is needed.
  let lastOpenSettingsNonce = -1;
  $effect(() => {
    const req = updateUi.openSettingsRequest;
    if (!req) return;
    if (req.nonce === lastOpenSettingsNonce) return;
    lastOpenSettingsNonce = req.nonce;
    settingsTab = req.tab;
  });

  // Recording state. Capture + ASR run on the Rust side via cpal +
  // whisper-rs; we drive it via Tauri commands and listen for delta
  // frames on a per-stream event channel.
  let recording = $state(false);
  let recordingStartedAt = $state(0);
  let elapsed = $state(0);
  let transcribeError = $state("");
  /** Active stream id while a recording is going. Per-recording UUID so a
   *  stale event from a previous session can't cross-contaminate the new
   *  one if the user stops + starts quickly. */
  let activeStreamId: string | null = null;
  let unlistenStream: UnlistenFn | null = null;
  let elapsedTimer: ReturnType<typeof setInterval> | null = null;

  // Load (or clear) a session whenever the active id changes.
  $effect(() => {
    const id = conversationId;
    if (!id) {
      activeConversation = null;
      transcript = "";
      talkingPoints = [];
      sessionName = "";
      return;
    }
    let cancelled = false;
    loadConversation(id).then((c) => {
      if (cancelled || !c) return;
      activeConversation = c;
      transcript = c.transcript ?? "";
      talkingPoints = c.talking_points ?? [];
      sessionName = c.title === "New chat" ? "" : c.title;
    });
    return () => {
      cancelled = true;
    };
  });

  // Reset on "+ New" presses. Same skip-first-tick trick as Chat.svelte.
  let _seenInitial = false;
  $effect(() => {
    void newChatCounter;
    if (!_seenInitial) {
      _seenInitial = true;
      return;
    }
    activeConversation = null;
    transcript = "";
    talkingPoints = [];
    sessionName = "";
    if (recording) stopRecording();
  });

  onDestroy(() => stopRecording());

  async function persist(opts: { force?: boolean } = {}): Promise<Conversation | null> {
    const hasContent = sessionName.trim() || transcript.trim() || talkingPoints.length > 0;
    if (!opts.force && !activeConversation && !hasContent) return null;
    let conv = activeConversation;
    if (!conv) {
      conv = newConversation(activeMode, activeModel, activeFamily);
    } else {
      conv.model = activeModel;
      conv.family = activeFamily;
      conv.mode = activeMode;
    }
    const trimmed = sessionName.trim();
    if (trimmed) conv.title = trimmed.slice(0, 80);
    conv.transcript = transcript;
    conv.talking_points = talkingPoints;
    conv.messages = [];
    await saveConversation(conv);
    activeConversation = conv;
    onConversationChanged(conv.id);
    return conv;
  }

  function onNameKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      persist().catch((err) => console.warn("save title failed:", err));
      (e.currentTarget as HTMLInputElement).blur();
    }
  }

  function onNameBlur() {
    persist().catch((e) => console.warn("save title failed:", e));
  }

  /** Pre-flight: confirm the configured whisper model is downloaded. If
   *  not, surface a clear "go to Settings → Transcription" call to action
   *  rather than letting the Rust side throw a model-not-found error. */
  async function modelInstalled(name: string): Promise<boolean> {
    try {
      const all = await invoke<WhisperModelInfo[]>("whisper_models_list");
      return all.find((m) => m.name === name)?.installed ?? false;
    } catch {
      return false;
    }
  }

  async function startRecording() {
    if (recording) return;
    transcribeError = "";
    const cfg = await loadConfig();
    const mic = cfg.mic;
    const model = mic.whisper_model || "tiny.en";

    if (!(await modelInstalled(model))) {
      transcribeError =
        `The whisper '${model}' model isn't installed yet. Open Settings → ` +
        `Transcription to download it.`;
      return;
    }

    const streamId = crypto.randomUUID();
    activeStreamId = streamId;

    try {
      // Listen first so we can't miss a fast-arriving frame between the
      // invoke returning and the listener attaching.
      unlistenStream = await listen<TranscribeFrame>(
        `anyai://transcribe-stream/${streamId}`,
        (e) => {
          const f = e.payload;
          if (f.delta) {
            transcript = transcript + f.delta;
          }
          if (f.final) {
            // Worker unwound on its own (error path or natural end). Make
            // sure our state stays in sync.
            if (recording) stopRecording();
          }
        },
      );

      await invoke("transcribe_start", {
        streamId,
        model,
        device: mic.device_name || null,
      });

      recording = true;
      recordingStartedAt = Date.now();
      elapsed = 0;
      elapsedTimer = setInterval(() => {
        elapsed = Math.floor((Date.now() - recordingStartedAt) / 1000);
      }, 250);

      // Snapshot the session name so a recording always lives on disk
      // under whatever the user typed when they hit record.
      await persist({ force: true });
    } catch (e) {
      transcribeError = String(e);
      // Tear down any partial state so a retry has a clean slate.
      unlistenStream?.();
      unlistenStream = null;
      activeStreamId = null;
    }
  }

  async function stopRecording() {
    const id = activeStreamId;
    recording = false;
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = null;
    activeStreamId = null;
    if (id) {
      try {
        await invoke("transcribe_stop", { streamId: id });
      } catch (e) {
        console.warn("transcribe_stop failed:", e);
      }
    }
    unlistenStream?.();
    unlistenStream = null;
    // Persist the final transcript text.
    persist().catch((e) => console.warn("save after stop failed:", e));
  }

  async function handleModeChange(mode: Mode) {
    if (recording) await stopRecording();
    await onModeChange(mode);
  }

  async function handleProviderChange() {
    settingsTab = null;
    await onProviderChange();
  }

  function fmtElapsed(sec: number): string {
    const m = Math.floor(sec / 60).toString().padStart(2, "0");
    const s = (sec % 60).toString().padStart(2, "0");
    return `${m}:${s}`;
  }
</script>

<div class="transcribe-shell">
  <StatusBar
    model={activeModel}
    mode={activeMode}
    family={activeFamily}
    {sidebarOpen}
    {onToggleSidebar}
    onOpenSettings={(tab) => (settingsTab = tab)}
  />

  <div class="split">
    <section class="pane left" aria-label="Live transcription">
      <header class="pane-head">
        <span class="pane-title">Transcription</span>
        {#if recording}
          <span class="rec-dot" aria-hidden="true"></span>
          <span class="rec-time">{fmtElapsed(elapsed)}</span>
        {/if}
      </header>
      <div class="pane-body">
        {#if transcript}
          <pre class="transcript">{transcript}</pre>
        {:else}
          <div class="placeholder">
            {#if recording}
              Listening… transcription will stream in here every few
              seconds.
            {:else}
              Press <strong>Record</strong> to start a session. The live
              transcript will appear in this pane.
            {/if}
          </div>
        {/if}
      </div>
    </section>

    <section class="pane right" aria-label="Talking points">
      <header class="pane-head">
        <span class="pane-title">Talking points</span>
      </header>
      <div class="pane-body">
        {#if talkingPoints.length > 0}
          <ul class="bullets">
            {#each talkingPoints as point, i (i)}
              <li>{point}</li>
            {/each}
          </ul>
        {:else}
          <div class="placeholder">
            Talking points will be summarised here once a session is running.
          </div>
        {/if}
      </div>
    </section>
  </div>

  <ModeBar
    current={activeMode}
    supported={supportedModes}
    tokensUsed={0}
    contextSize={0}
    onChange={handleModeChange}
  />

  {#if transcribeError}
    <div class="mic-error">{transcribeError}</div>
  {/if}

  <div class="input-row">
    <button class="new-btn" onclick={onNewSession} title="New session">
      <span class="plus" aria-hidden="true">+</span> New
    </button>
    <label class="name-field">
      <span class="name-label">Session Name:</span>
      <input
        type="text"
        bind:value={sessionName}
        onkeydown={onNameKeydown}
        onblur={onNameBlur}
        placeholder="Untitled session"
        spellcheck="false"
      />
    </label>
    {#if recording}
      <button class="record-btn stop" onclick={stopRecording} title="Stop recording">
        <span class="rec-square" aria-hidden="true"></span>
        Stop
      </button>
    {:else}
      <button class="record-btn" onclick={startRecording} title="Start recording">
        <span class="rec-circle" aria-hidden="true"></span>
        Record
      </button>
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
  .transcribe-shell {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    position: relative;
  }
  .split {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .pane {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .pane.left { border-right: 1px solid #1a1a1a; }
  .pane-head {
    display: flex;
    align-items: center;
    gap: .5rem;
    padding: .5rem .85rem;
    border-bottom: 1px solid #161616;
    background: #0d0d0d;
  }
  .pane-title {
    font-size: .78rem;
    color: #aaa;
    text-transform: uppercase;
    letter-spacing: .06em;
    font-weight: 600;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #e35a5a;
    box-shadow: 0 0 8px #e35a5a;
    animation: rec-pulse 1.4s ease-in-out infinite;
  }
  @keyframes rec-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: .35; }
  }
  .rec-time {
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: .76rem;
    color: #e35a5a;
  }
  .pane-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 1rem 1.15rem;
  }
  .transcript {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    font-size: .9rem;
    line-height: 1.6;
    color: #e8e8e8;
    white-space: pre-wrap;
    margin: 0;
  }
  .bullets {
    list-style: disc;
    padding-left: 1.25rem;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: .4rem;
  }
  .bullets li {
    font-size: .88rem;
    color: #ddd;
    line-height: 1.5;
  }
  .placeholder {
    color: #666;
    font-size: .85rem;
    line-height: 1.6;
    max-width: 38ch;
  }
  .placeholder strong { color: #aaa; font-weight: 600; }

  .mic-error {
    background: #3a1717;
    color: #ffb4b4;
    border-top: 1px solid #5a2424;
    padding: .4rem .85rem;
    font-size: .78rem;
  }

  .input-row {
    display: flex;
    align-items: center;
    gap: .55rem;
    padding: .65rem .75rem;
    border-top: 1px solid #1e1e1e;
    background: #0f0f0f;
  }
  .new-btn {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: .3rem;
    background: none;
    border: 1px solid #2a2a2a;
    color: #ccc;
    padding: .45rem .75rem;
    border-radius: 8px;
    font-size: .82rem;
    cursor: pointer;
    transition: border-color .12s, background .12s, color .12s;
  }
  .new-btn:hover { border-color: #3a3a55; background: #131320; color: #fff; }
  .new-btn .plus { font-size: 1rem; line-height: 1; color: #6e6ef7; }

  .name-field {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: .55rem;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 8px;
    padding: 0 .75rem;
    transition: border-color .12s;
  }
  .name-field:focus-within { border-color: #6e6ef7; }
  .name-label {
    flex-shrink: 0;
    font-size: .8rem;
    color: #888;
    user-select: none;
  }
  .name-field input {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    color: #e8e8e8;
    font-size: .9rem;
    font-family: inherit;
    padding: .55rem 0;
  }
  .name-field input:focus { outline: none; }

  .record-btn {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: .45rem;
    background: #6e6ef7;
    color: #fff;
    border: none;
    border-radius: 8px;
    padding: .5rem .9rem;
    font-size: .85rem;
    font-weight: 500;
    cursor: pointer;
    transition: background .12s;
  }
  .record-btn:hover:not(:disabled) { background: #5a5ae0; }
  .record-btn:disabled { opacity: .5; cursor: not-allowed; }
  .record-btn.stop { background: #b04444; }
  .record-btn.stop:hover { background: #c25050; }

  .rec-circle {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 0 0 2px rgba(255, 255, 255, .25);
  }
  .rec-square {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    background: #fff;
  }

  @media (max-width: 700px) {
    .split { flex-direction: column; }
    .pane.left { border-right: none; border-bottom: 1px solid #1a1a1a; }
    .input-row { flex-wrap: wrap; }
    .name-field { order: 3; flex-basis: 100%; }
  }
</style>
