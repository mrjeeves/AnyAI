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

  let phase = $state<Phase>("check");
  let progress = $state("");
  let errorMsg = $state("");
  let unlisten: (() => void) | null = null;

  onMount(async () => {
    unlisten = await listen<string>("ollama-pull-progress", (e) => {
      progress = e.payload;
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
      await invoke("ollama_pull", { model: activeModel });

      phase = "done";
      onComplete();
    } catch (e) {
      errorMsg = String(e);
      phase = "error";
    }
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
        {hardware.vram_gb != null
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

        {#if progress}
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
  .error-block { display: flex; flex-direction: column; gap: .75rem; align-items: center; }
  .error-block code { font-size: .8rem; color: #f66; background: #1a1a1a; padding: .5rem; border-radius: 6px; word-break: break-all; }
  .error-block code a { color: #6e6ef7; text-decoration: underline; cursor: pointer; }
  button {
    padding: .5rem 1.25rem; background: #6e6ef7; color: #fff; border: none;
    border-radius: 6px; cursor: pointer; font-size: .875rem;
  }
  button:hover { background: #5a5ae0; }
</style>
