<script lang="ts">
  import { updateUi } from "../update-state.svelte";
  import {
    transcribeUi,
    pauseRecording,
    resumeRecording,
  } from "./transcribe-state.svelte";
  import type { Mode } from "../types";

  let { model, mode, family, sidebarOpen, onToggleSidebar, onOpenSettings, onRequestStopTranscribe, onJumpToTranscribe } = $props<{
    model: string;
    mode: Mode;
    family: string;
    sidebarOpen: boolean;
    onToggleSidebar: () => void;
    onOpenSettings: (tab: "providers" | "families" | "models" | "storage" | "updates") => void;
    /** Wired by App so the stop-with-warning dialog lives outside the
     *  status bar (which renders inside Chat / TranscribeView and would
     *  make the dialog vanish on mode switch). */
    onRequestStopTranscribe?: () => void;
    /** Click target for the recording chip — jumps to Transcribe view so
     *  the user can see what's being captured. Optional: omitted in
     *  TranscribeView where we're already there. */
    onJumpToTranscribe?: () => void;
  }>();

  // If the update dot is showing, the user almost certainly clicked the
  // Settings button *because* of it — drop them on the Updates tab so they
  // don't have to dig through the sidebar to find what they came for.
  function openSettings() {
    onOpenSettings(updateUi.available ? "updates" : "providers");
  }

  function fmtElapsed(sec: number): string {
    const m = Math.floor(sec / 60).toString().padStart(2, "0");
    const s = (sec % 60).toString().padStart(2, "0");
    return `${m}:${s}`;
  }
</script>

<div class="status-bar">
  <button
    class="hamburger"
    onclick={onToggleSidebar}
    title={sidebarOpen ? "Hide conversations" : "Show conversations"}
    aria-label="Toggle conversations"
    aria-expanded={sidebarOpen}
  >
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
      <path
        fill="currentColor"
        d="M3 6h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2zm0 5h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2zm0 5h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2z"
      />
    </svg>
  </button>
  <button class="provider-btn" onclick={() => onOpenSettings("families")} title="Change family / model">
    <span class="dot"></span>
    {#if family}
      <span class="family-name">{family}</span>
      <span class="separator">·</span>
    {/if}
    <span class="model-name">{model}</span>
  </button>

  {#if transcribeUi.active}
    <div
      class="rec-chip"
      class:paused={transcribeUi.paused}
      class:drain={transcribeUi.drainOnly}
      role="group"
      aria-label="Transcription controls"
    >
      <button
        class="rec-label"
        onclick={onJumpToTranscribe}
        disabled={!onJumpToTranscribe}
        title={transcribeUi.drainOnly
          ? "Recovering transcript from previous session"
          : transcribeUi.paused
          ? "Mic paused — backlog draining"
          : "Recording — click to open Transcribe"}
      >
        <span class="rec-dot" aria-hidden="true"></span>
        {#if transcribeUi.drainOnly}
          <span class="rec-text">Recovering…</span>
        {:else}
          <span class="rec-text">{transcribeUi.paused ? "Paused" : "Rec"}</span>
          <span class="rec-time">{fmtElapsed(transcribeUi.elapsed)}</span>
        {/if}
        {#if transcribeUi.pendingChunks > 0}
          <span class="rec-backlog" title="{transcribeUi.pendingChunks} chunks pending whisper inference">
            +{transcribeUi.pendingChunks * 5}s
          </span>
        {/if}
      </button>
      {#if !transcribeUi.drainOnly}
        {#if transcribeUi.paused}
          <button class="rec-ctrl" onclick={() => resumeRecording()} title="Resume mic">
            <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
              <path fill="currentColor" d="M8 5v14l11-7z" />
            </svg>
          </button>
        {:else}
          <button class="rec-ctrl" onclick={() => pauseRecording()} title="Pause mic (keeps draining backlog)">
            <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
              <path fill="currentColor" d="M6 5h4v14H6zM14 5h4v14h-4z" />
            </svg>
          </button>
        {/if}
      {/if}
      <button
        class="rec-ctrl rec-stop"
        onclick={onRequestStopTranscribe}
        disabled={!onRequestStopTranscribe}
        title={transcribeUi.pendingChunks > 0
          ? `Stop (${transcribeUi.pendingChunks} chunks still pending)`
          : "Stop"}
      >
        <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
          <rect x="6" y="6" width="12" height="12" fill="currentColor" rx="1.5" />
        </svg>
      </button>
    </div>
  {/if}

  <div class="spacer"></div>
  <button
    class="models-btn"
    onclick={openSettings}
    title={updateUi.available
      ? `Update ${updateUi.available.version} available`
      : "Open settings"}
  >
    <span class="grid-icon" aria-hidden="true">⊞</span>
    <span class="label">Models/Settings</span>
    <svg
      class="gear-icon"
      viewBox="0 0 24 24"
      width="13"
      height="13"
      aria-hidden="true"
    >
      <path
        fill="currentColor"
        d="M19.43 12.98a7.7 7.7 0 0 0 0-1.96l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.5 7.5 0 0 0-1.7-.98l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.5.42l-.36 2.54a7.5 7.5 0 0 0-1.7.98l-2.39-.96a.5.5 0 0 0-.6.22L2.8 8.8a.5.5 0 0 0 .12.64l2.03 1.58a7.7 7.7 0 0 0 0 1.96L2.92 14.56a.5.5 0 0 0-.12.64l1.92 3.32a.5.5 0 0 0 .6.22l2.39-.96a7.5 7.5 0 0 0 1.7.98l.36 2.54a.5.5 0 0 0 .5.42h3.84a.5.5 0 0 0 .5-.42l.36-2.54a7.5 7.5 0 0 0 1.7-.98l2.39.96a.5.5 0 0 0 .6-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58zM12 15.5a3.5 3.5 0 1 1 0-7 3.5 3.5 0 0 1 0 7z"
      />
    </svg>
    {#if updateUi.available}
      <span
        class="update-dot"
        aria-label="Update {updateUi.available.version} available"
      ></span>
    {/if}
  </button>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    padding: .4rem .75rem;
    border-bottom: 1px solid #1a1a1a;
    background: #0d0d0d;
    gap: .5rem;
  }
  .hamburger {
    background: none;
    border: none;
    color: #777;
    cursor: pointer;
    padding: .25rem .35rem;
    border-radius: 5px;
    display: flex;
    align-items: center;
  }
  .hamburger:hover { background: #1a1a1a; color: #ccc; }
  .provider-btn {
    display: flex;
    align-items: center;
    gap: .4rem;
    background: none;
    border: none;
    color: #888;
    font-size: .78rem;
    font-family: monospace;
    cursor: pointer;
    padding: .2rem .5rem;
    border-radius: 5px;
    max-width: 60%;
  }
  .provider-btn:hover { background: #1a1a1a; color: #ccc; }
  .dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: #4caf50;
    box-shadow: 0 0 4px #4caf50;
    flex-shrink: 0;
  }
  .family-name { color: #6e6ef7; flex-shrink: 0; }
  .separator { color: #444; flex-shrink: 0; }
  .model-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .rec-chip {
    display: inline-flex;
    align-items: center;
    gap: .15rem;
    background: #1f1212;
    border: 1px solid #4a2020;
    border-radius: 6px;
    padding: .15rem .15rem .15rem .35rem;
    margin-left: .25rem;
  }
  .rec-chip.paused { background: #1f1c12; border-color: #4a4220; }
  .rec-chip.drain { background: #121a22; border-color: #1f3b54; }
  .rec-label {
    background: none; border: none; cursor: pointer;
    display: inline-flex; align-items: center; gap: .35rem;
    padding: .1rem .35rem .1rem .1rem; color: inherit;
    font-size: .72rem; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  }
  .rec-label:disabled { cursor: default; }
  .rec-label .rec-text { color: #f0a3a3; font-weight: 600; letter-spacing: .03em; }
  .rec-chip.paused .rec-text { color: #f0d49a; }
  .rec-chip.drain .rec-text { color: #9acaea; }
  .rec-time { color: #e0c5c5; }
  .rec-chip.paused .rec-time { color: #d4c8a8; }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #e35a5a;
    box-shadow: 0 0 6px #e35a5a;
    animation: rec-pulse 1.4s ease-in-out infinite;
    flex-shrink: 0;
  }
  .rec-chip.paused .rec-dot {
    background: #d4a64a; box-shadow: 0 0 6px #d4a64a;
    animation: none;
  }
  .rec-chip.drain .rec-dot {
    background: #6e9ad4; box-shadow: 0 0 6px #6e9ad4;
  }
  @keyframes rec-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: .35; }
  }
  .rec-backlog {
    background: #2a1410; color: #f0c2a8;
    padding: 0 .3rem; border-radius: 3px;
    font-size: .65rem; letter-spacing: .03em;
  }
  .rec-chip.paused .rec-backlog { background: #2a2410; color: #f0d8a8; }
  .rec-chip.drain .rec-backlog { background: #122030; color: #a8c8f0; }
  .rec-ctrl {
    background: none; border: none; cursor: pointer;
    color: #d8a4a4; padding: .2rem .3rem; border-radius: 4px;
    display: inline-flex; align-items: center; justify-content: center;
  }
  .rec-ctrl:hover:not(:disabled) { background: #2a1414; color: #fff; }
  .rec-ctrl:disabled { opacity: .4; cursor: default; }
  .rec-chip.paused .rec-ctrl { color: #d8c8a4; }
  .rec-chip.paused .rec-ctrl:hover:not(:disabled) { background: #2a2814; color: #fff; }
  .rec-chip.drain .rec-ctrl { color: #a4c4e8; }
  .rec-chip.drain .rec-ctrl:hover:not(:disabled) { background: #14202a; color: #fff; }
  .rec-stop:hover:not(:disabled) { color: #fff; background: #5a2424 !important; }

  .spacer { flex: 1; }
  .models-btn {
    display: flex;
    align-items: center;
    gap: .4rem;
    background: none;
    border: none;
    color: #555;
    font-size: .75rem;
    cursor: pointer;
    padding: .2rem .5rem;
    border-radius: 5px;
  }
  .models-btn { position: relative; }
  .models-btn:hover { background: #1a1a1a; color: #ccc; }
  .grid-icon { font-size: .85rem; line-height: 1; }
  .gear-icon { display: block; }
  .label { line-height: 1; }
  .update-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #f59e0b;
    box-shadow: 0 0 6px rgba(245, 158, 11, 0.7);
    flex-shrink: 0;
    margin-left: .15rem;
  }
</style>
