<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { open as openExternal } from "@tauri-apps/plugin-shell";
  import { onMount, onDestroy } from "svelte";
  import type { HardwareProfile } from "../types";

  let { hardware, activeModel, onComplete } = $props<{
    hardware: HardwareProfile | null;
    activeModel: string;
    onComplete: () => void;
  }>();

  type Phase = "check" | "install-ollama" | "pull" | "done" | "error";

  // Mirrors the Rust `PullEvent` emitted from ollama::pull_with. `total` /
  // `completed` are byte counts for the layer currently transferring;
  // status-only frames (e.g. "pulling manifest", "verifying sha256 digest",
  // "writing manifest", "success") arrive with both at 0.
  interface PullEvent {
    status: string;
    digest?: string;
    total?: number;
    completed?: number;
    percent?: number;
    done?: boolean;
  }

  let phase = $state<Phase>("check");
  let progress = $state("");
  let progressPercent = $state<number | null>(null); // 0–1, null = indeterminate
  let progressBytes = $state<{ done: number; total: number } | null>(null);
  let errorMsg = $state("");
  let unlisten: (() => void) | null = null;

  // Smoothed transfer-rate display, computed from the delta between successive
  // PullEvent frames. Useful as direct feedback that the download is or isn't
  // making real progress.
  let lastSampleAt = 0;
  let lastSampleBytes = 0;
  let bytesPerSec = $state<number | null>(null);

  onMount(async () => {
    unlisten = await listen<PullEvent>("ollama-pull-progress", (e) => {
      const evt = e.payload;
      progress = formatStatus(evt);
      if (evt.total && evt.total > 0) {
        progressPercent = evt.percent ?? (evt.completed ?? 0) / evt.total;
        progressBytes = { done: evt.completed ?? 0, total: evt.total };
        const now = Date.now();
        if (lastSampleAt && evt.completed != null && evt.completed >= lastSampleBytes) {
          const dt = (now - lastSampleAt) / 1000;
          if (dt >= 0.5) {
            bytesPerSec = (evt.completed - lastSampleBytes) / dt;
            lastSampleAt = now;
            lastSampleBytes = evt.completed;
          }
        } else {
          lastSampleAt = now;
          lastSampleBytes = evt.completed ?? 0;
        }
      } else {
        // Indeterminate phase (manifest fetch, verify, write).
        progressPercent = null;
        progressBytes = null;
        bytesPerSec = null;
      }
    });
    await run();
  });

  onDestroy(() => unlisten?.());

  async function run() {
    try {
      // Install Ollama if missing
      const installed = await invoke<boolean>("ollama_installed");
      if (!installed) {
        phase = "install-ollama";
        progress = "Installing Ollama…";
        await invoke("ollama_install");
      }

      // Pull model
      phase = "pull";
      progress = "Starting download…";
      progressPercent = null;
      progressBytes = null;
      await invoke("ollama_pull", { model: activeModel });

      phase = "done";
      onComplete();
    } catch (e) {
      errorMsg = String(e);
      phase = "error";
    }
  }

  function formatStatus(evt: PullEvent): string {
    // Tidy up Ollama's raw status strings — the digest-prefixed
    // "pulling 8a000b0d4e5a" form is more noise than signal in a one-line UI.
    const s = evt.status || "";
    if (/^pulling [0-9a-f]{6,}/i.test(s)) return "Downloading model";
    if (/^pulling manifest$/i.test(s)) return "Fetching manifest";
    if (/^verifying/i.test(s)) return "Verifying";
    if (/^writing manifest$/i.test(s)) return "Finalizing";
    if (/^removing/i.test(s)) return "Cleaning up";
    if (/^success$/i.test(s)) return "Done";
    return s.charAt(0).toUpperCase() + s.slice(1);
  }

  function formatBytes(n: number): string {
    if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`;
    if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
    if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
    return `${n} B`;
  }

  function formatRate(bps: number | null): string {
    if (bps == null || bps <= 0) return "";
    return `${formatBytes(bps)}/s`;
  }

  function formatModel(name: string): string {
    return name.split(":")[0].replace(/-/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  }

  // Split an error message into text + URL pieces so the URL can render as a
  // clickable link. The Windows "install Ollama manually" path has the
  // download URL embedded in the error string and the user otherwise has no
  // way to follow it from inside the app.
  function splitOnUrl(s: string): Array<{ kind: "text" | "url"; value: string }> {
    const parts: Array<{ kind: "text" | "url"; value: string }> = [];
    const re = /https?:\/\/\S+/g;
    let last = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(s))) {
      if (m.index > last) parts.push({ kind: "text", value: s.slice(last, m.index) });
      // Strip trailing punctuation that's almost never part of the URL.
      let url = m[0].replace(/[.,;:!?)\]]+$/, "");
      parts.push({ kind: "url", value: url });
      last = m.index + url.length;
    }
    if (last < s.length) parts.push({ kind: "text", value: s.slice(last) });
    return parts;
  }
</script>

<div class="first-run">
  <div class="content">
    <h1>AnyAI</h1>

    {#if hardware}
      <p class="hw">
        {#if hardware.soc}{hardware.soc} · {/if}{hardware.vram_gb != null
          ? `${hardware.vram_gb.toFixed(0)} GB ${hardware.gpu_type.toUpperCase()} · ${hardware.ram_gb.toFixed(0)} GB RAM`
          : `${hardware.ram_gb.toFixed(0)} GB RAM · CPU only`}
      </p>
    {/if}

    {#if phase !== "error"}
      <div class="status-block">
        <div class="model-name">{formatModel(activeModel)}</div>
        <div class="model-tag">{activeModel}</div>

        <div class="step">
          <span class="dot" class:active={phase === "install-ollama"}></span>
          Ollama
        </div>
        <div class="step">
          <span class="dot" class:active={phase === "pull"}></span>
          Downloading model
        </div>

        {#if phase === "pull"}
          <div class="bar" class:indeterminate={progressPercent === null}>
            {#if progressPercent !== null}
              <div class="bar-fill" style="width: {(progressPercent * 100).toFixed(1)}%"></div>
            {/if}
          </div>
          <div class="progress-meta">
            <span class="progress-status">{progress || "…"}</span>
            {#if progressBytes}
              <span class="progress-bytes">
                {formatBytes(progressBytes.done)} / {formatBytes(progressBytes.total)}
                {#if progressPercent !== null}
                  ({(progressPercent * 100).toFixed(1)}%)
                {/if}
                {#if bytesPerSec}
                  · {formatRate(bytesPerSec)}
                {/if}
              </span>
            {/if}
          </div>
        {:else if progress}
          <p class="progress">{progress}</p>
        {/if}
      </div>
    {:else}
      <div class="error-block">
        <p>Something went wrong:</p>
        <code>
          {#each splitOnUrl(errorMsg) as part}
            {#if part.kind === "url"}<a href={part.value} onclick={(e) => { e.preventDefault(); openExternal(part.value); }}>{part.value}</a>{:else}{part.value}{/if}
          {/each}
        </code>
        <button onclick={run}>Retry</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .first-run {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .content {
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1rem;
    max-width: 340px;
  }
  h1 { font-size: 2rem; font-weight: 700; letter-spacing: -.03em; }
  .hw { color: #666; font-size: .85rem; }
  .status-block { width: 100%; background: #1a1a1a; border-radius: 10px; padding: 1.25rem; display: flex; flex-direction: column; gap: .6rem; }
  .model-name { font-size: 1.15rem; font-weight: 600; }
  .model-tag { font-size: .75rem; color: #555; font-family: monospace; }
  .step { display: flex; align-items: center; gap: .5rem; font-size: .875rem; color: #888; }
  .dot {
    width: 8px; height: 8px; border-radius: 50%; background: #333;
    transition: background .3s;
  }
  .dot.active { background: #6e6ef7; box-shadow: 0 0 6px #6e6ef7; }
  .progress { font-size: .78rem; color: #555; font-family: monospace; word-break: break-all; }
  .bar {
    width: 100%;
    height: 6px;
    background: #2a2a2a;
    border-radius: 3px;
    overflow: hidden;
    position: relative;
  }
  .bar-fill {
    height: 100%;
    background: linear-gradient(90deg, #6e6ef7, #8a8af7);
    transition: width .25s ease;
  }
  .bar.indeterminate::after {
    content: "";
    position: absolute;
    top: 0; left: -40%;
    width: 40%; height: 100%;
    background: linear-gradient(90deg, transparent, #6e6ef7, transparent);
    animation: slide 1.4s infinite ease-in-out;
  }
  @keyframes slide {
    0% { left: -40%; }
    100% { left: 100%; }
  }
  .progress-meta {
    display: flex;
    flex-direction: column;
    gap: .15rem;
    font-size: .72rem;
    color: #777;
    font-family: monospace;
    text-align: left;
  }
  .progress-status { color: #aaa; }
  .progress-bytes { color: #666; }
  .error-block { display: flex; flex-direction: column; gap: .75rem; align-items: center; }
  .error-block code { font-size: .8rem; color: #f66; background: #1a1a1a; padding: .5rem; border-radius: 6px; word-break: break-all; }
  .error-block code a { color: #6e6ef7; text-decoration: underline; cursor: pointer; }
  button {
    padding: .5rem 1.25rem; background: #6e6ef7; color: #fff; border: none;
    border-radius: 6px; cursor: pointer; font-size: .875rem;
  }
  button:hover { background: #5a5ae0; }
</style>
