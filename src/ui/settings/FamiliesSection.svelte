<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getActiveManifest, getActiveProvider, setActiveFamily } from "../../providers";
  import { resolveModel, modeFor, defaultRuntimeFor } from "../../manifest";
  import { loadConfig, invalidateConfigCache } from "../../config";
  import type {
    HardwareProfile,
    Manifest,
    ManifestFamily,
    ManifestMode,
    Mode,
    OllamaModel,
  } from "../../types";

  let { onChanged, onClose } = $props<{
    onChanged: () => void;
    onClose: () => void;
  }>();

  interface WhisperInfo {
    name: string;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  let manifest = $state<Manifest | null>(null);
  let providerName = $state("");
  let activeFamily = $state("");
  let activeMode = $state<Mode>("text");
  let modeOverrides = $state<Partial<Record<Mode, string | null>>>({});
  let hardware = $state<HardwareProfile | null>(null);
  /** Pulled-tag → size in bytes (Ollama models). Whisper models live in a
   *  separate location and are tracked via `whisperSizes` below. */
  let pulledSizes = $state<Record<string, number>>({});
  /** Whisper model name (e.g. `tiny.en`) → installed size in bytes. */
  let whisperSizes = $state<Record<string, number>>({});
  let loading = $state(true);

  /** When non-null, render the detail page for this family instead of the
   *  list. This is a navigation surface inside the Family tab — back button
   *  takes the user to the list, the side-tabs take them out of the tab. */
  let detailFamily = $state<string | null>(null);

  onMount(load);

  async function load() {
    loading = true;
    try {
      const [m, provider, config, hw, pulled, whisper] = await Promise.all([
        getActiveManifest(),
        getActiveProvider(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
        invoke<OllamaModel[]>("ollama_list_models").catch(() => [] as OllamaModel[]),
        invoke<WhisperInfo[]>("whisper_models_list").catch(() => [] as WhisperInfo[]),
      ]);
      manifest = m;
      providerName = provider?.name ?? "(none)";
      activeFamily = config.active_family;
      activeMode = config.active_mode;
      modeOverrides = config.mode_overrides;
      hardware = hw;
      const sizes: Record<string, number> = {};
      for (const p of pulled) sizes[p.name] = p.size;
      pulledSizes = sizes;
      const wsizes: Record<string, number> = {};
      for (const w of whisper) {
        if (w.installed && w.installed_size_bytes != null) {
          wsizes[w.name] = w.installed_size_bytes;
        }
      }
      whisperSizes = wsizes;
    } finally {
      loading = false;
    }
  }

  /** Resolve actual on-disk size for a (mode, model) pair, returning bytes
   *  if the file is installed, the manifest's declared `disk_mb` * 1MB if
   *  not, or 0 if neither is known. Lets the tier table show real size
   *  when present and the manifest estimate otherwise. */
  function tierSize(modeSpec: ManifestMode, modelName: string, tierDiskMb?: number): {
    bytes: number;
    installed: boolean;
  } {
    const runtime = modeSpec.runtime ?? "ollama";
    const installedBytes =
      runtime === "whisper" ? whisperSizes[modelName] : pulledSizes[modelName];
    if (installedBytes && installedBytes > 0) {
      return { bytes: installedBytes, installed: true };
    }
    if (tierDiskMb && tierDiskMb > 0) {
      return { bytes: tierDiskMb * 1024 * 1024, installed: false };
    }
    return { bytes: 0, installed: false };
  }

  async function activate(name: string) {
    await setActiveFamily(name);
    invalidateConfigCache();
    activeFamily = name;
    onChanged();
    // onChanged closes the settings panel from the parent; this stays a
    // single-call action so the user lands directly in chat with the
    // freshly-activated family.
  }

  function startChatting() {
    onClose();
  }

  function familyEntries(m: Manifest): Array<[string, ManifestFamily]> {
    return Object.entries(m.families ?? {});
  }

  /** Tag the resolver picks for (familyName, modeName) at the current
   *  hardware. Each mode-block highlights its own picked tier; the active
   *  family + active mode pair gets the extra "picked for your hardware"
   *  badge so the user sees exactly what they're running right now. */
  function pickedTag(familyName: string, modeName: Mode): string {
    if (!hardware || !manifest) return "";
    return resolveModel(hardware, manifest, modeName, modeOverrides, familyName);
  }

  function gbLabel(bytes: number): string {
    if (bytes <= 0) return "—";
    const mb = bytes / 1024 / 1024;
    if (mb < 1024) return `${Math.round(mb)} MB`;
    return (mb / 1024).toFixed(1) + " GB";
  }

  /** Modes the family advertises (its own + manifest.shared_modes), in
   *  canonical order. The manifest's shared_modes block contributes
   *  modes (e.g. transcribe) that the family inherits without
   *  redeclaring. */
  function modesIn(m: Manifest, family: ManifestFamily): Mode[] {
    const order: Mode[] = ["text", "vision", "code", "transcribe"];
    return order.filter((mode) => !!modeFor(m, family, mode));
  }

  /** True when a mode block is the shared / inherited one (i.e. comes
   *  from manifest.shared_modes rather than the family's own
   *  declaration). Surfaces in the detail UI as a "shared" badge. */
  function isShared(m: Manifest, family: ManifestFamily, mode: Mode): boolean {
    return !family.modes[mode] && !!m.shared_modes?.[mode];
  }

  function familyOrFirst(name: string | null): { name: string; family: ManifestFamily } | null {
    if (!manifest) return null;
    const entries = familyEntries(manifest);
    if (entries.length === 0) return null;
    if (name) {
      const found = entries.find(([k]) => k === name);
      if (found) return { name: found[0], family: found[1] };
    }
    return { name: entries[0][0], family: entries[0][1] };
  }
</script>

<div class="section">
  {#if loading}
    <p class="loading">Loading…</p>
  {:else if !manifest}
    <p class="empty">No active provider — pick one in the Providers tab.</p>
  {:else if detailFamily === null}
    <!-- LIST VIEW -->
    <div class="head">
      <p class="lede">
        From <strong>{providerName}</strong>. Tap a family to inspect its tiers
        and activate it.
      </p>
    </div>

    <div class="list">
      {#each familyEntries(manifest) as [name, family]}
        {@const isActive = name === activeFamily}
        {@const picked = pickedTag(name, activeMode)}
        <button class="row" class:active={isActive} onclick={() => (detailFamily = name)}>
          <div class="row-main">
            <div class="row-titles">
              <span class="row-title">
                {#if isActive}<span class="check">✓</span>{/if}
                {family.label}
              </span>
              <span class="row-key">{name}</span>
            </div>
            {#if family.description}
              <p class="row-desc">{family.description}</p>
            {/if}
            {#if picked}
              <p class="row-picked">
                Picks <code>{picked}</code> for your hardware
                {#if pulledSizes[picked]}
                  · <span class="dim">{gbLabel(pulledSizes[picked])} on disk</span>
                {:else}
                  · <span class="dim">not pulled</span>
                {/if}
              </p>
            {/if}
          </div>
          <span class="chevron" aria-hidden="true">›</span>
        </button>
      {/each}
      {#if familyEntries(manifest).length === 0}
        <p class="empty">This provider's manifest exposes no families.</p>
      {/if}
    </div>
  {:else}
    <!-- DETAIL VIEW -->
    {@const picked = familyOrFirst(detailFamily)}
    {#if !picked}
      <p class="empty">Family not found.</p>
    {:else}
      {@const isActive = picked.name === activeFamily}
      {@const modes = modesIn(manifest, picked.family)}
      <div class="detail-head">
        <button class="back" onclick={() => (detailFamily = null)} aria-label="Back to families">
          ← Families
        </button>
        <div class="detail-titles">
          <span class="detail-title">
            {#if isActive}<span class="check">✓</span>{/if}
            {picked.family.label}
          </span>
          <span class="detail-key">{picked.name}</span>
        </div>
        {#if picked.family.description}
          <p class="detail-desc">{picked.family.description}</p>
        {/if}
      </div>

      <div class="detail-body">
        {#if modes.length === 0}
          <p class="empty-note">This family declares no modes.</p>
        {:else}
          {#each modes as modeName}
            {@const modeSpec = modeFor(manifest, picked.family, modeName)!}
            {@const pickedModel = pickedTag(picked.name, modeName)}
            {@const isActiveCell = isActive && modeName === activeMode}
            {@const runtime = modeSpec.runtime ?? defaultRuntimeFor(modeName)}
            {@const shared = isShared(manifest, picked.family, modeName)}
            <div class="mode-block">
              <div class="mode-head">
                <span class="mode-name">{modeSpec.label || modeName}</span>
                <span class="runtime-tag" class:whisper={runtime === "whisper"}>
                  {runtime === "whisper" ? "whisper.cpp" : "ollama"}
                </span>
                {#if shared}
                  <span class="shared-tag" title="Inherited from the manifest's shared_modes block — same ladder for every family unless they override.">shared</span>
                {/if}
                {#if isActiveCell}
                  <span class="mode-tag active-mode">your active mode</span>
                {/if}
              </div>
              <div class="tier-list" aria-label="{picked.family.label} {modeName} tiers">
                {#each modeSpec.tiers as tier}
                  {@const hit = tier.model === pickedModel}
                  {@const sz = tierSize(modeSpec, tier.model, tier.disk_mb)}
                  <div class="tier" class:hit class:hit-active={hit && isActiveCell}>
                    <span class="tier-spec">
                      ≥ {tier.min_vram_gb} GB VRAM · ≥ {tier.min_ram_gb ?? 0} GB RAM
                    </span>
                    <span class="tier-model">{tier.model}</span>
                    <span class="tier-size" class:dim={!sz.installed}>
                      {#if sz.bytes > 0}
                        {gbLabel(sz.bytes)}{#if !sz.installed}<span class="dl-hint"> to download</span>{/if}
                      {:else}
                        —
                      {/if}
                    </span>
                    {#if hit && isActiveCell}
                      <span class="tier-badge">picked for your hardware</span>
                    {:else if hit}
                      <span class="tier-badge soft">would pick</span>
                    {:else}
                      <span class="tier-badge-spacer"></span>
                    {/if}
                  </div>
                {/each}
              </div>
            </div>
          {/each}
        {/if}
      </div>

      <div class="detail-footer">
        {#if isActive}
          <button class="primary" onclick={startChatting}>Start Chatting →</button>
        {:else}
          <button class="primary" onclick={() => activate(picked.name)}>
            Activate {picked.family.label}
          </button>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  code { font-family: monospace; font-size: .76rem; color: #aaa; background: #1a1a22; padding: 0 .25rem; border-radius: 3px; }

  /* List view */
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede strong { color: #ccc; font-weight: 600; }
  .list { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .5rem; min-height: 0; }
  .row {
    width: 100%;
    text-align: left;
    background: #131318;
    border: 1px solid #1e1e1e;
    border-radius: 8px;
    padding: .75rem .9rem;
    color: #ccc;
    cursor: pointer;
    display: flex; align-items: center; gap: .75rem;
  }
  .row:hover { background: #181820; border-color: #2a2a2a; }
  .row.active { border-color: #6e6ef7; background: #181828; }
  .row-main { flex: 1; display: flex; flex-direction: column; gap: .2rem; min-width: 0; }
  .row-titles { display: flex; align-items: baseline; gap: .55rem; }
  .row-title { font-size: .92rem; font-weight: 600; color: #e8e8e8; }
  .row-key { font-family: monospace; font-size: .72rem; color: #555; }
  .row-desc { font-size: .76rem; color: #888; line-height: 1.45; }
  .row-picked { font-size: .73rem; color: #888; }
  .row-picked .dim { color: #555; }
  .chevron {
    color: #555;
    font-size: 1.2rem;
    line-height: 1;
    flex-shrink: 0;
  }
  .check { color: #6e6ef7; margin-right: .15rem; }

  /* Detail view */
  .detail-head {
    padding: .65rem 1rem .75rem;
    border-bottom: 1px solid #1e1e1e;
    flex-shrink: 0;
    display: flex; flex-direction: column; gap: .35rem;
  }
  .back {
    align-self: flex-start;
    background: none; border: none;
    color: #6e6ef7; cursor: pointer;
    font-size: .78rem;
    padding: .15rem 0;
  }
  .back:hover { color: #8a8af7; }
  .detail-titles { display: flex; align-items: baseline; gap: .55rem; }
  .detail-title { font-size: 1.05rem; font-weight: 600; color: #e8e8e8; }
  .detail-key { font-family: monospace; font-size: .78rem; color: #666; }
  .detail-desc { font-size: .82rem; color: #999; line-height: 1.5; }

  .detail-body {
    flex: 1; overflow-y: auto; padding: .5rem .75rem 1rem;
    display: flex; flex-direction: column; gap: .6rem;
    min-height: 0;
  }

  .mode-block {
    border: 1px solid #1e1e1e;
    background: #0f0f14;
    border-radius: 7px;
    overflow: hidden;
  }
  .mode-head {
    display: flex; align-items: center; gap: .55rem;
    padding: .4rem .85rem .3rem;
    font-size: .72rem; color: #777;
    text-transform: uppercase;
    letter-spacing: .05em;
    border-bottom: 1px solid #18181f;
  }
  .mode-name { color: #aaa; }
  .mode-tag {
    font-size: .65rem;
    color: #6e6ef7;
    background: #1a1a2a;
    padding: 0 .35rem;
    border-radius: 4px;
    text-transform: none;
    letter-spacing: 0;
  }
  .mode-tag.active-mode { color: #b3b3ff; }
  .runtime-tag {
    font-size: .62rem;
    color: #888;
    background: #1a1a22;
    padding: 0 .35rem;
    border-radius: 4px;
    text-transform: lowercase;
    letter-spacing: 0;
    border: 1px solid #25252f;
    font-family: monospace;
  }
  .runtime-tag.whisper {
    color: #d4a64a;
    border-color: #4a3a1a;
    background: #1f1812;
  }
  .shared-tag {
    font-size: .62rem;
    color: #8a8af0;
    background: #14182a;
    padding: 0 .35rem;
    border-radius: 4px;
    text-transform: lowercase;
    letter-spacing: 0;
    border: 1px solid #1e2545;
    cursor: help;
  }
  .dl-hint { color: #555; font-size: .68rem; margin-left: .15rem; }

  .tier-list { display: flex; flex-direction: column; }
  .tier {
    display: grid;
    grid-template-columns: 1fr 1fr 70px auto;
    gap: .5rem;
    align-items: center;
    padding: .35rem .85rem;
    font-size: .76rem;
    border-top: 1px solid #181820;
  }
  .tier:first-child { border-top: none; }
  .tier-spec { color: #555; }
  .tier-model { font-family: monospace; color: #aaa; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tier-size { font-family: monospace; color: #888; text-align: right; font-size: .72rem; }
  .tier-size.dim { color: #444; font-family: inherit; font-style: italic; }
  .tier.hit { background: #15151c; }
  .tier.hit .tier-spec { color: #777; }
  .tier.hit .tier-model { color: #d8d8d8; font-weight: 600; }
  .tier.hit-active { background: #16162a; }
  .tier.hit-active .tier-model { color: #e8e8e8; }
  .tier-badge {
    font-size: .66rem;
    color: #6e6ef7;
    text-transform: uppercase;
    letter-spacing: .03em;
    text-align: right;
    min-width: 9rem;
  }
  .tier-badge.soft { color: #555; }
  .tier-badge-spacer { min-width: 9rem; }

  .detail-footer {
    flex-shrink: 0;
    padding: .75rem 1rem;
    border-top: 1px solid #1e1e1e;
    background: #0d0d0d;
    display: flex;
    justify-content: flex-end;
  }
  .primary {
    padding: .5rem 1.1rem;
    background: #6e6ef7;
    color: #fff;
    border: none;
    border-radius: 7px;
    cursor: pointer;
    font-size: .85rem;
    font-weight: 500;
  }
  .primary:hover { background: #5a5ae0; }

  .loading, .empty, .empty-note {
    color: #555; font-size: .82rem; text-align: center; padding: 1rem;
  }
</style>
