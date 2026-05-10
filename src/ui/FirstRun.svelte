<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open as openExternal } from "@tauri-apps/plugin-shell";
  import { onMount, onDestroy } from "svelte";
  import type { HardwareProfile } from "../types";

  let {
    hardware,
    activeModel,
    whisperModel,
    onComplete,
  } = $props<{
    hardware: HardwareProfile | null;
    /** Ollama text-mode tag to pull (always present). */
    activeModel: string;
    /** Optional whisper-rs ggml name (e.g. "tiny.en") to pull alongside
     *  the text model. When the active family resolves transcribe to a
     *  whisper model and that model isn't on disk, App.svelte passes
     *  it in here so first-run can grab both. Empty / missing skips
     *  the whisper pull. */
    whisperModel?: string;
    onComplete: () => void;
  }>();

  type Phase = "check" | "install-ollama" | "pull" | "done" | "error";

  // Mirrors the Rust `PullEvent` emitted from ollama::pull_with. `total` /
  // `completed` are byte counts for the layer currently transferring;
  // status-only frames (e.g. "pulling manifest", "verifying sha256 digest",
  // "writing manifest", "success") arrive with both at 0.
  interface OllamaPullEvent {
    status: string;
    digest?: string;
    total?: number;
    completed?: number;
    percent?: number;
    done?: boolean;
  }

  /** Mirrors `WhisperPullProgress` in src-tauri/src/transcribe.rs. */
  interface WhisperPullEvent {
    name: string;
    bytes: number;
    total: number;
    done: boolean;
    error: string | null;
  }

  /** Per-model UI state for the parallel-pull layout. */
  interface ModelProgress {
    label: string;
    tag: string;
    status: string;
    percent: number | null;
    bytes: { done: number; total: number } | null;
    rate: number | null;
    done: boolean;
    error: string | null;
  }

  let phase = $state<Phase>("check");
  // Initialised in onMount with the freshly-read prop values. We keep
  // `$state` declarations cheap so svelte-check doesn't flag the
  // prop-ref warning ("only captures initial value").
  let textProgress = $state<ModelProgress | null>(null);
  let whisperProgress = $state<ModelProgress | null>(null);
  let errorMsg = $state("");
  let unlisteners: UnlistenFn[] = [];

  // Per-model rate tracking. Smoothing via the delta-between-frames
  // approach the original FirstRun used.
  let textLastSampleAt = 0;
  let textLastSampleBytes = 0;
  let whisperLastSampleAt = 0;
  let whisperLastSampleBytes = 0;

  function emptyProgress(tag: string, label: string): ModelProgress {
    return {
      label,
      tag,
      status: "",
      percent: null,
      bytes: null,
      rate: null,
      done: false,
      error: null,
    };
  }

  onMount(async () => {
    // Seed the per-model progress rows from the props. Either side may
    // be empty / missing — App.svelte only sets the model name when
    // that side is genuinely missing on disk, and we should NOT show
    // a "Downloading" row for something the user already has.
    if (activeModel) {
      textProgress = emptyProgress(activeModel, "Text model");
    }
    if (whisperModel) {
      whisperProgress = emptyProgress(whisperModel, "Transcribe model");
    }
    // Subscribe to both event streams up front so neither pull can fire
    // a frame faster than its listener attaches.
    if (activeModel) {
      unlisteners.push(
        await listen<OllamaPullEvent>("ollama-pull-progress", (e) => {
          if (!textProgress) return;
          const evt = e.payload;
          const status = formatOllamaStatus(evt);
          if (evt.total && evt.total > 0) {
            const completed = evt.completed ?? 0;
            const percent = evt.percent ?? completed / evt.total;
            const now = Date.now();
            let rate = textProgress.rate;
            if (textLastSampleAt && completed >= textLastSampleBytes) {
              const dt = (now - textLastSampleAt) / 1000;
              if (dt >= 0.5) {
                rate = (completed - textLastSampleBytes) / dt;
                textLastSampleAt = now;
                textLastSampleBytes = completed;
              }
            } else {
              textLastSampleAt = now;
              textLastSampleBytes = completed;
            }
            textProgress = {
              ...textProgress,
              status,
              percent,
              bytes: { done: completed, total: evt.total },
              rate,
            };
          } else {
            textProgress = {
              ...textProgress,
              status,
              percent: null,
              bytes: null,
              rate: null,
            };
          }
        }),
      );
    }

    if (whisperModel) {
      const event = `myownllm://whisper-pull/${whisperModel}`;
      unlisteners.push(
        await listen<WhisperPullEvent>(event, (e) => {
          if (!whisperProgress) return;
          const f = e.payload;
          const now = Date.now();
          let rate = whisperProgress.rate;
          if (
            whisperLastSampleAt &&
            f.bytes >= whisperLastSampleBytes
          ) {
            const dt = (now - whisperLastSampleAt) / 1000;
            if (dt >= 0.5) {
              rate = (f.bytes - whisperLastSampleBytes) / dt;
              whisperLastSampleAt = now;
              whisperLastSampleBytes = f.bytes;
            }
          } else {
            whisperLastSampleAt = now;
            whisperLastSampleBytes = f.bytes;
          }
          whisperProgress = {
            ...whisperProgress,
            status: f.error ? `Failed: ${f.error}` : f.done ? "Done" : "Downloading",
            percent: f.total > 0 ? f.bytes / f.total : null,
            bytes: f.total > 0 ? { done: f.bytes, total: f.total } : null,
            rate,
            done: f.done && !f.error,
            error: f.error,
          };
        }),
      );
    }

    await run();
  });

  onDestroy(() => {
    for (const u of unlisteners) u();
  });

  async function run() {
    try {
      // Install Ollama if either we need to pull a text model OR the
      // user chose Ollama-runtime mode at startup. Skipping the
      // install when there's literally nothing for Ollama to do means
      // a transcribe-only first launch isn't gated on a multi-MB
      // ollama install.
      if (activeModel) {
        const installed = await invoke<boolean>("ollama_installed");
        if (!installed) {
          phase = "install-ollama";
          if (textProgress) textProgress = { ...textProgress, status: "Installing Ollama…" };
          await invoke("ollama_install");
        }
      }

      phase = "pull";
      if (textProgress) textProgress = { ...textProgress, status: "Starting download…" };
      if (whisperProgress) {
        whisperProgress = { ...whisperProgress, status: "Starting download…" };
      }

      // Pull each side in parallel only if we have a model name for it.
      // Promise.all rejects on first failure; we wrap each side so a
      // whisper failure surfaces but doesn't abort the text pull, and
      // text failure is fatal (you can't really "skip" the LLM).
      const textPull: Promise<void> = activeModel
        ? invoke("ollama_pull", { model: activeModel })
            .then(() => {
              if (textProgress) textProgress = { ...textProgress, done: true, status: "Done" };
            })
            .catch((e) => {
              if (textProgress) textProgress = { ...textProgress, error: String(e) };
              throw e; // text is required — abort first-run on failure.
            })
        : Promise.resolve();

      const whisperPull: Promise<void> = whisperModel
        ? invoke("whisper_model_pull", { name: whisperModel })
            .then(() => {
              if (whisperProgress) {
                whisperProgress = { ...whisperProgress, done: true, status: "Done" };
              }
            })
            .catch((e) => {
              if (whisperProgress) {
                whisperProgress = { ...whisperProgress, error: String(e) };
              }
              // Whisper failure is non-fatal — text mode still works.
            })
        : Promise.resolve();

      await Promise.all([textPull, whisperPull]);

      phase = "done";
      onComplete();
    } catch (e) {
      errorMsg = String(e);
      phase = "error";
    }
  }

  function formatOllamaStatus(evt: OllamaPullEvent): string {
    const s = evt.status || "";
    if (/^pulling [0-9a-f]{6,}/i.test(s)) return "Downloading";
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

  function splitOnUrl(s: string): Array<{ kind: "text" | "url"; value: string }> {
    const parts: Array<{ kind: "text" | "url"; value: string }> = [];
    const re = /https?:\/\/\S+/g;
    let last = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(s))) {
      if (m.index > last) parts.push({ kind: "text", value: s.slice(last, m.index) });
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
    <h1>MyOwnLLM</h1>

    {#if hardware}
      <p class="hw">
        {#if hardware.soc}{hardware.soc} · {/if}{hardware.vram_gb != null
          ? `${hardware.vram_gb.toFixed(0)} GB ${hardware.gpu_type.toUpperCase()} · ${hardware.ram_gb.toFixed(0)} GB RAM`
          : `${hardware.ram_gb.toFixed(0)} GB RAM · CPU only`}
      </p>
    {/if}

    {#if phase !== "error"}
      <div class="status-block">
        <div class="step">
          <span class="dot" class:active={phase === "install-ollama"}></span>
          Ollama
        </div>

        {#snippet modelRow(p: ModelProgress, primary: boolean)}
          <div class="model-row">
            <div class="model-row-head">
              <span class="model-label">{p.label}</span>
              <code class="model-tag">{p.tag}</code>
              {#if p.done}
                <span class="model-badge">✓ ready</span>
              {/if}
            </div>
            <div class="bar" class:indeterminate={!p.done && p.percent === null}>
              {#if p.percent !== null}
                <div class="bar-fill" style="width: {(p.percent * 100).toFixed(1)}%"></div>
              {:else if p.done}
                <div class="bar-fill" style="width: 100%"></div>
              {/if}
            </div>
            <div class="progress-meta">
              <span class="progress-status">{p.status || "…"}</span>
              {#if p.bytes}
                <span class="progress-bytes">
                  {formatBytes(p.bytes.done)} / {formatBytes(p.bytes.total)}
                  {#if p.percent !== null}
                    ({(p.percent * 100).toFixed(1)}%)
                  {/if}
                  {#if p.rate}
                    · {formatRate(p.rate)}
                  {/if}
                </span>
              {/if}
              {#if p.error}
                <span class="progress-error">{p.error}</span>
              {/if}
            </div>
          </div>
        {/snippet}

        {#if textProgress}
          {@render modelRow(textProgress, true)}
        {/if}
        {#if whisperProgress}
          {@render modelRow(whisperProgress, false)}
        {/if}
        {#if !textProgress && !whisperProgress}
          <p class="empty">Nothing to download — finishing up…</p>
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
    max-width: 380px;
  }
  h1 { font-size: 2rem; font-weight: 700; letter-spacing: -.03em; }
  .hw { color: #666; font-size: .85rem; }
  .status-block {
    width: 100%;
    background: #1a1a1a;
    border-radius: 10px;
    padding: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: .85rem;
  }
  .model-row {
    display: flex;
    flex-direction: column;
    gap: .35rem;
    text-align: left;
  }
  .model-row-head {
    display: flex;
    align-items: center;
    gap: .55rem;
    flex-wrap: wrap;
  }
  .model-label { font-size: .82rem; font-weight: 600; color: #ccc; }
  .model-tag { font-size: .72rem; color: #888; font-family: monospace; }
  .model-badge { font-size: .68rem; color: #6c6; margin-left: auto; }
  .step { display: flex; align-items: center; gap: .5rem; font-size: .82rem; color: #888; }
  .dot {
    width: 8px; height: 8px; border-radius: 50%; background: #333;
    transition: background .3s;
  }
  .dot.active { background: #6e6ef7; box-shadow: 0 0 6px #6e6ef7; }
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
  }
  .progress-status { color: #aaa; }
  .progress-bytes { color: #666; }
  .progress-error { color: #d66; }
  .empty { color: #777; font-size: .82rem; text-align: center; padding: .5rem 0; }
  .error-block { display: flex; flex-direction: column; gap: .75rem; align-items: center; }
  .error-block code { font-size: .8rem; color: #f66; background: #1a1a1a; padding: .5rem; border-radius: 6px; word-break: break-all; }
  .error-block code a { color: #6e6ef7; text-decoration: underline; cursor: pointer; }
  button {
    padding: .5rem 1.25rem; background: #6e6ef7; color: #fff; border: none;
    border-radius: 6px; cursor: pointer; font-size: .875rem;
  }
  button:hover { background: #5a5ae0; }
</style>
