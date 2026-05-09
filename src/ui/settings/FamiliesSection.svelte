<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getActiveManifest, getActiveProvider, setActiveFamily } from "../../providers";
  import { resolveModel } from "../../manifest";
  import { loadConfig, invalidateConfigCache } from "../../config";
  import type { HardwareProfile, Manifest, ManifestFamily, Mode, OllamaModel } from "../../types";

  let { onChanged } = $props<{ onChanged: () => void }>();

  let manifest = $state<Manifest | null>(null);
  let providerName = $state("");
  let activeFamily = $state("");
  let activeMode = $state<Mode>("text");
  let modeOverrides = $state<Partial<Record<Mode, string | null>>>({});
  let hardware = $state<HardwareProfile | null>(null);
  /** Pulled-tag → size in bytes. Lets us show per-tier disk requirements
   *  when a model is already on disk; unpulled tiers fall back to "—" so the
   *  user can't be surprised by a quietly-downloaded gigabyte. */
  let pulledSizes = $state<Record<string, number>>({});
  let loading = $state(true);

  onMount(load);

  async function load() {
    loading = true;
    try {
      const [m, provider, config, hw, pulled] = await Promise.all([
        getActiveManifest(),
        getActiveProvider(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
        invoke<OllamaModel[]>("ollama_list_models").catch(() => [] as OllamaModel[]),
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
    return (bytes / 1024 / 1024 / 1024).toFixed(1) + " GB";
  }

  /** Modes the family declares tiers for, in canonical order. */
  function modesIn(family: ManifestFamily): Mode[] {
    const order: Mode[] = ["text", "vision", "code", "transcribe"];
    return order.filter((m) => !!family.modes[m]);
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
        From <strong>{providerName}</strong> · pick a model family. Each family
        lists every mode it supports; the highlighted tier is what runs on this
        machine for that mode.
      </p>
    </div>

    <div class="list">
      {#each familyEntries(manifest) as [name, family]}
        {@const isActive = name === activeFamily}
        {@const modes = modesIn(family)}
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

          {#if modes.length === 0}
            <p class="empty-note">This family declares no modes.</p>
          {:else}
            {#each modes as modeName}
              {@const modeSpec = family.modes[modeName]!}
              {@const picked = pickedTag(name, modeName)}
              {@const isActiveCell = isActive && modeName === activeMode}
              <div class="mode-block">
                <div class="mode-head">
                  <span class="mode-name">{modeSpec.label || modeName}</span>
                  {#if isActiveCell}
                    <span class="mode-tag active-mode">your active mode</span>
                  {/if}
                </div>
                <div class="tier-list" aria-label="{family.label} {modeName} tiers">
                  {#each modeSpec.tiers as tier}
                    {@const hit = tier.model === picked}
                    <div class="tier" class:hit class:hit-active={hit && isActiveCell}>
                      <span class="tier-spec">
                        ≥ {tier.min_vram_gb} GB VRAM · ≥ {tier.min_ram_gb ?? 0} GB RAM
                      </span>
                      <span class="tier-model">{tier.model}</span>
                      <span class="tier-size" class:dim={!pulledSizes[tier.model]}>
                        {pulledSizes[tier.model] ? gbLabel(pulledSizes[tier.model]) : "not pulled"}
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

  .mode-block {
    border-top: 1px solid #1e1e1e;
    background: #0f0f14;
  }
  .mode-head {
    display: flex; align-items: center; gap: .55rem;
    padding: .35rem .85rem .25rem;
    font-size: .72rem; color: #777;
    text-transform: uppercase;
    letter-spacing: .05em;
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

  .tier-list {
    display: flex; flex-direction: column;
  }
  .tier {
    display: grid;
    grid-template-columns: 1fr 1fr 70px auto;
    gap: .5rem;
    align-items: center;
    padding: .3rem .85rem;
    font-size: .74rem;
    border-top: 1px solid #181820;
  }
  .tier:first-child { border-top: none; }
  .tier-spec { color: #555; }
  .tier-model { font-family: monospace; color: #aaa; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tier-size { font-family: monospace; color: #888; text-align: right; font-size: .7rem; }
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

  .loading, .empty, .empty-note {
    color: #555; font-size: .82rem; text-align: center; padding: 1rem;
  }
</style>
