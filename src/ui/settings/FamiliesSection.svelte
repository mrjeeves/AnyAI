<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getActiveManifest, getActiveProvider, setActiveFamily } from "../../providers";
  import { resolveModel } from "../../manifest";
  import { loadConfig, invalidateConfigCache } from "../../config";
  import type { HardwareProfile, Manifest, ManifestFamily, Mode } from "../../types";

  let { onChanged } = $props<{ onChanged: () => void }>();

  let manifest = $state<Manifest | null>(null);
  let providerName = $state("");
  let activeFamily = $state("");
  let activeMode = $state<Mode>("text");
  let modeOverrides = $state<Partial<Record<Mode, string | null>>>({});
  let hardware = $state<HardwareProfile | null>(null);
  let loading = $state(true);

  onMount(load);

  async function load() {
    loading = true;
    try {
      const [m, provider, config, hw] = await Promise.all([
        getActiveManifest(),
        getActiveProvider(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
      ]);
      manifest = m;
      providerName = provider?.name ?? "(none)";
      activeFamily = config.active_family;
      activeMode = config.active_mode;
      modeOverrides = config.mode_overrides;
      hardware = hw;
    } finally {
      loading = false;
    }
  }

  async function switchFamily(name: string) {
    await setActiveFamily(name);
    invalidateConfigCache();
    activeFamily = name;
    onChanged();
  }

  function familyEntries(m: Manifest): Array<[string, ManifestFamily]> {
    return Object.entries(m.families ?? {});
  }

  /** Tag the resolver picks for THIS family at the current hardware + mode.
   *  Highlighted in the tier list so users can see exactly what they'll run. */
  function recommendedTag(familyName: string, family: ManifestFamily): string {
    if (!hardware || !manifest) return "";
    return resolveModel(hardware, manifest, activeMode, modeOverrides, familyName);
  }

  function tierMatches(tier: { min_vram_gb: number; min_ram_gb?: number; model: string }, recommended: string): boolean {
    return tier.model === recommended;
  }
</script>

<div class="section">
  {#if loading}
    <p class="loading">Loading…</p>
  {:else if !manifest}
    <p class="empty">No active provider — pick one in the Providers tab.</p>
  {:else}
    <div class="head">
      <p class="lede">
        From <strong>{providerName}</strong> · pick a model family. The highlighted
        tier is what runs on this machine for the current mode.
      </p>
    </div>

    <div class="list">
      {#each familyEntries(manifest) as [name, family]}
        {@const recommended = recommendedTag(name, family)}
        {@const isActive = name === activeFamily}
        {@const tiers = family.modes[activeMode]?.tiers ?? family.modes[family.default_mode]?.tiers ?? []}
        <div class="family-card" class:active={isActive}>
          <button class="card-head" onclick={() => switchFamily(name)} title="Use {family.label}">
            <div class="card-titles">
              <span class="card-title">
                {#if isActive}<span class="check">✓</span>{/if}
                {family.label}
              </span>
              <span class="card-key">{name}</span>
            </div>
            {#if family.description}
              <p class="card-desc">{family.description}</p>
            {/if}
          </button>

          {#if tiers.length > 0}
            <div class="tier-list" aria-label="{family.label} tiers">
              {#each tiers as tier}
                <div class="tier" class:hit={tierMatches(tier, recommended)}>
                  <span class="tier-spec">
                    ≥ {tier.min_vram_gb} GB VRAM · ≥ {tier.min_ram_gb ?? 0} GB RAM
                  </span>
                  <span class="tier-model">{tier.model}</span>
                  {#if tierMatches(tier, recommended)}
                    <span class="tier-badge">picked for your hardware</span>
                  {/if}
                </div>
              {/each}
            </div>
          {:else}
            <p class="empty-note">This family has no tiers for mode "{activeMode}".</p>
          {/if}
        </div>
      {/each}
      {#if familyEntries(manifest).length === 0}
        <p class="empty">This provider's manifest exposes no families.</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede strong { color: #ccc; font-weight: 600; }
  .list { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-height: 0; }
  .family-card {
    border: 1px solid #1e1e1e;
    background: #131318;
    border-radius: 8px;
    overflow: hidden;
  }
  .family-card.active {
    border-color: #6e6ef7;
    background: #181828;
  }
  .card-head {
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    padding: .65rem .85rem .55rem;
    cursor: pointer;
    color: #ccc;
    display: flex; flex-direction: column; gap: .2rem;
  }
  .card-head:hover { background: #1a1a22; }
  .card-titles { display: flex; align-items: baseline; gap: .55rem; }
  .card-title { font-size: .92rem; font-weight: 600; color: #e8e8e8; }
  .card-key { font-family: monospace; font-size: .72rem; color: #555; }
  .card-desc { font-size: .76rem; color: #888; line-height: 1.45; }
  .check { color: #6e6ef7; margin-right: .15rem; }
  .tier-list {
    display: flex; flex-direction: column;
    border-top: 1px solid #1e1e1e;
    background: #0f0f14;
  }
  .tier {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: .5rem;
    align-items: center;
    padding: .35rem .85rem;
    font-size: .75rem;
    border-top: 1px solid #181820;
  }
  .tier:first-child { border-top: none; }
  .tier-spec { color: #555; }
  .tier-model { font-family: monospace; color: #aaa; }
  .tier.hit { background: #16162a; }
  .tier.hit .tier-spec { color: #888; }
  .tier.hit .tier-model { color: #e8e8e8; font-weight: 600; }
  .tier-badge {
    font-size: .68rem;
    color: #6e6ef7;
    text-transform: uppercase;
    letter-spacing: .03em;
  }
  .loading, .empty, .empty-note {
    color: #555; font-size: .82rem; text-align: center; padding: 1rem;
  }
</style>
