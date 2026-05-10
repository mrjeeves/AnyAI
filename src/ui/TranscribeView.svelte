<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import ModeBar from "./ModeBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import type { SettingsTab } from "../update-state.svelte";
  import {
    transcribeUi,
    startRecording,
    stopRecording,
    clearLiveDelta,
    clearAfterPersist,
  } from "./transcribe-state.svelte";
  import { chatSlot } from "./chat-slot.svelte";
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
    onRequestStopTranscribe,
    onRequestStopChat,
    onRequestStartRecording,
    onRequestActivateTalkingPoints,
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
    onNewSession: () => void;
    onRequestStopTranscribe: () => void;
    /** Stop the chat-slot occupant (chat or Talking Points). Routed to App
     *  so the conflict-modal flow lives in one place. */
    onRequestStopChat: () => void;
    /** Ask App to start a recording — App handles the singleton check
     *  against any other in-flight session and shows a conflict modal. */
    onRequestStartRecording: (start: () => Promise<void>) => void;
    /** Ask App to activate Talking Points — App owns the singleton check
     *  against the chat slot and forwards to the chat-slot store. */
    onRequestActivateTalkingPoints: () => void;
  }>();

  interface WhisperModelInfo {
    name: string;
    approx_size_bytes: number;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  let activeConversation = $state<Conversation | null>(null);
  let transcript = $state("");
  let talkingPoints = $state<string[]>([]);
  let sessionName = $state("");
  let settingsTab = $state<SettingsTab | null>(null);
  let transcribeError = $state("");

  /** Debounced live-transcript flush. Without this, the on-disk transcript
   *  only updates on start/stop/title-edit, so anything reading the
   *  conversation file during a session (notably the Talking Points loop in
   *  chat-slot.svelte.ts) sees a stale snapshot and never re-summarises. */
  let liveSaveTimer: ReturnType<typeof setTimeout> | null = null;
  function scheduleLiveSave() {
    if (liveSaveTimer) return;
    liveSaveTimer = setTimeout(() => {
      liveSaveTimer = null;
      persist().catch((e) => console.warn("live transcript save failed:", e));
    }, 1500);
  }

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
    if (transcribeUi.active && transcribeUi.conversationId === conversationId) {
      // Cancelling the active recording on "+ New" matches the previous
      // single-view behaviour. The store's stopRecording awaits the
      // final frame before resolving so the live delta fully lands first.
      stopRecording().then(() => {
        flushLiveDelta();
        clearAfterPersist();
      });
    }
  });

  // Watch the global store: when a frame arrives for our conversation,
  // append the delta to our visible transcript and clear it from the
  // store so we don't double-append. Untrack on `liveDelta` itself —
  // we drive off `framePulse` to avoid resubscription churn.
  $effect(() => {
    void transcribeUi.framePulse;
    untrack(flushLiveDelta);
  });

  function flushLiveDelta() {
    const myConv = transcribeUi.conversationId;
    if (!transcribeUi.liveDelta) return;
    if (myConv && myConv !== conversationId) return; // belongs to another conv
    transcript = transcript + transcribeUi.liveDelta;
    clearLiveDelta();
    scheduleLiveSave();
  }

  onDestroy(() => {
    // Don't tear down the recording when this view unmounts — that's
    // the whole point of lifting state into the store. Just flush any
    // text we haven't rendered yet so it lives on the conversation
    // instead of staying buffered in the store.
    if (liveSaveTimer) {
      clearTimeout(liveSaveTimer);
      liveSaveTimer = null;
    }
    flushLiveDelta();
    if (transcribeUi.active && transcribeUi.conversationId === conversationId) {
      // Best-effort save of the current transcript so a crash later
      // doesn't lose the in-flight text. The active recording keeps
      // appending to the conversation file via stopRecording's flush.
      persist().catch(() => {});
    }
  });

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

  /** Pre-flight: confirm the configured whisper model is downloaded. */
  async function modelInstalled(name: string): Promise<boolean> {
    try {
      const all = await invoke<WhisperModelInfo[]>("whisper_models_list");
      return all.find((m) => m.name === name)?.installed ?? false;
    } catch {
      return false;
    }
  }

  async function startRec() {
    transcribeError = "";
    onRequestStartRecording(doStartRec);
  }

  async function doStartRec(): Promise<void> {
    if (transcribeUi.active) return;
    const cfg = await loadConfig();
    const mic = cfg.mic;
    const model = activeModel.startsWith("whisper:")
      ? activeModel.slice("whisper:".length)
      : activeModel || "tiny.en";

    if (!(await modelInstalled(model))) {
      transcribeError =
        `The whisper '${model}' model isn't downloaded yet. Switch family ` +
        `or relaunch to trigger the auto-pull, or check Settings → Models.`;
      return;
    }

    // Snapshot the conversation before starting so deltas land on it
    // even if the user navigates away mid-recording.
    const conv = await persist({ force: true });
    try {
      await startRecording({
        model,
        device: mic.device_name || null,
        conversationId: conv?.id ?? null,
      });
    } catch (e) {
      transcribeError = String(e);
    }
  }

  async function stopRec() {
    await stopRecording();
    flushLiveDelta();
    clearAfterPersist();
    persist().catch((e) => console.warn("save after stop failed:", e));
  }

  async function handleModeChange(mode: Mode) {
    // No longer auto-stop on mode switch — that's the whole point of the
    // global store. The recording keeps capturing in the background and
    // the StatusBar shows progress from any mode.
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

  // True iff this view is the one tied to the active recording — used
  // to draw the rec dot in the local pane chrome.
  let isMyRecording = $derived(
    transcribeUi.active && transcribeUi.conversationId === conversationId,
  );

  // True iff Talking Points is the chat-slot occupant for this very
  // conversation — controls the "Activate" button vs. the running
  // panel content in the right pane.
  let isMyTalkingPoints = $derived(
    chatSlot.kind === "tp" && chatSlot.conversationId === conversationId,
  );

  /** Read the talking_points file off disk when our TP loop persists.
   *  Watching `chatSlot` for cycles isn't enough — the loop writes
   *  through `saveConversation` and we want to reflect that here. */
  $effect(() => {
    if (!isMyTalkingPoints) return;
    void chatSlot.elapsed; // tick on each elapsed update so we re-read
    const id = conversationId;
    if (!id) return;
    let cancelled = false;
    loadConversation(id)
      .then((c) => {
        if (cancelled || !c) return;
        talkingPoints = c.talking_points ?? [];
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });
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
        {#if isMyRecording && !transcribeUi.paused}
          <span class="rec-dot" aria-hidden="true"></span>
          <span class="rec-time">{fmtElapsed(transcribeUi.elapsed)}</span>
        {:else if isMyRecording && transcribeUi.paused}
          <span class="rec-paused">paused</span>
        {/if}
      </header>
      <div class="pane-body">
        {#if transcript}
          <pre class="transcript">{transcript}</pre>
        {:else}
          <div class="placeholder">
            {#if isMyRecording}
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
        {#if isMyTalkingPoints}
          <span class="tp-running">
            <span class="tp-dot" aria-hidden="true"></span>
            {chatSlot.status === "paused" ? "paused" : "live"}
          </span>
        {/if}
      </header>
      <div class="pane-body">
        {#if isMyTalkingPoints}
          {#if talkingPoints.length > 0}
            <ul class="bullets">
              {#each talkingPoints as point, i (i)}
                <li>{point}</li>
              {/each}
            </ul>
          {:else}
            <div class="placeholder">
              Listening… the first summary will arrive once whisper has a
              chunk or two of transcript to work with.
            </div>
          {/if}
        {:else if talkingPoints.length > 0}
          <ul class="bullets dim">
            {#each talkingPoints as point, i (i)}
              <li>{point}</li>
            {/each}
          </ul>
          {#if isMyRecording && chatSlot.kind === null}
            <div class="tp-activate-row">
              <button class="tp-activate" onclick={onRequestActivateTalkingPoints}>
                Resume Talking Points
              </button>
            </div>
          {/if}
        {:else if isMyRecording && chatSlot.kind === null}
          <div class="tp-activate-shell">
            <button class="tp-activate big" onclick={onRequestActivateTalkingPoints}>
              <span class="tp-spark" aria-hidden="true">✦</span>
              Activate Talking Points
            </button>
            <p class="tp-help">
              Continuously summarises the transcript into a live bullet
              list. Uses the chat model — the Text slot will be held until
              you stop it.
            </p>
          </div>
        {:else if isMyRecording && chatSlot.kind && chatSlot.conversationId !== conversationId}
          <div class="placeholder">
            The chat slot is busy with another conversation. Stop it from
            the Text mode button to free up Talking Points here.
          </div>
        {:else}
          <div class="placeholder">
            Talking points will be summarised here once a session is
            running and you activate the feature.
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
    onRequestStopTranscribe={() => onRequestStopTranscribe()}
    onRequestStopChat={() => onRequestStopChat()}
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
    {#if isMyRecording}
      <button class="record-btn stop" onclick={onRequestStopTranscribe} title="Stop recording">
        <span class="rec-square" aria-hidden="true"></span>
        Stop
      </button>
    {:else}
      <button
        class="record-btn"
        onclick={startRec}
        title={transcribeUi.active ? "Another recording is in progress — confirm to stop it first" : "Start recording"}
      >
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
  .rec-paused {
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    font-size: .76rem;
    color: #d4a64a;
    text-transform: uppercase;
    letter-spacing: .05em;
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
  .bullets.dim li { color: #888; }
  .tp-running {
    display: inline-flex;
    align-items: center;
    gap: .35rem;
    font-size: .7rem;
    color: #b899f7;
    text-transform: uppercase;
    letter-spacing: .06em;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .tp-dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: #b899f7;
    box-shadow: 0 0 6px #b899f7;
    animation: rec-pulse 1.4s ease-in-out infinite;
  }
  .tp-activate-shell {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: .85rem;
    text-align: center;
    padding: 2rem .5rem;
  }
  .tp-activate-row {
    margin-top: 1rem;
    display: flex;
    justify-content: center;
  }
  .tp-activate {
    background: #2a2147;
    color: #ddd2ff;
    border: 1px solid #4a3a7a;
    border-radius: 8px;
    padding: .55rem 1rem;
    font-size: .85rem;
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: .45rem;
    transition: background .15s, border-color .15s;
  }
  .tp-activate:hover { background: #352856; border-color: #6e6ef7; }
  .tp-activate.big { padding: .75rem 1.25rem; font-size: .92rem; }
  .tp-spark { color: #b899f7; font-size: 1rem; line-height: 1; }
  .tp-help {
    color: #777;
    font-size: .78rem;
    line-height: 1.55;
    max-width: 36ch;
    margin: 0;
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
