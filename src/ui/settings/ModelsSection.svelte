<script lang="ts">
  import { onMount } from "svelte";
  import { getModelStatusWithMeta, keepModel, unkeepModel, setModeOverride, pruneNow, recomputeRecommendedSet } from "../../model-lifecycle";
  import { getAllManifests } from "../../providers";
  import { loadConfig } from "../../config";
  import type { Mode } from "../../types";

  type ModelMeta = Awaited<ReturnType<typeof getModelStatusWithMeta>>[number];

  let models = $state<ModelMeta[]>([]);
  let loading = $state(true);
  let pruning = $state(false);
  let prunedList = $state<string[]>([]);
  let tab = $state<"installed" | "overrides">("installed");

  let overridePicker = $state<{ mode: Mode; open: boolean } | null>(null);
  let availableModels = $state<string[]>([]);

  onMount(async () => {
    await reload();
    try {
      const manifests = await getAllManifests();
      const set = new Set<string>();
      for (const { manifest } of manifests) {
        for (const modeSpec of Object.values(manifest.modes)) {
          for (const tier of modeSpec.tiers) {
            set.add(tier.model);
            set.add(tier.fallback);
          }
        }
      }
      availableModels = [...set].sort();
    } catch {}
  });

  async function reload() {
    loading = true;
    // Refresh the recommended-by set against currently saved manifests before
    // reading. Otherwise a model pulled this session — including the one the
    // resolver just picked — keeps showing as "unrecommended" until the next
    // cleanup pass writes the cache.
    try { await recomputeRecommendedSet(); } catch {}
    models = await getModelStatusWithMeta();
    loading = false;
  }

  async function toggleKeep(name: string, kept: boolean) {
    if (kept) await unkeepModel(name);
    else await keepModel(name);
    await reload();
  }

  async function prune() {
    pruning = true;
    prunedList = await pruneNow();
    pruning = false;
    await reload();
  }

  async function setOverride(mode: Mode, model: string | null) {
    await setModeOverride(mode, model);
    overridePicker = null;
    await reload();
  }

  function ageLabel(isoDate: string): string {
    const ms = Date.now() - new Date(isoDate).getTime();
    const hours = Math.floor(ms / 3_600_000);
    if (hours < 1) return "just now";
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }

  function sizeLabel(bytes: number): string {
    return (bytes / 1024 / 1024 / 1024).toFixed(1) + " GB";
  }

  const modes: Mode[] = ["text", "vision", "code", "transcribe"];
</script>

<div class="section">
  <div class="h-tabs">
    <button class:active={tab === "installed"} onclick={() => (tab = "installed")}>Installed</button>
    <button class:active={tab === "overrides"} onclick={() => (tab = "overrides")}>Mode overrides</button>
    {#if tab === "installed"}
      <button class="prune-btn" onclick={prune} disabled={pruning}>
        {pruning ? "Cleaning…" : "Clean up"}
      </button>
    {/if}
  </div>

  {#if tab === "installed"}
    {#if prunedList.length > 0}
      <div class="notice">Removed: {prunedList.join(", ")}</div>
    {/if}
    {#if loading}
      <div class="loading">Loading…</div>
    {:else if models.length === 0}
      <div class="empty">No models pulled yet.</div>
    {:else}
      <div class="list">
        {#each models as m}
          <div class="model-row" class:unrecommended={m.recommended_by.length === 0}>
            <div class="model-info">
              <span class="name">{m.name}</span>
              <span class="size">{sizeLabel(m.size)}</span>
            </div>
            <div class="model-meta">
              {#if m.recommended_by.length > 0}
                <span class="rec-badge">
                  ✓ {m.recommended_by.length === 1 ? m.recommended_by[0] : `${m.recommended_by.length} providers`}
                </span>
              {:else}
                <span class="unrec-badge">unrecommended · {ageLabel(m.last_recommended)}</span>
              {/if}
              {#if m.override_for.length > 0}
                <span class="override-badge">override: {m.override_for.join(", ")}</span>
              {/if}
            </div>
            <button
              class="pin-btn"
              class:pinned={m.kept}
              onclick={() => toggleKeep(m.name, m.kept)}
              title={m.kept ? "Unpin" : "Pin (never clean up)"}
            >
              {m.kept ? "📌" : "📍"}
            </button>
          </div>
        {/each}
      </div>
    {/if}
  {:else}
    <div class="overrides-section">
      {#each modes as mode}
        {#await loadConfig() then config}
          {@const current = config.mode_overrides[mode] ?? null}
          <div class="override-row">
            <span class="mode-label">{mode}</span>
            {#if current}
              <span class="current-override">{current}</span>
              <button class="clear-override" onclick={() => setOverride(mode, null)}>clear</button>
            {:else}
              <span class="using-provider">provider default</span>
            {/if}
            <button class="change-override" onclick={() => (overridePicker = { mode, open: true })}>
              change
            </button>
          </div>
        {/await}
      {/each}
    </div>
  {/if}

  {#if overridePicker}
    <div class="picker-overlay" onclick={() => (overridePicker = null)} role="presentation"></div>
    <div class="picker">
      <div class="picker-header">
        Override for <strong>{overridePicker.mode}</strong>
        <button class="close" onclick={() => (overridePicker = null)}>✕</button>
      </div>
      <div class="picker-list">
        {#each availableModels as tag}
          <button onclick={() => setOverride(overridePicker!.mode, tag)}>{tag}</button>
        {/each}
        {#if availableModels.length === 0}
          <p class="empty">No models from any provider yet.</p>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .h-tabs { display: flex; align-items: center; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; gap: .25rem; padding-right: .5rem; }
  .h-tabs button:not(.prune-btn) {
    padding: .55rem; background: none; border: none; color: #666;
    font-size: .8rem; cursor: pointer; border-bottom: 2px solid transparent;
  }
  .h-tabs button:not(.prune-btn).active { color: #e8e8e8; border-bottom-color: #6e6ef7; }
  .h-tabs button:not(.prune-btn):not(.active):not(.prune-btn) { flex: 0 0 auto; padding-left: 1rem; padding-right: 1rem; }
  .h-tabs button:not(.prune-btn).active { flex: 0 0 auto; padding-left: 1rem; padding-right: 1rem; }
  .h-tabs > button:not(.prune-btn):first-child { padding-left: 1rem; }
  .prune-btn {
    margin-left: auto;
    padding: .3rem .7rem; background: #2a2a2a; border: 1px solid #3a3a3a;
    color: #ccc; border-radius: 6px; font-size: .75rem; cursor: pointer;
  }
  .prune-btn:hover:not(:disabled) { background: #333; }
  .prune-btn:disabled { opacity: .4; cursor: default; }
  .notice {
    padding: .5rem 1rem; background: #1a2a1a; font-size: .78rem; color: #6a6;
    border-bottom: 1px solid #1e1e1e;
  }
  .loading, .empty { padding: 2rem; text-align: center; color: #555; font-size: .85rem; }
  .list { flex: 1; overflow-y: auto; padding: .5rem; display: flex; flex-direction: column; gap: .25rem; min-height: 0; }
  .model-row {
    padding: .5rem .6rem; border-radius: 7px; background: #1a1a1a;
    display: flex; align-items: center; gap: .5rem;
  }
  .model-row.unrecommended { border-left: 3px solid #444; }
  .model-info { flex: 1; display: flex; flex-direction: column; gap: .15rem; min-width: 0; }
  .name { font-size: .83rem; font-family: monospace; color: #ccc; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .size { font-size: .72rem; color: #555; }
  .model-meta { display: flex; flex-direction: column; gap: .15rem; align-items: flex-end; }
  .rec-badge { font-size: .7rem; color: #4a4; }
  .unrec-badge { font-size: .7rem; color: #777; }
  .override-badge { font-size: .68rem; color: #9a7; }
  .pin-btn { background: none; border: none; cursor: pointer; font-size: .9rem; opacity: .5; }
  .pin-btn:hover, .pin-btn.pinned { opacity: 1; }
  .overrides-section {
    padding: .75rem;
    display: flex; flex-direction: column; gap: .5rem;
    overflow-y: auto;
  }
  .override-row {
    display: flex; align-items: center; gap: .5rem; font-size: .8rem;
  }
  .mode-label { width: 80px; color: #888; text-transform: capitalize; }
  .current-override { flex: 1; font-family: monospace; font-size: .75rem; color: #9a7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .using-provider { flex: 1; color: #444; font-style: italic; }
  .clear-override, .change-override {
    background: none; border: none; font-size: .72rem;
    cursor: pointer; border-radius: 4px; padding: .15rem .35rem;
  }
  .clear-override { color: #844; }
  .clear-override:hover { background: #2a1a1a; color: #f66; }
  .change-override { color: #557; }
  .change-override:hover { background: #1a1a2a; color: #6e6ef7; }
  .picker-overlay {
    position: fixed; inset: 0; z-index: 20;
  }
  .picker {
    position: fixed; bottom: 0; right: 0; width: 360px;
    background: #161616; border: 1px solid #2a2a2a; border-radius: 10px 10px 0 0;
    z-index: 21; max-height: 50vh; display: flex; flex-direction: column;
  }
  .picker-header {
    display: flex; align-items: center; justify-content: space-between;
    padding: .75rem 1rem; border-bottom: 1px solid #222; font-size: .85rem; color: #ccc;
  }
  .close { background: none; border: none; color: #666; font-size: 1rem; cursor: pointer; }
  .close:hover { color: #ccc; }
  .picker-list {
    overflow-y: auto; padding: .5rem;
    display: flex; flex-direction: column; gap: .2rem;
  }
  .picker-list button {
    text-align: left; background: none; border: none; color: #aaa;
    font-size: .82rem; font-family: monospace; padding: .4rem .6rem;
    border-radius: 5px; cursor: pointer;
  }
  .picker-list button:hover { background: #1e1e1e; color: #e8e8e8; }
  .picker-list .empty { color: #555; text-align: center; padding: 1rem; font-style: italic; }
</style>
