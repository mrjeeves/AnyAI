<script lang="ts">
  import type { Mode } from "../types";
  import { transcribeUi, pauseRecording, resumeRecording } from "./transcribe-state.svelte";
  import {
    chatSlot,
    pauseTalkingPoints,
    resumeTalkingPoints,
    stopTalkingPoints,
  } from "./chat-slot.svelte";

  let {
    current,
    supported,
    tokensUsed,
    contextSize,
    onChange,
    onRequestStopTranscribe,
    onRequestStopChat,
  } = $props<{
    current: Mode;
    /** Modes the active manifest defines tiers for. Modes outside this set
     *  render disabled with an "(unsupported)" hint. */
    supported: Set<Mode>;
    /** Estimated tokens currently in context (history + draft). The bar
     *  shows it as `used / total` with a small ring, no tooltips needed. */
    tokensUsed: number;
    /** Model's reported context window. 0 means "not yet known" — we hide
     *  the saturation block in that case rather than render `0 / 0`. */
    contextSize: number;
    onChange: (mode: Mode) => void;
    /** Stop transcription. Routed to the App-level confirm dialog so the
     *  pending-chunks warning lives in one place. */
    onRequestStopTranscribe: () => void;
    /** Stop the chat-slot occupant — cancels an in-flight chat stream or
     *  stops the Talking Points loop. */
    onRequestStopChat: () => void;
  }>();

  // Trimmed to text + transcribe to match the redesigned mode bar — vision
  // and code aren't surfaced in the GUI right now.
  const modes: Array<{ id: Mode; label: string }> = [
    { id: "text", label: "Text" },
    { id: "transcribe", label: "Transcribe" },
  ];

  const ratio = $derived(contextSize > 0 ? Math.min(1, tokensUsed / contextSize) : 0);

  // SVG ring geometry: circumference = 2πr. r=6 on a 16x16 canvas keeps the
  // stroke from clipping the bbox while leaving a 1px stroke ring readable.
  const RADIUS = 6;
  const CIRC = 2 * Math.PI * RADIUS;
  const dash = $derived(CIRC * ratio);

  /** Saturation-aware ring colour: green → amber → red as the context fills.
   *  Same thresholds the macOS battery icon uses, for familiarity. */
  const ringColor = $derived(
    ratio < 0.6 ? "#4caf50" : ratio < 0.85 ? "#d49a3b" : "#e35a5a",
  );

  /** Compact display: 1234 → "1.2k". Keeps the bar a fixed-ish width so
   *  the mode buttons don't shift as the conversation grows. */
  function fmt(n: number): string {
    if (n < 1000) return String(n);
    if (n < 10_000) return (n / 1000).toFixed(1).replace(/\.0$/, "") + "k";
    return Math.round(n / 1000) + "k";
  }

  function fmtElapsed(sec: number): string {
    const m = Math.floor(sec / 60).toString().padStart(2, "0");
    const s = (sec % 60).toString().padStart(2, "0");
    return `${m}:${s}`;
  }

  // Per-mode slot state pulled from the global stores so any mode's view
  // renders the same indicator. The mode buttons don't care which
  // conversation/session owns the slot — they just reflect "is this slot
  // doing something right now".
  const textKind = $derived(chatSlot.kind);
  const textStatus = $derived(chatSlot.status);
  const textLabel = $derived(
    textKind === "tp"
      ? "Talking Points"
      : textKind === "chat"
        ? chatSlot.conversationTitle || "Chat"
        : "",
  );

  const transcribeStatus = $derived(
    transcribeUi.active
      ? transcribeUi.paused
        ? "paused"
        : transcribeUi.drainOnly
          ? "drain"
          : "running"
      : "idle",
  );

  /** A live chat stream pins the UI to its conversation — the messages list
   *  lives on Chat.svelte, so unmounting that component (by switching modes)
   *  orphans the deltas. We disable the other mode buttons until the stream
   *  releases the slot, matching the "stop to switch" rule for transcribe. */
  const chatRunning = $derived(chatSlot.kind === "chat");
</script>

<div class="mode-bar">
  <div class="modes">
    {#each modes as m}
      {@const ok = supported.has(m.id)}
      {@const isText = m.id === "text"}
      {@const slotStatus = isText ? textStatus : transcribeStatus}
      {@const slotActive = slotStatus !== "idle"}
      {@const lockedOut = chatRunning && m.id !== current}
      {@const btnDisabled = !ok || lockedOut}
      <div
        class="slot"
        class:active={m.id === current}
        class:running={slotStatus === "running"}
        class:paused={slotStatus === "paused"}
        class:drain={slotStatus === "drain"}
        class:unsupported={!ok}
        class:locked={lockedOut}
      >
        <button
          class="mode-btn"
          class:active={m.id === current}
          class:unsupported={!ok}
          disabled={btnDisabled}
          title={!ok
            ? `${m.label} isn't in the active manifest — no model is recommended for it.`
            : lockedOut
              ? "Stop the chat to switch modes."
              : ""}
          onclick={() => !btnDisabled && onChange(m.id)}
        >
          <span class="mode-label">{m.label}{!ok ? " · unsupported" : ""}</span>
          {#if slotActive}
            <span class="status-row" aria-hidden="true">
              <span class="status-dot"></span>
              {#if isText}
                <span class="status-text">{textLabel}</span>
              {:else if slotStatus === "drain"}
                <span class="status-text">Recovering…</span>
              {:else}
                <span class="status-text">{slotStatus === "paused" ? "Paused" : "Rec"}</span>
                <span class="status-time">{fmtElapsed(transcribeUi.elapsed)}</span>
              {/if}
              {#if !isText && transcribeUi.pendingChunks > 0}
                <span class="status-backlog" title="{transcribeUi.pendingChunks} chunks pending whisper inference">
                  +{transcribeUi.pendingChunks * 5}s
                </span>
              {/if}
            </span>
          {/if}
        </button>

        {#if slotActive}
          <div class="ctrls" role="group" aria-label="{m.label} slot controls">
            {#if isText}
              {#if textKind === "tp" && textStatus === "running"}
                <button class="ctrl" onclick={() => pauseTalkingPoints()} title="Pause Talking Points">
                  <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                    <path fill="currentColor" d="M6 5h4v14H6zM14 5h4v14h-4z" />
                  </svg>
                </button>
              {:else if textKind === "tp" && textStatus === "paused"}
                <button class="ctrl" onclick={() => resumeTalkingPoints()} title="Resume Talking Points">
                  <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                    <path fill="currentColor" d="M8 5v14l11-7z" />
                  </svg>
                </button>
              {/if}
              <button
                class="ctrl stop"
                onclick={() => (textKind === "tp" ? stopTalkingPoints() : onRequestStopChat())}
                title={textKind === "tp" ? "Stop Talking Points" : "Stop chat"}
              >
                <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                  <rect x="6" y="6" width="12" height="12" fill="currentColor" rx="1.5" />
                </svg>
              </button>
            {:else}
              {#if slotStatus !== "drain"}
                {#if slotStatus === "paused"}
                  <button class="ctrl" onclick={() => resumeRecording()} title="Resume mic">
                    <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                      <path fill="currentColor" d="M8 5v14l11-7z" />
                    </svg>
                  </button>
                {:else}
                  <button class="ctrl" onclick={() => pauseRecording()} title="Pause mic (keeps draining backlog)">
                    <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                      <path fill="currentColor" d="M6 5h4v14H6zM14 5h4v14h-4z" />
                    </svg>
                  </button>
                {/if}
              {/if}
              <button
                class="ctrl stop"
                onclick={onRequestStopTranscribe}
                title={transcribeUi.pendingChunks > 0
                  ? `Stop (${transcribeUi.pendingChunks} chunks still pending)`
                  : "Stop"}
              >
                <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
                  <rect x="6" y="6" width="12" height="12" fill="currentColor" rx="1.5" />
                </svg>
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>

  {#if contextSize > 0}
    <div
      class="ctx"
      title="Context: {tokensUsed} / {contextSize} tokens"
      aria-label="Context saturation: {tokensUsed} of {contextSize} tokens"
    >
      <svg class="ring" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
        <circle cx="8" cy="8" r={RADIUS} fill="none" stroke="#2a2a2a" stroke-width="2" />
        <circle
          cx="8"
          cy="8"
          r={RADIUS}
          fill="none"
          stroke={ringColor}
          stroke-width="2"
          stroke-linecap="round"
          stroke-dasharray="{dash} {CIRC}"
          transform="rotate(-90 8 8)"
        />
      </svg>
      <span class="num">{fmt(tokensUsed)}</span>
      <span class="sep">/</span>
      <span class="den">{fmt(contextSize)}</span>
    </div>
  {/if}
</div>

<style>
  .mode-bar {
    display: flex;
    align-items: center;
    gap: .5rem;
    padding: .45rem .75rem;
    background: #0f0f0f;
    border-top: 1px solid #1a1a1a;
  }
  .modes { display: flex; gap: .5rem; flex: 1; min-width: 0; flex-wrap: wrap; }

  .slot {
    display: inline-flex;
    align-items: center;
    gap: .15rem;
    border: 1px solid #2a2a2a;
    border-radius: 20px;
    padding: 0;
    background: none;
    transition: border-color .15s, background .15s;
  }
  .slot.running { border-color: #4a2020; background: #1a1010; }
  .slot.paused { border-color: #4a4220; background: #1a1810; }
  .slot.drain { border-color: #1f3b54; background: #0f1820; }

  .mode-btn {
    display: inline-flex;
    align-items: center;
    gap: .4rem;
    padding: .3rem .75rem;
    background: none;
    border: none;
    border-radius: 20px;
    color: #666;
    font-size: .8rem;
    cursor: pointer;
    transition: all .15s;
  }
  .mode-btn:hover:not(:disabled) { color: #ccc; }
  .mode-btn.active { background: #6e6ef7; color: #fff; font-weight: 500; }
  .slot.running .mode-btn.active { background: #6e6ef7; }
  .mode-btn.unsupported {
    opacity: .45;
    cursor: not-allowed;
    font-style: italic;
  }
  .slot.locked .mode-btn {
    opacity: .45;
    cursor: not-allowed;
  }
  .mode-label { line-height: 1; }

  .status-row {
    display: inline-flex;
    align-items: center;
    gap: .3rem;
    padding-left: .4rem;
    margin-left: .15rem;
    border-left: 1px solid rgba(255, 255, 255, .15);
    font-size: .7rem;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .status-dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: #e35a5a;
    box-shadow: 0 0 5px #e35a5a;
    animation: pulse 1.4s ease-in-out infinite;
    flex-shrink: 0;
  }
  .slot.paused .status-dot {
    background: #d4a64a;
    box-shadow: 0 0 5px #d4a64a;
    animation: none;
  }
  .slot.drain .status-dot {
    background: #6e9ad4;
    box-shadow: 0 0 5px #6e9ad4;
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: .35; }
  }
  .status-text { color: #f0a3a3; font-weight: 600; letter-spacing: .03em; }
  .slot.paused .status-text { color: #f0d49a; }
  .slot.drain .status-text { color: #9acaea; }
  .status-time { color: #e0c5c5; }
  .slot.paused .status-time { color: #d4c8a8; }
  .status-backlog {
    background: #2a1410; color: #f0c2a8;
    padding: 0 .3rem; border-radius: 3px;
    font-size: .62rem; letter-spacing: .03em;
  }
  .slot.paused .status-backlog { background: #2a2410; color: #f0d8a8; }
  .slot.drain .status-backlog { background: #122030; color: #a8c8f0; }
  /* Active mode (purple) overrides the per-status text colours so the
     label stays readable against the purple fill. */
  .mode-btn.active .status-row { border-left-color: rgba(255, 255, 255, .35); }
  .mode-btn.active .status-text,
  .mode-btn.active .status-time { color: #fff; }

  .ctrls {
    display: inline-flex;
    align-items: center;
    gap: 0;
    padding: 0 .15rem 0 .05rem;
  }
  .ctrl {
    background: none;
    border: none;
    cursor: pointer;
    color: #d8a4a4;
    padding: .25rem .3rem;
    border-radius: 4px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .ctrl:hover:not(:disabled) { background: #2a1414; color: #fff; }
  .slot.paused .ctrl { color: #d8c8a4; }
  .slot.paused .ctrl:hover:not(:disabled) { background: #2a2814; color: #fff; }
  .slot.drain .ctrl { color: #a4c4e8; }
  .slot.drain .ctrl:hover:not(:disabled) { background: #14202a; color: #fff; }
  .ctrl.stop:hover:not(:disabled) { color: #fff; background: #5a2424; }

  .ctx {
    display: inline-flex;
    align-items: center;
    gap: .3rem;
    color: #777;
    font-size: .72rem;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    user-select: none;
    flex-shrink: 0;
  }
  .ring { display: block; }
  .num { color: #aaa; }
  .sep { color: #444; }
  .den { color: #666; }
</style>
