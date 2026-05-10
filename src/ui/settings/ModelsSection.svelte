<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import {
    getModelStatusWithMeta,
    keepModel,
    unkeepModel,
    setModeOverride,
    pruneNow,
    recomputeRecommendedSet,
    lookupModelUsage,
    type ModelUsage,
  } from "../../model-lifecycle";
  import { getAllManifests } from "../../providers";
  import { loadConfig } from "../../config";
  import type { HardwareProfile, Mode } from "../../types";

  type ModelMeta = Awaited<ReturnType<typeof getModelStatusWithMeta>>[number];

  let models = $state<ModelMeta[]>([]);
  let loading = $state(true);
  let pruning = $state(false);
  let prunedList = $state<string[]>([]);
  let tab = $state<"installed" | "overrides">("installed");

  let hardware = $state<HardwareProfile | null>(null);
  let activeMode = $state<Mode>("text");
  /** Every model tag listed in any tier (model or fallback) of the active
   *  family inside the active provider. These are the rows we lock from
   *  deletion: switching modes within the active family stays cheap because
   *  the user can't accidentally delete a tag they'd need on the next mode
   *  swap. Switching families (CLI or Family tab) recomputes this set. */
  let activeFamilyTags = $state<Set<string>>(new Set());
  /** The active family's display label, surfaced in the row badge so users
   *  can read "active · Gemma 4" instead of decoding their config. */
  let activeFamilyLabel = $state<string>("");
  /** Per-tag list of every (provider, family) pair that lists the tag in
   *  any tier — so a row in family `qwen3` reads "in Qwen 3 family" rather
   *  than the old "in N providers" which lost the family signal entirely. */
  let tagFamilies = $state<Record<string, Array<{ provider: string; familyName: string; familyLabel: string }>>>({});

  let overridePicker = $state<{ mode: Mode; open: boolean } | null>(null);
  let availableModels = $state<string[]>([]);
  let deleteTarget = $state<{
    name: string;
    size: number;
    kept: boolean;
    runtime: "ollama" | "whisper";
  } | null>(null);
  let deleteUsage = $state<ModelUsage | null>(null);
  let deleteUsageLoading = $state(false);
  let deleting = $state(false);
  let deleteError = $state("");

  onMount(async () => {
    try {
      hardware = await invoke<HardwareProfile>("detect_hardware");
    } catch {}
    try {
      const config = await loadConfig();
      activeMode = config.active_mode;
    } catch {}
    await reload();
    try {
      const manifests = await getAllManifests();
      const set = new Set<string>();
      for (const { manifest } of manifests) {
        for (const family of Object.values(manifest.families ?? {})) {
          for (const modeSpec of Object.values(family.modes)) {
            for (const tier of modeSpec.tiers) {
              set.add(tier.model);
              set.add(tier.fallback);
            }
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
    await computeFamilyMembership();
    loading = false;
  }

  /** Walk every saved provider's manifest and bucket every tag into:
   *  (a) the active-family lock set, and
   *  (b) the per-tag family map for row badges. One pass over O(providers
   *  × families × modes × tiers) — cheap enough to redo on every reload. */
  async function computeFamilyMembership() {
    try {
      const [allManifests, config] = await Promise.all([getAllManifests(), loadConfig()]);
      const lockSet = new Set<string>();
      const map: Record<string, Array<{ provider: string; familyName: string; familyLabel: string }>> = {};
      let activeLabel = "";

      for (const { provider, manifest } of allManifests) {
        for (const [familyName, family] of Object.entries(manifest.families ?? {})) {
          const isActiveFam =
            provider.name === config.active_provider && familyName === config.active_family;
          if (isActiveFam) activeLabel = family.label;

          for (const modeSpec of Object.values(family.modes)) {
            for (const tier of modeSpec.tiers) {
              for (const tag of [tier.model, tier.fallback]) {
                if (!tag) continue;
                if (isActiveFam) lockSet.add(tag);
                const list = map[tag] ?? [];
                if (!list.find((e) => e.provider === provider.name && e.familyName === familyName)) {
                  list.push({ provider: provider.name, familyName, familyLabel: family.label });
                }
                map[tag] = list;
              }
            }
          }
        }
      }
      // Mode overrides can point at a tag outside the active family. Whichever
      // tag the resolver picks for the active mode is the "live" model and
      // also belongs in the lock set, even if it doesn't appear in any tier
      // of the active family.
      if (hardware) {
        try {
          const probe = await lookupModelUsage("__probe__", hardware, activeMode);
          if (probe.activeTag) lockSet.add(probe.activeTag);
        } catch {}
      }

      activeFamilyTags = lockSet;
      activeFamilyLabel = activeLabel;
      tagFamilies = map;
    } catch {
      // Non-fatal: the rows will fall back to the unrecommended badge.
    }
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

  async function startDelete(model: ModelMeta) {
    if (!hardware) return;
    deleteTarget = {
      name: model.name,
      size: model.size,
      kept: model.kept,
      runtime: model.runtime,
    };
    deleteUsage = null;
    deleteError = "";
    deleteUsageLoading = true;
    try {
      deleteUsage = await lookupModelUsage(model.name, hardware, activeMode);
    } catch {
      deleteUsage = null;
    } finally {
      deleteUsageLoading = false;
    }
  }

  async function confirmDelete() {
    if (!deleteTarget || deleting) return;
    // Active-family lock is enforced at the row level (no trash button is
    // rendered for those tags). Belt + suspenders here in case state slips.
    if (activeFamilyTags.has(deleteTarget.name)) return;
    deleting = true;
    deleteError = "";
    try {
      // Manual delete trumps the pin: clear the keep flag first so it doesn't
      // resurrect the entry on the next reload.
      if (deleteTarget.kept) {
        try { await unkeepModel(deleteTarget.name); } catch {}
      }
      // Route the delete to the right backend — whisper models live
      // under `~/.anyai/whisper/`, not in Ollama's library.
      if (deleteTarget.runtime === "whisper") {
        await invoke("whisper_model_remove", { name: deleteTarget.name });
      } else {
        await invoke("ollama_delete_model", { name: deleteTarget.name });
      }
      deleteTarget = null;
      deleteUsage = null;
      await reload();
    } catch (e) {
      deleteError = String(e);
    } finally {
      deleting = false;
    }
  }

  function closeDelete() {
    if (deleting) return;
    deleteTarget = null;
    deleteUsage = null;
    deleteError = "";
  }

  /** Cross-family / cross-mode usages worth bolding in the dialog. The
   *  delete is hard-blocked when isActiveTag is true, so the active triple
   *  is filtered out either way and the dialog lists what the user might not
   *  expect — what they'd silently re-pull later if they switch family or
   *  mode after deleting. */
  async function getActiveTriple(): Promise<{ provider: string; family: string; mode: Mode } | null> {
    try {
      const config = await loadConfig();
      return { provider: config.active_provider, family: config.active_family, mode: activeMode };
    } catch {
      return null;
    }
  }
  let activeTriple = $state<{ provider: string; family: string; mode: Mode } | null>(null);
  $effect(() => {
    if (deleteTarget) {
      getActiveTriple().then((t) => (activeTriple = t));
    }
  });
  function otherUses(usage: ModelUsage | null): ModelUsage["uses"] {
    if (!usage || !activeTriple) return usage?.uses ?? [];
    return usage.uses.filter(
      (u) =>
        !(
          u.provider === activeTriple!.provider &&
          u.familyName === activeTriple!.family &&
          u.mode === activeTriple!.mode
        ),
    );
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
          {@const inActive = activeFamilyTags.has(m.name)}
          {@const fams = tagFamilies[m.name] ?? []}
          {@const otherFams = fams.filter((f) => !(inActive && f.familyLabel === activeFamilyLabel))}
          <div class="model-row" class:unrecommended={!inActive && fams.length === 0}>
            <div class="model-info">
              <div class="name-row">
                <span class="name">{m.name}</span>
                <span class="runtime-tag" class:whisper={m.runtime === "whisper"}>
                  {m.runtime === "whisper" ? "whisper.cpp" : "ollama"}
                </span>
              </div>
              <span class="size">{sizeLabel(m.size)}</span>
            </div>
            <div class="model-meta">
              {#if inActive}
                <span class="rec-badge primary" title="Locked — part of the active family">
                  ✓ active · {activeFamilyLabel} family
                </span>
              {:else if fams.length === 1}
                <span class="rec-badge soft">in {fams[0].familyLabel} family</span>
              {:else if fams.length > 1}
                <span class="rec-badge soft">in {fams.length} families</span>
              {:else}
                <span class="unrec-badge">unrecommended · {ageLabel(m.last_recommended)}</span>
              {/if}
              {#if inActive && otherFams.length > 0}
                <span class="rec-meta">
                  also in {otherFams.length === 1 ? `${otherFams[0].familyLabel} family` : `${otherFams.length} other families`}
                </span>
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
            {#if inActive}
              <span
                class="trash-btn locked"
                title="Active family ({activeFamilyLabel}) — switch family to delete"
                aria-hidden="true"
              >🔒</span>
            {:else}
              <button
                class="trash-btn"
                onclick={() => startDelete(m)}
                title="Delete this model"
                aria-label="Delete {m.name}"
              >
                🗑
              </button>
            {/if}
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

  {#if deleteTarget}
    <div
      class="confirm-overlay"
      onclick={closeDelete}
      role="presentation"
    ></div>
    <div class="confirm" role="dialog" aria-label="Delete model">
      <h3>Delete this model?</h3>
      <p class="confirm-name">{deleteTarget.name}</p>
      <p class="confirm-size">Frees {sizeLabel(deleteTarget.size)} of disk space.</p>

      {#if deleteUsageLoading}
        <p class="confirm-info">Checking where this model is used…</p>
      {:else if deleteUsage?.isActiveTag}
        <p class="confirm-error">
          <strong>This is the model currently in use.</strong>
          Switch family or mode first, then delete.
        </p>
      {:else if otherUses(deleteUsage).length > 0}
        <p class="confirm-warn-lead">
          Heads up — this is the recommended model for:
        </p>
        <ul class="confirm-uses">
          {#each otherUses(deleteUsage) as u}
            <li>
              <strong>{u.familyLabel}</strong>
              <span class="use-meta">· {u.mode} mode</span>
              {#if u.provider !== activeTriple?.provider}
                <span class="use-meta">· {u.provider}</span>
              {/if}
            </li>
          {/each}
        </ul>
        <p class="confirm-warn-tail">
          You can still delete it; AnyAI will re-pull when you switch.
        </p>
      {/if}

      {#if deleteTarget.kept}
        <p class="confirm-info">This model is pinned. Deleting will unpin it.</p>
      {/if}

      {#if deleteError}
        <p class="confirm-error">{deleteError}</p>
      {/if}
      <div class="confirm-actions">
        <button class="cancel" disabled={deleting} onclick={closeDelete}>Cancel</button>
        <button
          class="delete"
          disabled={deleting || deleteUsageLoading || deleteUsage?.isActiveTag}
          onclick={confirmDelete}
        >
          {deleting ? "Deleting…" : "Delete"}
        </button>
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
  .name-row { display: flex; align-items: center; gap: .4rem; min-width: 0; }
  .name { font-size: .83rem; font-family: monospace; color: #ccc; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .runtime-tag {
    font-size: .62rem;
    color: #888;
    background: #1a1a22;
    padding: 0 .35rem;
    border-radius: 4px;
    text-transform: lowercase;
    border: 1px solid #25252f;
    font-family: monospace;
    flex-shrink: 0;
  }
  .runtime-tag.whisper {
    color: #d4a64a;
    border-color: #4a3a1a;
    background: #1f1812;
  }
  .size { font-size: .72rem; color: #555; }
  .model-meta { display: flex; flex-direction: column; gap: .15rem; align-items: flex-end; }
  .rec-badge { font-size: .7rem; color: #4a4; }
  .rec-badge.primary {
    color: #b3b3ff;
    background: #1a1a2a;
    padding: .1rem .45rem;
    border-radius: 4px;
    font-weight: 600;
  }
  .rec-badge.soft { color: #777; }
  .rec-meta { font-size: .68rem; color: #555; font-style: italic; }
  .unrec-badge { font-size: .7rem; color: #777; }
  .override-badge { font-size: .68rem; color: #9a7; }
  .pin-btn { background: none; border: none; cursor: pointer; font-size: .9rem; opacity: .5; }
  .pin-btn:hover, .pin-btn.pinned { opacity: 1; }
  .trash-btn { background: none; border: none; cursor: pointer; font-size: .9rem; opacity: .5; }
  .trash-btn:hover { opacity: 1; color: #f66; }
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
  .confirm-overlay {
    position: fixed; inset: 0; background: rgba(0, 0, 0, .65); z-index: 30;
  }
  .confirm {
    position: fixed; top: 50%; left: 50%; transform: translate(-50%, -50%);
    width: min(380px, 90vw);
    background: #161616; border: 1px solid #2a2a2a; border-radius: 10px;
    padding: 1rem 1.1rem; z-index: 31;
    box-shadow: 0 12px 40px rgba(0, 0, 0, .6);
  }
  .confirm h3 { font-size: .9rem; font-weight: 600; margin-bottom: .5rem; }
  .confirm-name {
    font-family: monospace; font-size: .85rem; color: #e8e8e8;
    background: #0d0d0d; padding: .4rem .6rem; border-radius: 5px;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    margin-bottom: .5rem;
  }
  .confirm-size { font-size: .78rem; color: #888; margin-bottom: .85rem; }
  .confirm-info {
    font-size: .75rem; color: #aaa; background: #1a1a22;
    padding: .4rem .6rem; border-radius: 5px; margin-bottom: .6rem;
  }
  .confirm-warn-lead {
    font-size: .78rem; color: #ddd; margin-bottom: .35rem;
  }
  .confirm-warn-tail {
    font-size: .73rem; color: #888; margin-top: .25rem; margin-bottom: .75rem;
    font-style: italic;
  }
  .confirm-uses {
    list-style: none;
    background: #1f1a0d; border: 1px solid #3a2c10;
    border-radius: 6px; padding: .45rem .65rem; margin-bottom: .35rem;
    display: flex; flex-direction: column; gap: .25rem;
  }
  .confirm-uses li { font-size: .8rem; color: #f0d9a0; }
  .confirm-uses li strong { color: #ffd166; font-weight: 700; }
  .confirm-uses .use-meta { color: #a89070; font-weight: 400; margin-left: .15rem; }
  .trash-btn.locked {
    cursor: default;
    opacity: .35;
  }
  .confirm-error {
    font-size: .75rem; color: #f88; background: #2a1a1a;
    padding: .4rem .6rem; border-radius: 5px; margin-bottom: .75rem;
    word-break: break-word;
  }
  .confirm-error strong { color: #ffb3b3; }
  .confirm-actions { display: flex; justify-content: flex-end; gap: .5rem; }
  .confirm-actions button {
    padding: .4rem .9rem; border-radius: 6px; font-size: .8rem;
    cursor: pointer; border: 1px solid transparent;
  }
  .confirm-actions button:disabled { opacity: .5; cursor: default; }
  .confirm-actions .cancel {
    background: #1e1e1e; color: #ccc; border-color: #2a2a2a;
  }
  .confirm-actions .cancel:hover:not(:disabled) { background: #252525; }
  .confirm-actions .delete {
    background: #5a2424; color: #ffd6d6; border-color: #7a3434;
  }
  .confirm-actions .delete:hover:not(:disabled) { background: #6a2c2c; }
</style>
