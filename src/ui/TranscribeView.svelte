<script lang="ts">
  import { onDestroy } from "svelte";
  import ModeBar from "./ModeBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import { loadConfig } from "../config";
  import {
    loadConversation,
    saveConversation,
    newConversation,
    type Conversation,
  } from "../conversations";
  import type { HardwareProfile, Mode, MicConfig } from "../types";

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

  let activeConversation = $state<Conversation | null>(null);
  let transcript = $state("");
  let talkingPoints = $state<string[]>([]);
  /** Title input — bound directly to the bottom-bar field. Persisted on
   *  blur / record-start so we don't write a JSON file on every keystroke. */
  let sessionName = $state("");
  let settingsTab = $state<"providers" | "families" | "models" | "storage" | null>(null);

  // Recording state. We capture the raw mic stream so the VU meter can live
  // next to the record button; actual transcription wiring is a follow-up
  // (no ASR pipeline yet — see ARCHITECTURE.md).
  let recording = $state(false);
  let recordingStartedAt = $state(0);
  let elapsed = $state(0);
  let level = $state(0);
  let micError = $state("");
  let stream: MediaStream | null = null;
  let audioCtx: AudioContext | null = null;
  let raf = 0;
  let elapsedTimer: ReturnType<typeof setInterval> | null = null;

  /** Guard for WebViews (notably WebKitGTK / older WKWebView) that don't
   *  expose mediaDevices.getUserMedia. We disable the Record button and
   *  surface the limitation instead of letting the click crash. */
  const canCaptureMic =
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices &&
    typeof navigator.mediaDevices.getUserMedia === "function";

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
      // Treat the placeholder title as "blank" so users see the prompt copy
      // instead of literal "New chat" in the field.
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

  function onNameBlur() {
    persist().catch((e) => console.warn("save title failed:", e));
  }

  async function startRecording() {
    if (recording) return;
    micError = "";
    if (!canCaptureMic) {
      micError =
        "This WebView build can't open the microphone. Native audio capture " +
        "is on the roadmap — see Hardware → Microphone for status.";
      return;
    }
    try {
      const cfg = await loadConfig();
      const mic: MicConfig = cfg.mic;
      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: mic.device_id ? { exact: mic.device_id } : undefined,
          sampleRate: mic.sample_rate,
          echoCancellation: mic.echo_cancellation,
          noiseSuppression: mic.noise_suppression,
          autoGainControl: mic.auto_gain_control,
        },
      };
      stream = await navigator.mediaDevices.getUserMedia(constraints);
      const Ctx =
        window.AudioContext ??
        (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      audioCtx = new Ctx();
      const src = audioCtx.createMediaStreamSource(stream);
      const analyser = audioCtx.createAnalyser();
      analyser.fftSize = 1024;
      src.connect(analyser);
      const buf = new Float32Array(analyser.fftSize);
      const tick = () => {
        analyser.getFloatTimeDomainData(buf);
        let sum = 0;
        for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
        level = Math.min(1, Math.sqrt(sum / buf.length) * 4);
        raf = requestAnimationFrame(tick);
      };
      recording = true;
      recordingStartedAt = Date.now();
      elapsed = 0;
      elapsedTimer = setInterval(() => {
        elapsed = Math.floor((Date.now() - recordingStartedAt) / 1000);
      }, 250);
      tick();
      // Snapshot the session name so a recording always lives on disk under
      // whatever the user typed when they hit record.
      await persist({ force: true });
    } catch (e: unknown) {
      const err = e as { name?: string; message?: string };
      if (err?.name === "NotAllowedError" || err?.name === "PermissionDeniedError") {
        micError = "Microphone access was blocked. Allow it and try again.";
      } else {
        micError = String(err?.message ?? e);
      }
      stopRecording();
    }
  }

  function stopRecording() {
    recording = false;
    level = 0;
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = null;
    audioCtx?.close().catch(() => {});
    audioCtx = null;
    stream?.getTracks().forEach((t) => t.stop());
    stream = null;
  }

  async function handleModeChange(mode: Mode) {
    if (recording) stopRecording();
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
              Listening… transcription will stream in here.
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

  {#if micError}
    <div class="mic-error">{micError}</div>
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
        onblur={onNameBlur}
        placeholder="Untitled session"
        spellcheck="false"
      />
    </label>
    {#if recording}
      <button class="record-btn stop" onclick={stopRecording} title="Stop recording">
        <span class="vu" aria-hidden="true">
          <span class="vu-fill" style="width: {Math.round(level * 100)}%"></span>
        </span>
        <span class="rec-square" aria-hidden="true"></span>
        Stop
      </button>
    {:else}
      <button
        class="record-btn"
        onclick={startRecording}
        title={canCaptureMic ? "Start recording" : "Microphone capture unavailable in this build"}
        disabled={!canCaptureMic}
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
  .vu {
    display: inline-block;
    width: 56px;
    height: 5px;
    background: rgba(0, 0, 0, .3);
    border-radius: 3px;
    overflow: hidden;
  }
  .vu-fill {
    display: block;
    height: 100%;
    background: linear-gradient(90deg, #ffe7e7 0%, #fff 100%);
    transition: width .05s linear;
  }

  @media (max-width: 700px) {
    .split { flex-direction: column; }
    .pane.left { border-right: none; border-bottom: 1px solid #1a1a1a; }
    .input-row { flex-wrap: wrap; }
    .name-field { order: 3; flex-basis: 100%; }
  }
</style>
