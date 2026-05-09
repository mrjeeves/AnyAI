<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { loadConfig } from "../../config";
  import type { HardwareProfile, GpuType } from "../../types";

  type Tab = "providers" | "families" | "models" | "storage" | "updates" | "hardware";

  let { setActive } = $props<{ setActive: (tab: Tab) => void }>();

  let hardware = $state<HardwareProfile | null>(null);
  let conversationDir = $state("");
  let loading = $state(true);
  let error = $state("");

  onMount(async () => {
    try {
      const [hw, config] = await Promise.all([
        invoke<HardwareProfile>("detect_hardware"),
        loadConfig(),
      ]);
      hardware = hw;
      conversationDir = config.conversation_dir ?? "";
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  function gpuLabel(g: GpuType): string {
    switch (g) {
      case "nvidia": return "NVIDIA";
      case "amd":    return "AMD";
      case "apple":  return "Apple Silicon";
      case "none":   return "None detected";
    }
  }

  function gbLabel(gb: number | null | undefined): string {
    if (gb == null) return "—";
    return `${gb.toFixed(1)} GB`;
  }
</script>

<div class="section">
  <div class="head">
    <p class="lede">
      What AnyAI sees on this machine. The resolver picks model tiers against
      <strong>VRAM</strong> and <strong>RAM</strong>; storage limits how many
      models you can keep pulled.
    </p>
  </div>

  {#if loading}
    <p class="loading">Loading…</p>
  {:else if error && !hardware}
    <p class="error">{error}</p>
  {:else if hardware}
    <div class="cards">
      <div class="group-label">Compute</div>

      <div class="card">
        <div class="card-title">Accelerator</div>
        <dl class="info">
          <div>
            <dt>GPU</dt>
            <dd>
              <span class="badge gpu-{hardware.gpu_type}">{gpuLabel(hardware.gpu_type)}</span>
            </dd>
          </div>
          <div>
            <dt>VRAM</dt>
            <dd>
              {gbLabel(hardware.vram_gb)}
              {#if hardware.gpu_type === "apple" && hardware.vram_gb != null}
                <span class="dim">(unified)</span>
              {/if}
            </dd>
          </div>
          <div>
            <dt>Used for</dt>
            <dd class="dim">tier selection in picks</dd>
          </div>
        </dl>
        {#if hardware.gpu_type === "none"}
          <p class="card-meta">
            No discrete GPU detected — picks fall back to CPU-friendly tiers
            sized against RAM.
          </p>
        {/if}
      </div>

      <div class="card">
        <div class="card-title">CPU &amp; system memory</div>
        <dl class="info">
          <div>
            <dt>Architecture</dt>
            <dd><code>{hardware.arch ?? "unknown"}</code></dd>
          </div>
          <div>
            <dt>RAM</dt>
            <dd>{gbLabel(hardware.ram_gb)}</dd>
          </div>
          {#if hardware.soc}
            <div>
              <dt>Board</dt>
              <dd>{hardware.soc}</dd>
            </div>
          {/if}
        </dl>
      </div>

      <div class="group-label">Storage</div>

      <div class="card">
        <div class="card-title">Disk</div>
        <dl class="info">
          <div>
            <dt>Free space</dt>
            <dd>{gbLabel(hardware.disk_free_gb)}</dd>
          </div>
          <div>
            <dt>Conversations</dt>
            <dd>
              {#if conversationDir}
                <code class="path">{conversationDir}</code>
              {:else}
                <span class="dim">default under ~/.anyai/</span>
              {/if}
            </dd>
          </div>
        </dl>
        <div class="card-actions">
          <button class="link-btn" onclick={() => setActive("storage")}>
            Manage in Storage →
          </button>
          <button class="link-btn" onclick={() => setActive("models")}>
            Manage in Models →
          </button>
        </div>
      </div>

      <p class="footnote">
        Audio input (mic), audio output (speakers), camera, GPU grouping, and
        CPU/GPU-only modes will surface here as multimodal support lands.
      </p>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede strong { color: #ccc; font-weight: 600; }

  .loading, .error { padding: 2rem; text-align: center; color: #555; font-size: .82rem; }
  .error { color: #d66; }

  .cards { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-height: 0; }
  .group-label {
    font-size: .68rem; color: #666; text-transform: uppercase;
    letter-spacing: .06em; margin: .35rem .15rem -.1rem;
  }
  .group-label:first-child { margin-top: 0; }

  .card {
    border: 1px solid #1e1e1e;
    background: #131318;
    border-radius: 8px;
    padding: .75rem .9rem;
    display: flex; flex-direction: column; gap: .5rem;
  }
  .card-title { font-size: .9rem; font-weight: 600; color: #e8e8e8; }
  .card-meta { font-size: .76rem; color: #888; line-height: 1.5; margin: 0; }

  .info {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: .65rem;
  }
  .info > div { display: flex; flex-direction: column; gap: .2rem; min-width: 0; }
  dt { font-size: .68rem; color: #666; text-transform: uppercase; letter-spacing: .03em; }
  dd { margin: 0; font-size: .82rem; color: #ccc; display: flex; align-items: center; gap: .35rem; flex-wrap: wrap; }
  dd .dim { color: #555; font-size: .74rem; }
  dd code { font-family: monospace; font-size: .76rem; color: #9a7; }

  .badge {
    font-size: .72rem;
    padding: .12rem .5rem;
    border-radius: 4px;
    border: 1px solid;
  }
  .badge.gpu-nvidia { background: #14221a; border-color: #1e3325; color: #6c6; }
  .badge.gpu-amd    { background: #221414; border-color: #331e1e; color: #e88; }
  .badge.gpu-apple  { background: #181822; border-color: #25253a; color: #aab; }
  .badge.gpu-none   { background: #1a1a1a; border-color: #2a2a2a; color: #888; }

  .path {
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    max-width: 100%;
  }

  .card-actions { display: flex; gap: .35rem; flex-wrap: wrap; }
  .link-btn {
    background: none; border: 1px solid #2a2a3a; color: #6e6ef7;
    padding: .35rem .65rem; border-radius: 6px; font-size: .78rem; cursor: pointer;
  }
  .link-btn:hover { background: #1a1a2a; }

  .footnote {
    font-size: .72rem; color: #555; line-height: 1.5;
    padding: .35rem .15rem 0; margin: 0;
  }
</style>
