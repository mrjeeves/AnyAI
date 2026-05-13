<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getActiveManifest, getActiveProvider, setActiveFamily } from "../../providers";
  import { resolveModel, modeFor, defaultRuntimeFor, tierRuntime } from "../../manifest";
  import { loadConfig, saveConfig, invalidateConfigCache } from "../../config";
  import type {
    HardwareProfile,
    Manifest,
    ManifestFamily,
    ManifestMode,
    ManifestTier,
    Mode,
    ModelRuntime,
    OllamaModel,
  } from "../../types";

  let { onChanged, onClose, initialDetailFamily = null } = $props<{
    onChanged: () => void;
    onClose: () => void;
    /** Optional family name to open directly into the detail view (rather
     *  than the list). Used by the chat StatusBar's "{family} · {model}"
     *  button to drop the user straight into the tier ladder for the
     *  family they're already on, making per-tier Switch / Un-switch
     *  discoverable without an extra click through the list. */
    initialDetailFamily?: string | null;
  }>();

  /** Mirror of `models::ModelInfo` in src-tauri/src/models.rs. The
   *  Family tier ladder calls `asr_models_list` to surface installed
   *  sizes alongside the manifest's declared `disk_mb` estimate. */
  interface ModelInfo {
    name: string;
    kind: string;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  let manifest = $state<Manifest | null>(null);
  let providerName = $state("");
  let activeFamily = $state("");
  let activeMode = $state<Mode>("text");
  let modeOverrides = $state<Partial<Record<Mode, string | null>>>({});
  /** Per-family-per-mode tier overrides loaded from
   *  `config.family_overrides`. Mirrors the resolver precedence: a
   *  set entry here wins over the flat `mode_overrides` and the
   *  hardware-walked tier ladder. Mutated locally on Switch / Un-switch
   *  and re-saved via `saveConfig`. */
  let familyOverrides = $state<Record<string, Partial<Record<Mode, string | null>>>>({});
  let hardware = $state<HardwareProfile | null>(null);
  /** Mirrors `config.auto_cleanup.models`. Drives whether the
   *  switch-tier confirmation modal fires at all. If the user
   *  disabled the Models cleanup pass in Settings → Storage, swapping
   *  the picked tier doesn't strand the previous model, so the modal
   *  is skipped entirely. */
  let cleanupEnabled = $state(false);
  let suppressedFamilies = $state<string[]>([]);
  /** Pulled-tag → size in bytes (Ollama models). Local-runtime models
   *  (ASR / diarize ONNX) live in a separate location and are tracked
   *  via `localSizes` below. */
  let pulledSizes = $state<Record<string, number>>({});
  /** Local-runtime model name → installed size in bytes. Keyed by
   *  bare name (e.g. `moonshine-small-q8`,
   *  `pyannote-seg-3.0+wespeaker-r34`). */
  let localSizes = $state<Record<string, number>>({});
  let loading = $state(true);

  /** When non-null, render the detail page for this family instead of the
   *  list. This is a navigation surface inside the Family tab — back button
   *  takes the user to the list, the side-tabs take them out of the tab.
   *  Seeded from `initialDetailFamily` so a deep-link from the chat
   *  StatusBar lands directly on the tier ladder; the Back button then
   *  walks the user out to the family list as usual. The Settings panel
   *  is mounted fresh each open (`{#if settingsTab}` in Chat.svelte), so
   *  capturing the initial prop value is exactly what we want — we don't
   *  need to track later prop changes.
   */
  // svelte-ignore state_referenced_locally
  let detailFamily = $state<string | null>(initialDetailFamily ?? null);

  /** Tag → in-flight download. Per-tag rather than per-tier so the same
   *  tag appearing in multiple modes / tier ladders shows a spinner
   *  wherever the user can see it. */
  let downloading = $state<Set<string>>(new Set());
  /** Tag → last error from a failed pull. Cleared when a retry starts. */
  let downloadError = $state<Record<string, string>>({});

  /** Switch-tier confirmation modal. Opens on Switch / Un-switch when
   *  the change would actually swap the resolved tag for that
   *  (family, mode) AND auto-cleanup is on AND the family isn't in
   *  the suppression list. `toModel: null` represents an un-switch
   *  (revert to hardware pick); otherwise it's the tier model the
   *  user clicked. `fromModel` is what the resolver returned for
   *  this (family, mode) before the change so the modal can name
   *  the stranded model. */
  let switchConfirm = $state<{
    familyName: string;
    familyLabel: string;
    mode: Mode;
    modeLabel: string;
    fromModel: string;
    toModel: string | null;
  } | null>(null);

  onMount(load);

  async function load() {
    loading = true;
    try {
      const [m, provider, config, hw, pulled, asr, diarize] = await Promise.all([
        getActiveManifest(),
        getActiveProvider(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
        invoke<OllamaModel[]>("ollama_list_models").catch(() => [] as OllamaModel[]),
        invoke<ModelInfo[]>("asr_models_list").catch(() => [] as ModelInfo[]),
        invoke<ModelInfo[]>("diarize_models_list").catch(() => [] as ModelInfo[]),
      ]);
      manifest = m;
      providerName = provider?.name ?? "(none)";
      activeFamily = config.active_family;
      activeMode = config.active_mode;
      modeOverrides = config.mode_overrides;
      familyOverrides = config.family_overrides ?? {};
      hardware = hw;
      cleanupEnabled = config.auto_cleanup?.models !== false;
      suppressedFamilies = [...(config.cleanup_warning_suppressed_families ?? [])];
      const sizes: Record<string, number> = {};
      for (const p of pulled) sizes[p.name] = p.size;
      pulledSizes = sizes;
      const lsizes: Record<string, number> = {};
      for (const m of [...asr, ...diarize]) {
        if (m.installed && m.installed_size_bytes != null) {
          lsizes[m.name] = m.installed_size_bytes;
        }
      }
      localSizes = lsizes;
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
      runtime !== "ollama" ? localSizes[modelName] : pulledSizes[modelName];
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

  /** Hardware-only pick for (family, mode) — passes `{}` as the family
   *  overrides map so we deliberately ignore the user's switch and ask
   *  "what would the resolver pick from the tier ladder alone?" This
   *  is the tag the "Recommended" badge attaches to and the target of
   *  the Un-switch action. */
  function recommendedTagFor(familyName: string, modeName: Mode): string {
    if (!hardware || !manifest) return "";
    return resolveModel(hardware, manifest, modeName, modeOverrides, familyName, {});
  }

  /** Full resolution including family overrides — what the app
   *  actually uses for this (family, mode) right now. The "Switched to"
   *  badge and the modal's "fromModel" both come from here. */
  function effectiveTagFor(familyName: string, modeName: Mode): string {
    if (!hardware || !manifest) return "";
    return resolveModel(
      hardware,
      manifest,
      modeName,
      modeOverrides,
      familyName,
      familyOverrides,
    );
  }

  /** Whether this (family, mode) currently has a user-set tier override.
   *  Drives the Un-switch button visibility and the "switched to"
   *  highlight on the chosen tier. */
  function hasFamilyOverride(familyName: string, modeName: Mode): boolean {
    return !!familyOverrides[familyName]?.[modeName];
  }

  function tierInstalled(runtime: ModelRuntime, tag: string): boolean {
    if (runtime === "ollama") return !!pulledSizes[tag];
    return !!localSizes[tag];
  }

  /** Effective runtime for a tier under a given mode. Mirrors
   *  `tierRuntime` in manifest.ts and falls back through the same
   *  per-tier → mode-level → default chain so the right pull
   *  command lands. */
  function runtimeOfTier(modeSpec: ManifestMode, modeName: Mode, tier: ManifestTier): ModelRuntime {
    return tierRuntime(tier, modeSpec, modeName);
  }

  /** Pull a single tier's model (no override mutation — just the
   *  download). Routes by runtime so Ollama tags go through
   *  `ollama_pull` and local-runtime ASR models go through
   *  `asr_model_pull`. Diarize / sortformer don't have a per-family
   *  pull path yet; the button is hidden for those (see template). */
  async function downloadTier(runtime: ModelRuntime, model: string) {
    if (downloading.has(model)) return;
    downloadError = { ...downloadError, [model]: "" };
    downloading = new Set([...downloading, model]);
    try {
      if (runtime === "ollama") {
        await invoke("ollama_pull", { model });
      } else if (runtime === "moonshine" || runtime === "parakeet") {
        await invoke("asr_model_pull", { name: model });
      } else {
        throw new Error(`Downloads for runtime "${runtime}" are managed elsewhere.`);
      }
      await load();
    } catch (e) {
      downloadError = { ...downloadError, [model]: String(e) };
    } finally {
      const next = new Set(downloading);
      next.delete(model);
      downloading = next;
    }
  }

  /** Persist a per-(family, mode) override. Passing `null` clears the
   *  override (un-switch). Pruning empties so the saved JSON doesn't
   *  carry empty per-family maps once everything's reverted. */
  async function writeFamilyOverride(
    familyName: string,
    mode: Mode,
    model: string | null,
  ): Promise<void> {
    const config = await loadConfig();
    const cur: Partial<Record<Mode, string | null>> = {
      ...(config.family_overrides?.[familyName] ?? {}),
    };
    if (model) cur[mode] = model;
    else delete cur[mode];
    const nextMap = { ...(config.family_overrides ?? {}) };
    if (Object.keys(cur).length > 0) nextMap[familyName] = cur;
    else delete nextMap[familyName];
    config.family_overrides = nextMap;
    await saveConfig(config);
    invalidateConfigCache();
    familyOverrides = config.family_overrides;
    // Notify parent so the chat slot picks up the new resolved model
    // on the next mode switch (or right away if the changed family is
    // the active one and the mode is the active one).
    onChanged?.();
  }

  /** Entry point for clicking a tier's Switch / Un-switch button.
   *  `toModel` is the tier model the user picked, or `null` for
   *  un-switch (revert to the hardware tier). Decides whether to
   *  surface the auto-cleanup confirm modal or just apply the change
   *  directly. */
  function requestTierSwitch(
    familyName: string,
    familyLabel: string,
    mode: Mode,
    modeLabel: string,
    toModel: string | null,
  ) {
    const fromModel = effectiveTagFor(familyName, mode);
    const targetModel = toModel ?? recommendedTagFor(familyName, mode);
    if (!fromModel || fromModel === targetModel) {
      // No-op (e.g. clicking the current effective tier's Switch button
      // never happens because we hide it, but defensive).
      return;
    }
    if (cleanupEnabled && !suppressedFamilies.includes(familyName)) {
      switchConfirm = {
        familyName,
        familyLabel,
        mode,
        modeLabel,
        fromModel,
        toModel,
      };
      return;
    }
    applyTierSwitch(familyName, mode, toModel);
  }

  /** Persist the resolved override change. If the requested target is
   *  the hardware pick we clear the override instead of writing it —
   *  storing the recommended tag would prevent future hardware
   *  upgrades from re-picking a different rung. */
  async function applyTierSwitch(
    familyName: string,
    mode: Mode,
    toModel: string | null,
  ): Promise<void> {
    if (toModel === null) {
      await writeFamilyOverride(familyName, mode, null);
      return;
    }
    const rec = recommendedTagFor(familyName, mode);
    if (toModel === rec) {
      await writeFamilyOverride(familyName, mode, null);
      return;
    }
    await writeFamilyOverride(familyName, mode, toModel);
  }

  async function confirmSwitchPlain() {
    if (!switchConfirm) return;
    const c = switchConfirm;
    switchConfirm = null;
    await applyTierSwitch(c.familyName, c.mode, c.toModel);
  }

  async function confirmSwitchSuppress() {
    if (!switchConfirm) return;
    const c = switchConfirm;
    switchConfirm = null;
    const config = await loadConfig();
    const list = config.cleanup_warning_suppressed_families ?? [];
    if (!list.includes(c.familyName)) {
      config.cleanup_warning_suppressed_families = [...list, c.familyName];
      await saveConfig(config);
      suppressedFamilies = config.cleanup_warning_suppressed_families;
    }
    await applyTierSwitch(c.familyName, c.mode, c.toModel);
  }

  async function confirmSwitchTurnOffCleanup() {
    if (!switchConfirm) return;
    const c = switchConfirm;
    switchConfirm = null;
    const config = await loadConfig();
    // Flips the Models-pass toggle that Settings → Storage exposes.
    // The other sections (transcribe / legacy / updates / conversations)
    // stay where the user left them — this is a per-section disable,
    // not a global kill switch.
    config.auto_cleanup = { ...config.auto_cleanup, models: false };
    await saveConfig(config);
    cleanupEnabled = false;
    await applyTierSwitch(c.familyName, c.mode, c.toModel);
  }

  function cancelSwitch() {
    switchConfirm = null;
  }

  function familyEntries(m: Manifest): Array<[string, ManifestFamily]> {
    return Object.entries(m.families ?? {});
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

    <div class="list scroll-fade">
      {#each familyEntries(manifest) as [name, family]}
        {@const isActive = name === activeFamily}
        {@const effective = effectiveTagFor(name, activeMode)}
        {@const overridden = hasFamilyOverride(name, activeMode)}
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
            {#if effective}
              <p class="row-picked">
                {#if overridden}
                  Switched to <code>{effective}</code>
                {:else}
                  Picks <code>{effective}</code> for your hardware
                {/if}
                {#if pulledSizes[effective] || localSizes[effective]}
                  · <span class="dim">{gbLabel(pulledSizes[effective] ?? localSizes[effective])} on disk</span>
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

      <div class="detail-body scroll-fade">
        {#if modes.length === 0}
          <p class="empty-note">This family declares no modes.</p>
        {:else}
          {#each modes as modeName}
            {@const modeSpec = modeFor(manifest, picked.family, modeName)!}
            {@const recommendedModel = recommendedTagFor(picked.name, modeName)}
            {@const effectiveModel = effectiveTagFor(picked.name, modeName)}
            {@const overridden = hasFamilyOverride(picked.name, modeName)}
            {@const isActiveCell = isActive && modeName === activeMode}
            {@const modeRuntime = modeSpec.runtime ?? defaultRuntimeFor(modeName)}
            {@const shared = isShared(manifest, picked.family, modeName)}
            <div class="mode-block">
              <div class="mode-head">
                <span class="mode-name">{modeSpec.label || modeName}</span>
                <span class="runtime-tag" class:local={modeRuntime !== "ollama"}>
                  {modeRuntime}
                </span>
                {#if shared}
                  <span class="shared-tag" title="Inherited from the manifest's shared_modes block — same ladder for every family unless they override.">shared</span>
                {/if}
                {#if isActiveCell}
                  <span class="mode-tag active-mode">your active mode</span>
                {/if}
                {#if overridden}
                  <button
                    class="mode-revert"
                    onclick={() =>
                      requestTierSwitch(
                        picked.name,
                        picked.family.label,
                        modeName,
                        modeSpec.label || modeName,
                        null,
                      )}
                    title="Clear the override and use the hardware-recommended tier ({recommendedModel || "—"}) for this mode."
                  >
                    ↺ Un-switch
                  </button>
                {/if}
              </div>
              <div class="tier-list" aria-label="{picked.family.label} {modeName} tiers">
                {#each modeSpec.tiers as tier}
                  {@const recommended = tier.model === recommendedModel}
                  {@const current = tier.model === effectiveModel}
                  {@const switched = current && overridden}
                  {@const tierRt = runtimeOfTier(modeSpec, modeName, tier)}
                  {@const sz = tierSize(modeSpec, tier.model, tier.disk_mb)}
                  {@const downloadable = tierRt === "ollama" || tierRt === "moonshine" || tierRt === "parakeet"}
                  {@const isDownloading = downloading.has(tier.model)}
                  {@const dlErr = downloadError[tier.model]}
                  <div
                    class="tier"
                    class:current
                    class:switched
                    class:recommended={recommended && !current}
                    class:hit-active={current && isActiveCell}
                  >
                    <div class="tier-main">
                      <div class="tier-row1">
                        <span class="tier-model">{tier.model}</span>
                        {#if switched}
                          <span class="tier-badge switched-badge" title="You picked this tier as the override for this family + mode.">✓ Switched to</span>
                        {:else if recommended && current}
                          <span class="tier-badge rec-badge" title="The hardware-recommended tier — and what the app is using.">✓ Recommended · in use</span>
                        {:else if recommended}
                          <span class="tier-badge rec-badge soft" title="What the resolver would pick from the tier ladder for your hardware. Click Switch on this row to un-switch back to it.">★ Recommended</span>
                        {/if}
                      </div>
                      <div class="tier-row2">
                        <span class="tier-spec">
                          ≥ {tier.min_vram_gb} GB VRAM · ≥ {tier.min_ram_gb ?? 0} GB RAM
                        </span>
                        <span class="tier-size" class:dim={!sz.installed}>
                          {#if sz.bytes > 0}
                            {gbLabel(sz.bytes)}{#if !sz.installed}<span class="dl-hint"> · not yet downloaded</span>{:else}<span class="ok-hint"> · on disk</span>{/if}
                          {:else}
                            —
                          {/if}
                        </span>
                      </div>
                      {#if dlErr}
                        <div class="tier-err">Download failed: {dlErr}</div>
                      {/if}
                    </div>
                    <div class="tier-actions">
                      {#if downloadable && !sz.installed}
                        <button
                          class="tier-btn"
                          disabled={isDownloading}
                          onclick={() => downloadTier(tierRt, tier.model)}
                          title="Pull this model without switching to it."
                          aria-label="Download {tier.model}"
                        >
                          {#if isDownloading}…{:else}↓ Download{/if}
                        </button>
                      {/if}
                      {#if switched}
                        <button
                          class="tier-btn unswitch-btn"
                          onclick={() =>
                            requestTierSwitch(
                              picked.name,
                              picked.family.label,
                              modeName,
                              modeSpec.label || modeName,
                              null,
                            )}
                          title="Revert to the hardware-recommended tier ({recommendedModel || "—"})."
                        >
                          ↺ Un-switch
                        </button>
                      {:else if !current}
                        <button
                          class="tier-btn switch-btn"
                          onclick={() =>
                            requestTierSwitch(
                              picked.name,
                              picked.family.label,
                              modeName,
                              modeSpec.label || modeName,
                              tier.model,
                            )}
                          title={recommended
                            ? "Switch back to the hardware-recommended tier."
                            : "Use this tier instead of the recommended one for this family + mode."}
                        >
                          ⇄ Switch to
                        </button>
                      {/if}
                    </div>
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

  {#if switchConfirm}
    {@const sc = switchConfirm}
    {@const isUnswitch = sc.toModel === null}
    {@const targetLabel = sc.toModel ?? recommendedTagFor(sc.familyName, sc.mode)}
    <div class="confirm-overlay" onclick={cancelSwitch} role="presentation"></div>
    <div class="confirm" role="dialog" aria-label="Confirm tier switch">
      <h3>
        {#if isUnswitch}
          Un-switch {sc.familyLabel} · {sc.modeLabel}?
        {:else}
          Switch {sc.familyLabel} · {sc.modeLabel} to a different tier?
        {/if}
      </h3>
      <p class="confirm-lead">
        {sc.familyLabel}'s <strong>{sc.modeLabel}</strong> mode is currently using
        <code>{sc.fromModel}</code>.
        {#if isUnswitch}
          Un-switching reverts to the hardware-recommended tier, <code>{targetLabel}</code>.
        {:else}
          You're switching to <code>{targetLabel}</code>.
        {/if}
      </p>
      <p class="confirm-warn">
        <strong>Auto-cleanup is on</strong> for installed models, so
        <code>{sc.fromModel}</code> may be removed later if nothing else
        recommends it.
      </p>
      <p class="confirm-hint">
        You can change auto-cleanup any time in <strong>Settings → Storage</strong>.
      </p>
      <div class="confirm-actions confirm-stack">
        <button class="cs-primary" onclick={confirmSwitchPlain}>
          {isUnswitch ? "Un-switch" : "Switch"}
        </button>
        <button class="cs-secondary" onclick={confirmSwitchSuppress}>
          {isUnswitch ? "Un-switch" : "Switch"} · don't warn for {sc.familyLabel} again
        </button>
        <button class="cs-secondary" onclick={confirmSwitchTurnOffCleanup}>
          Turn off auto-cleanup &amp; {isUnswitch ? "un-switch" : "switch"}
        </button>
        <button class="cs-cancel" onclick={cancelSwitch}>Cancel</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  code { font-family: monospace; font-size: .76rem; color: #aaa; background: #1a1a22; padding: 0 .25rem; border-radius: 3px; }

  /* List view */
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede strong { color: #ccc; font-weight: 600; }
  .list { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .5rem; min-height: 0; --scroll-fade-bg: #111; }
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
    --scroll-fade-bg: #111;
    /* Keep the scroll track always rendered so the user sees there's
     * more below; the WebKit pseudo-elements style it to match the
     * dark theme. The scroll-fade class layers on shadow gradients
     * for a discoverable "more above / below" hint when the OS
     * auto-hides the scrollbar. */
    scrollbar-width: thin;
    scrollbar-color: #2a2a2a transparent;
  }
  .detail-body::-webkit-scrollbar { width: 8px; }
  .detail-body::-webkit-scrollbar-track { background: transparent; }
  .detail-body::-webkit-scrollbar-thumb {
    background: #2a2a2a;
    border-radius: 4px;
  }
  .detail-body::-webkit-scrollbar-thumb:hover { background: #3a3a3a; }

  .mode-block {
    border: 1px solid #1e1e1e;
    background: #0f0f14;
    border-radius: 7px;
    overflow: hidden;
    /* Don't let flex layout in `.detail-body` shrink these blocks — the
     * outer scroll on `.detail-body` should be the only scroll, and
     * `overflow: hidden` here (used for the rounded corners) would
     * otherwise let the flex item shrink to 0 and clip the tier list. */
    flex-shrink: 0;
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
  .runtime-tag.local {
    /* Highlights non-Ollama runtimes (moonshine / parakeet /
       pyannote-diarize / sortformer) so the user can tell at a
       glance that this tier's model lives under ~/.myownllm/models/
       rather than as an Ollama tag. */
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
  .ok-hint { color: #6a6; font-size: .68rem; margin-left: .15rem; }

  .mode-revert {
    margin-left: auto;
    padding: .15rem .55rem;
    background: #1a1a22;
    color: #b3b3ff;
    border: 1px solid #2a2a45;
    border-radius: 5px;
    cursor: pointer;
    font-size: .62rem;
    text-transform: none;
    letter-spacing: 0;
    font-family: inherit;
  }
  .mode-revert:hover { background: #232333; border-color: #3a3a55; color: #c4c4ff; }

  .tier-list { display: flex; flex-direction: column; }
  .tier {
    display: flex;
    align-items: center;
    gap: .6rem;
    padding: .5rem .85rem;
    font-size: .76rem;
    border-top: 1px solid #181820;
  }
  .tier:first-child { border-top: none; }
  .tier-main { flex: 1; display: flex; flex-direction: column; gap: .2rem; min-width: 0; }
  .tier-row1 {
    display: flex; align-items: baseline; gap: .55rem; flex-wrap: wrap;
  }
  .tier-row2 {
    display: flex; align-items: center; gap: .6rem; flex-wrap: wrap;
  }
  .tier-spec { color: #555; font-size: .72rem; }
  .tier-model {
    font-family: monospace; color: #aaa;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    font-size: .82rem;
  }
  .tier-size { font-family: monospace; color: #888; font-size: .72rem; }
  .tier-size.dim { color: #444; font-family: inherit; font-style: italic; }
  .tier-err { color: #f88; font-size: .7rem; margin-top: .15rem; }

  .tier.recommended { background: #15151c; }
  .tier.recommended .tier-spec { color: #777; }
  .tier.recommended .tier-model { color: #d8d8d8; }
  .tier.current {
    background: #16162a;
    border-left: 3px solid #6e6ef7;
  }
  .tier.current .tier-spec { color: #888; }
  .tier.current .tier-model { color: #e8e8e8; font-weight: 600; }
  .tier.switched {
    background: #1a1626;
    border-left: 3px solid #b3b3ff;
  }
  .tier.switched .tier-model { color: #f0eaff; font-weight: 600; }
  .tier.hit-active { background: #181838; }

  .tier-badge {
    font-size: .66rem;
    color: #6e6ef7;
    text-transform: uppercase;
    letter-spacing: .03em;
    padding: 0 .35rem;
    border-radius: 4px;
    font-family: inherit;
    background: transparent;
  }
  .rec-badge { color: #6e6ef7; background: #181828; border: 1px solid #25254a; }
  .rec-badge.soft { color: #555; background: transparent; border-color: transparent; }
  .switched-badge { color: #b3b3ff; background: #1f1a30; border: 1px solid #3a2f55; }

  .tier-actions { display: flex; align-items: center; gap: .35rem; flex-shrink: 0; }
  .tier-btn {
    padding: .3rem .65rem;
    background: #1a1a22;
    color: #d8d8d8;
    border: 1px solid #2a2a3a;
    border-radius: 6px;
    cursor: pointer;
    font-size: .72rem;
    font-family: inherit;
    white-space: nowrap;
  }
  .tier-btn:hover:not(:disabled) { background: #232333; border-color: #3a3a55; }
  .tier-btn:disabled { opacity: .5; cursor: default; }
  .switch-btn { color: #b3b3ff; border-color: #2a2a45; }
  .switch-btn:hover:not(:disabled) { color: #c4c4ff; border-color: #3a3a55; background: #1f1f33; }
  .unswitch-btn { color: #d4a64a; border-color: #3a2f1a; }
  .unswitch-btn:hover:not(:disabled) { color: #e6c068; background: #1f1812; border-color: #4a3a1a; }

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

  .confirm-overlay {
    position: fixed; inset: 0; background: rgba(0, 0, 0, .65); z-index: 30;
  }
  .confirm {
    position: fixed; top: 50%; left: 50%; transform: translate(-50%, -50%);
    width: min(420px, 92vw);
    background: #161616; border: 1px solid #2a2a2a; border-radius: 10px;
    padding: 1rem 1.1rem; z-index: 31;
    box-shadow: 0 12px 40px rgba(0, 0, 0, .6);
  }
  .confirm h3 { font-size: .95rem; font-weight: 600; margin: 0 0 .55rem; color: #e8e8e8; }
  .confirm code {
    font-family: monospace; font-size: .76rem; color: #d8d8d8;
    background: #0d0d12; padding: 0 .3rem; border-radius: 3px;
  }
  .confirm-lead {
    font-size: .8rem; color: #ccc; line-height: 1.5;
    margin: 0 0 .55rem;
  }
  .confirm-warn {
    font-size: .78rem; color: #d8d8d8; line-height: 1.5;
    background: #1f1812; border: 1px solid #3a2c1a; border-radius: 6px;
    padding: .5rem .65rem; margin: 0 0 .55rem;
  }
  .confirm-warn strong { color: #ffd166; }
  .confirm-hint {
    font-size: .73rem; color: #888; line-height: 1.5;
    margin: 0 0 .9rem;
  }
  .confirm-hint strong { color: #b3b3ff; }
  .confirm-stack {
    display: flex; flex-direction: column; gap: .35rem;
    justify-content: stretch;
  }
  .confirm-stack button {
    width: 100%;
    padding: .5rem .75rem;
    border-radius: 7px;
    font-size: .82rem;
    cursor: pointer;
    border: 1px solid transparent;
    text-align: center;
  }
  .cs-primary { background: #6e6ef7; color: #fff; border-color: #6e6ef7; }
  .cs-primary:hover { background: #5a5ae0; }
  .cs-secondary {
    background: #1a1a22; color: #d8d8d8; border-color: #2a2a3a;
  }
  .cs-secondary:hover { background: #232333; border-color: #3a3a55; }
  .cs-cancel {
    background: transparent; color: #888; border-color: transparent;
    margin-top: .1rem;
  }
  .cs-cancel:hover { color: #ccc; background: #1a1a1a; }

  .loading, .empty, .empty-note {
    color: #555; font-size: .82rem; text-align: center; padding: 1rem;
  }
</style>
