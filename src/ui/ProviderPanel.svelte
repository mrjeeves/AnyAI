<script lang="ts">
  import { onMount } from "svelte";
  import { getProviders, addProvider, removeProvider, setActiveProvider } from "../providers";
  import { addSource, getSources, fetchSourceCatalog, removeSource } from "../sources";
  import { loadConfig, invalidateConfigCache } from "../config";
  import type { Provider, Source } from "../types";

  let { onClose, onChanged } = $props<{
    onClose: () => void;
    onChanged: () => void;
  }>();

  let providers = $state<Provider[]>([]);
  let sources = $state<Source[]>([]);
  let activeProvider = $state("");
  let tab = $state<"providers" | "sources">("providers");

  // Add provider form
  let newUrl = $state("");
  let newName = $state("");
  let adding = $state(false);
  let addError = $state("");

  // Add source form
  let newSourceUrl = $state("");
  let newSourceName = $state("");
  let addingSource = $state(false);

  // Source browse
  let browsingSource = $state<string | null>(null);
  let browseCatalog = $state<Array<{ name: string; url: string; description?: string; origin?: string }>>([]);
  let browseLoading = $state(false);

  onMount(load);

  async function load() {
    [providers, sources] = await Promise.all([getProviders(), getSources()]);
    const config = await loadConfig();
    activeProvider = config.active_provider;
  }

  function groupByDomain(items: Provider[]): Map<string, Provider[]> {
    const groups = new Map<string, Provider[]>();
    for (const p of items) {
      let domain = "(local)";
      try { domain = new URL(p.url).hostname; } catch {}
      const list = groups.get(domain) ?? [];
      list.push(p);
      groups.set(domain, list);
    }
    return groups;
  }

  async function switchProvider(name: string) {
    await setActiveProvider(name);
    invalidateConfigCache();
    activeProvider = name;
    onChanged();
  }

  async function deleteProvider(name: string) {
    await removeProvider(name);
    await load();
  }

  async function addProviderFromForm() {
    if (!newUrl.trim()) return;
    adding = true;
    addError = "";
    try {
      const name = newName.trim() || new URL(newUrl).hostname;
      await addProvider({ name, url: newUrl.trim(), source: null });
      newUrl = "";
      newName = "";
      await load();
    } catch (e) {
      addError = String(e);
    } finally {
      adding = false;
    }
  }

  async function addSourceFromForm() {
    if (!newSourceUrl.trim()) return;
    addingSource = true;
    try {
      const name = newSourceName.trim() || new URL(newSourceUrl).hostname;
      await addSource({ name, url: newSourceUrl.trim() });
      newSourceUrl = "";
      newSourceName = "";
      await load();
    } finally {
      addingSource = false;
    }
  }

  async function browseSource(source: Source) {
    browsingSource = source.name;
    browseLoading = true;
    try {
      const catalog = await fetchSourceCatalog(source.url);
      browseCatalog = catalog.providers;
    } catch {
      browseCatalog = [];
    } finally {
      browseLoading = false;
    }
  }

  async function addFromCatalog(entry: { name: string; url: string }) {
    await addProvider({ name: entry.name, url: entry.url, source: browsingSource! });
    await load();
  }

  async function deleteSource(name: string) {
    await removeSource(name);
    await load();
  }
</script>

<div class="overlay" onclick={onClose} role="presentation"></div>
<div class="panel">
  <div class="panel-header">
    <h2>Providers & Sources</h2>
    <button class="close" onclick={onClose}>✕</button>
  </div>

  <div class="tabs">
    <button class:active={tab === "providers"} onclick={() => { tab = "providers"; browsingSource = null; }}>Providers</button>
    <button class:active={tab === "sources"} onclick={() => (tab = "sources")}>Sources</button>
  </div>

  {#if tab === "providers"}
    <div class="list">
      {#each [...groupByDomain(providers)] as [domain, group]}
        <div class="domain-group">
          <div class="domain-label">{domain}</div>
          {#each group as p}
            <div class="item" class:active={p.name === activeProvider}>
              <button class="item-name" onclick={() => switchProvider(p.name)}>
                {#if p.name === activeProvider}<span class="check">✓</span>{/if}
                {p.name}
              </button>
              {#if p.name !== activeProvider}
                <button class="icon-btn" onclick={() => deleteProvider(p.name)} title="Remove">✕</button>
              {/if}
            </div>
          {/each}
        </div>
      {/each}
    </div>

    <div class="add-form">
      <input bind:value={newUrl} placeholder="Provider URL" />
      <input bind:value={newName} placeholder="Name (optional)" />
      {#if addError}<p class="err">{addError}</p>{/if}
      <button onclick={addProviderFromForm} disabled={adding || !newUrl.trim()}>
        {adding ? "Adding…" : "Add provider"}
      </button>
    </div>
  {:else}
    {#if browsingSource !== null}
      <div class="browse-header">
        <button class="back" onclick={() => (browsingSource = null)}>← Back</button>
        <span>{browsingSource}</span>
      </div>
      {#if browseLoading}
        <p class="loading">Loading…</p>
      {:else}
        <div class="list">
          {#each browseCatalog as entry}
            <div class="item">
              <div class="item-info">
                <span class="item-name-text">{entry.name}</span>
                {#if entry.description}<span class="desc">{entry.description}</span>{/if}
                {#if entry.origin && entry.origin !== browsingSource}
                  <span class="desc">via {entry.origin}</span>
                {/if}
              </div>
              {#if providers.find((p) => p.name === entry.name)}
                <span class="added">Added</span>
              {:else}
                <button class="icon-btn add" onclick={() => addFromCatalog(entry)}>+</button>
              {/if}
            </div>
          {/each}
          {#if browseCatalog.length === 0}
            <p class="empty-note">No providers in this source.</p>
          {/if}
        </div>
      {/if}
    {:else}
      <div class="list">
        {#each sources as s}
          <div class="item">
            <button class="item-name" onclick={() => browseSource(s)}>{s.name}</button>
            <button class="icon-btn" onclick={() => deleteSource(s.name)} title="Remove">✕</button>
          </div>
        {/each}
        {#if sources.length === 0}
          <p class="empty-note">No sources added.</p>
        {/if}
      </div>
      <div class="add-form">
        <input bind:value={newSourceUrl} placeholder="Source URL" />
        <input bind:value={newSourceName} placeholder="Name (optional)" />
        <button onclick={addSourceFromForm} disabled={addingSource || !newSourceUrl.trim()}>
          {addingSource ? "Adding…" : "Add source"}
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .overlay {
    position: fixed; inset: 0;
    background: rgba(0,0,0,.6);
    z-index: 10;
  }
  .panel {
    position: fixed; right: 0; top: 0; bottom: 0;
    width: 320px;
    background: #111;
    border-left: 1px solid #222;
    z-index: 11;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .panel-header {
    display: flex; align-items: center; justify-content: space-between;
    padding: .75rem 1rem;
    border-bottom: 1px solid #1e1e1e;
  }
  h2 { font-size: .95rem; font-weight: 600; }
  .close { background: none; border: none; color: #666; font-size: 1rem; cursor: pointer; padding: .2rem; }
  .close:hover { color: #ccc; }
  .tabs { display: flex; border-bottom: 1px solid #1e1e1e; }
  .tabs button {
    flex: 1; padding: .6rem; background: none; border: none; color: #666;
    font-size: .82rem; cursor: pointer; border-bottom: 2px solid transparent;
  }
  .tabs button.active { color: #e8e8e8; border-bottom-color: #6e6ef7; }
  .list { flex: 1; overflow-y: auto; padding: .5rem; display: flex; flex-direction: column; gap: .25rem; }
  .domain-group { margin-bottom: .5rem; }
  .domain-label { font-size: .7rem; color: #444; padding: .3rem .5rem; text-transform: uppercase; letter-spacing: .05em; }
  .item {
    display: flex; align-items: center; gap: .4rem;
    padding: .4rem .5rem; border-radius: 6px;
  }
  .item:hover { background: #1a1a1a; }
  .item.active { background: #1a1a2a; }
  .item-name {
    flex: 1; background: none; border: none; color: #ccc;
    font-size: .85rem; text-align: left; cursor: pointer; display: flex; align-items: center; gap: .35rem;
  }
  .item-info { flex: 1; display: flex; flex-direction: column; gap: .15rem; }
  .item-name-text { font-size: .85rem; color: #ccc; }
  .desc { font-size: .73rem; color: #555; }
  .check { color: #6e6ef7; font-size: .8rem; }
  .icon-btn {
    background: none; border: none; color: #444; cursor: pointer;
    padding: .15rem .35rem; border-radius: 4px; font-size: .85rem;
  }
  .icon-btn:hover { background: #2a2a2a; color: #ccc; }
  .icon-btn.add { color: #6e6ef7; }
  .add-form {
    padding: .75rem;
    border-top: 1px solid #1e1e1e;
    display: flex;
    flex-direction: column;
    gap: .4rem;
  }
  input {
    background: #1a1a1a; border: 1px solid #2a2a2a; border-radius: 6px;
    color: #e8e8e8; padding: .45rem .6rem; font-size: .82rem;
  }
  input:focus { outline: none; border-color: #6e6ef7; }
  .add-form button {
    padding: .45rem; background: #6e6ef7; color: #fff; border: none;
    border-radius: 6px; cursor: pointer; font-size: .82rem;
  }
  .add-form button:hover:not(:disabled) { background: #5a5ae0; }
  .add-form button:disabled { opacity: .4; cursor: default; }
  .err { font-size: .75rem; color: #f66; }
  .loading, .empty-note { color: #555; font-size: .82rem; text-align: center; padding: 1rem; }
  .browse-header {
    display: flex; align-items: center; gap: .5rem;
    padding: .5rem .75rem; border-bottom: 1px solid #1e1e1e;
    font-size: .82rem; color: #888;
  }
  .back { background: none; border: none; color: #6e6ef7; cursor: pointer; font-size: .82rem; }
  .added { font-size: .73rem; color: #555; }
</style>
