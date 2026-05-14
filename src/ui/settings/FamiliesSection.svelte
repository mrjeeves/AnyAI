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
  /** Tag → in-flight delete. Mirrors `downloading` so the row can show a
   *  spinner / disabled state while the Tauri delete call is running. */
  let deleting = $state<Set<string>>(new Set());
  /** Tag → last error from a failed delete. Cleared when a retry starts. */
  let deleteError = $state<Record<string, string>>({});

  /** Delete-tier confirmation modal. Opens when the user clicks the
   *  trash button on an installed tier that isn't the family's
   *  hardware-recommended pick and isn't the currently-effective
   *  tier — the safe-to-delete population. `sizeBytes` carries the
   *  on-disk size so the modal can quote how much disk gets freed
   *  rather than just "delete this thing". */
  let deleteConfirm = $state<{
    familyLabel: string;
    modeLabel: string;
    model: string;
    runtime: ModelRuntime;
    sizeBytes: number;
  } | null>(null);

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

  /** Bucket a tier into a user-friendly relative-capability label based on
   *  its position in the family's ladder (top = smartest, bottom =
   *  lightest). End users don't think about "B parameters" or
   *  "MoE vs dense" — they want to know "is this the smart slow one or
   *  the quick light one?" Five buckets covers every ladder length we
   *  ship today (longest is 7 tiers in the Gemma 4 + Qwen lines) without
   *  collapsing to identical labels. */
  function smartnessLabel(index: number, total: number): {
    label: string;
    rank: 1 | 2 | 3 | 4 | 5;
  } {
    if (total <= 1) return { label: "Only option", rank: 3 };
    if (index === 0) return { label: "Most capable", rank: 5 };
    if (index === total - 1) return { label: "Lightest", rank: 1 };
    const ratio = index / (total - 1);
    if (ratio < 0.34) return { label: "Strong", rank: 4 };
    if (ratio < 0.66) return { label: "Balanced", rank: 3 };
    return { label: "Light", rank: 2 };
  }

  /** Per-GPU-class headroom defaults — kept in sync with manifest.ts so
   *  the displayed "Needs ~N GB" matches the resolver's actual threshold
   *  when the manifest doesn't declare a tier-specific
   *  `min_unified_ram_gb`. */
  const DEFAULT_HEADROOM_GB: Record<string, number> = {
    apple: 5,
    none: 2,
    nvidia: 1,
    amd: 1,
  };

  /** User-meaningful memory hint for a tier — one number, in the kind of
   *  memory the user's machine actually uses. Apple Silicon and no-GPU
   *  hosts share RAM with the model, so we show the unified threshold
   *  (already includes OS / paired-ASR headroom). Discrete GPUs show
   *  VRAM (the bytes that matter on those hosts; the system RAM fallback
   *  is a resolver implementation detail the user doesn't need to
   *  see). */
  function memoryHint(tier: ManifestTier): string {
    if (!hardware || !manifest) {
      const fallback = tier.min_unified_ram_gb ?? tier.min_ram_gb ?? tier.min_vram_gb ?? 0;
      return fallback > 0 ? `~${fallback} GB memory` : "any";
    }
    const gpu = hardware.gpu_type;
    const unified = gpu === "apple" || gpu === "none";
    if (unified) {
      const headroom = manifest.headroom_gb?.[gpu] ?? DEFAULT_HEADROOM_GB[gpu] ?? 2;
      const need = tier.min_unified_ram_gb ?? (tier.min_ram_gb ?? 0) + headroom;
      return need > 0 ? `Needs ~${need} GB RAM` : "Runs on tiny machines";
    }
    // Discrete GPU host (nvidia/amd).
    if (tier.min_vram_gb > 0) {
      return `Needs ~${tier.min_vram_gb} GB VRAM`;
    }
    return tier.min_ram_gb ? `Needs ~${tier.min_ram_gb} GB RAM` : "Runs on tiny machines";
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

  /** Opens the delete-confirm modal for a tier. Callers gate this on
   *  "installed && not recommended && not current" so the user can
   *  never delete the model that's keeping the family alive or the
   *  resolver's safety-net pick. */
  function requestDeleteTier(
    familyLabel: string,
    modeLabel: string,
    model: string,
    runtime: ModelRuntime,
    sizeBytes: number,
  ) {
    deleteError = { ...deleteError, [model]: "" };
    deleteConfirm = { familyLabel, modeLabel, model, runtime, sizeBytes };
  }

  function cancelDelete() {
    if (deleteConfirm && deleting.has(deleteConfirm.model)) return;
    deleteConfirm = null;
  }

  /** Carry out the delete after the user confirms. Routes by runtime
   *  the same way ModelsSection's row-level delete does: Ollama tags
   *  go through `ollama_delete_model`, local-runtime ASR models
   *  through `asr_model_remove`. Other runtimes shouldn't reach this
   *  path (the trash button is only shown for the supported set), but
   *  surface a clear error if they do rather than silently no-op. */
  async function confirmDelete() {
    if (!deleteConfirm) return;
    const c = deleteConfirm;
    if (deleting.has(c.model)) return;
    deleteError = { ...deleteError, [c.model]: "" };
    deleting = new Set([...deleting, c.model]);
    try {
      if (c.runtime === "ollama") {
        await invoke("ollama_delete_model", { name: c.model });
      } else if (c.runtime === "moonshine" || c.runtime === "parakeet") {
        await invoke("asr_model_remove", { name: c.model });
      } else {
        throw new Error(`Delete for runtime "${c.runtime}" is managed elsewhere.`);
      }
      deleteConfirm = null;
      await load();
    } catch (e) {
      deleteError = { ...deleteError, [c.model]: String(e) };
    } finally {
      const next = new Set(deleting);
      next.delete(c.model);
      deleting = next;
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
        {@const activeModeSpec = modeFor(manifest, family, activeMode)}
        {@const effectiveTier = activeModeSpec?.tiers.find((t) => t.model === effective)}
        {@const effectiveIdx = activeModeSpec?.tiers.findIndex((t) => t.model === effective) ?? -1}
        {@const effectiveSmart = activeModeSpec && effectiveIdx >= 0
          ? smartnessLabel(effectiveIdx, activeModeSpec.tiers.length)
          : null}
        <button class="row" class:active={isActive} onclick={() => (detailFamily = name)}>
          <div class="row-main">
            <div class="row-titles">
              <span class="row-title">
                {#if isActive}<span class="check">✓</span>{/if}
                {family.label}
              </span>
              {#if effectiveSmart}
                <span class="row-rank rank-{effectiveSmart.rank}">{effectiveSmart.label}</span>
              {/if}
            </div>
            {#if family.description}
              <p class="row-desc">{family.description}</p>
            {/if}
            {#if effective}
              <p class="row-picked">
                {#if overridden}
                  <strong>Switched to</strong>
                {:else}
                  <strong>Recommended for your hardware</strong>
                {/if}
                {#if effectiveTier}
                  · <span class="dim">{memoryHint(effectiveTier)}</span>
                {/if}
                {#if pulledSizes[effective] || localSizes[effective]}
                  · <span class="dim">{gbLabel(pulledSizes[effective] ?? localSizes[effective])} on disk</span>
                {:else if effectiveTier?.disk_mb}
                  · <span class="dim">~{gbLabel(effectiveTier.disk_mb * 1024 * 1024)} to download</span>
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
                {#each modeSpec.tiers as tier, tierIdx}
                  {@const recommended = tier.model === recommendedModel}
                  {@const current = tier.model === effectiveModel}
                  {@const switched = current && overridden}
                  {@const tierRt = runtimeOfTier(modeSpec, modeName, tier)}
                  {@const sz = tierSize(modeSpec, tier.model, tier.disk_mb)}
                  {@const downloadable = tierRt === "ollama" || tierRt === "moonshine" || tierRt === "parakeet"}
                  {@const isDownloading = downloading.has(tier.model)}
                  {@const isDeleting = deleting.has(tier.model)}
                  {@const dlErr = downloadError[tier.model]}
                  {@const delErr = deleteError[tier.model]}
                  {@const smart = smartnessLabel(tierIdx, modeSpec.tiers.length)}
                  {@const memHint = memoryHint(tier)}
                  {@const canDelete = downloadable && sz.installed && !recommended && !current}
                  <div
                    class="tier"
                    class:current
                    class:switched
                    class:recommended={recommended && !current}
                    class:hit-active={current && isActiveCell}
                  >
                    <div class="tier-main">
                      <div class="tier-row1">
                        <span class="tier-rank rank-{smart.rank}" title="Relative capability inside the {picked.family.label} family. Top of the ladder = most capable; bottom = lightest and fastest.">
                          {smart.label}
                        </span>
                        {#if switched}
                          <span class="tier-badge switched-badge" title="You picked this option for this family.">✓ Switched to</span>
                        {:else if recommended && current && sz.installed}
                          <span class="tier-badge rec-badge" title="Best fit for your hardware — and what the app is using.">✓ Recommended · in use</span>
                        {:else if recommended && current}
                          <!-- Resolver's pick but not on disk yet — call
                               that out explicitly so the row's Download
                               button doesn't contradict an "in use" badge. -->
                          <span class="tier-badge rec-badge soft" title="Best fit for your hardware. Click Download to pull it; the app will start using it once it's on disk.">★ Recommended · needs download</span>
                        {:else if recommended}
                          <span class="tier-badge rec-badge soft" title="Best fit for your hardware. Click Switch on this row to revert to it.">★ Recommended</span>
                        {/if}
                      </div>
                      <div class="tier-row2">
                        <span class="tier-mem">{memHint}</span>
                        <span class="tier-sep" aria-hidden="true">·</span>
                        <span class="tier-size" class:dim={!sz.installed}>
                          {#if sz.bytes > 0}
                            {gbLabel(sz.bytes)} {#if !sz.installed}<span class="dl-hint">to download</span>{:else}<span class="ok-hint">on disk</span>{/if}
                          {:else}
                            tiny
                          {/if}
                        </span>
                        <span class="tier-model-tag" title="Internal model tag — for reference only">{tier.model}</span>
                      </div>
                      {#if dlErr}
                        <div class="tier-err">Download failed: {dlErr}</div>
                      {/if}
                      {#if delErr}
                        <div class="tier-err">Delete failed: {delErr}</div>
                      {/if}
                    </div>
                    <div class="tier-actions">
                      {#if canDelete}
                        <button
                          class="tier-btn delete-btn"
                          disabled={isDeleting}
                          onclick={() =>
                            requestDeleteTier(
                              picked.family.label,
                              modeSpec.label || modeName,
                              tier.model,
                              tierRt,
                              sz.bytes,
                            )}
                          title="Free up {gbLabel(sz.bytes)} by removing this model from disk. Re-pulled on demand if you Switch to it later."
                          aria-label="Delete {tier.model}"
                        >
                          {#if isDeleting}…{:else}🗑 Delete{/if}
                        </button>
                      {:else if downloadable && !sz.installed}
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
                      {:else if current && sz.installed}
                        <!-- Steady state: this is the resolver's pick AND
                             on disk. Without this stub the action area
                             went empty after a successful Download —
                             which read as "the button reset the view"
                             instead of "the model is now ready." -->
                        <span class="tier-ready" title="This model is on disk and active for this family.">✓ Installed</span>
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

  {#if deleteConfirm}
    {@const dc = deleteConfirm}
    {@const inFlight = deleting.has(dc.model)}
    <div class="confirm-overlay" onclick={cancelDelete} role="presentation"></div>
    <div class="confirm" role="dialog" aria-label="Confirm model delete">
      <h3>Delete this model?</h3>
      <p class="confirm-lead">
        Removes <code>{dc.model}</code> — the {dc.familyLabel}
        <strong>{dc.modeLabel}</strong> tier — from disk.
      </p>
      <p class="confirm-warn">
        Frees about <strong>{gbLabel(dc.sizeBytes)}</strong>. You can
        re-download it any time by clicking Download on this tier, or
        the app will pull it again automatically if you Switch to it
        later.
      </p>
      <div class="confirm-actions confirm-stack">
        <button class="cs-danger" disabled={inFlight} onclick={confirmDelete}>
          {inFlight ? "Deleting…" : "Delete"}
        </button>
        <button class="cs-cancel" disabled={inFlight} onclick={cancelDelete}>
          Cancel
        </button>
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
  .row-titles { display: flex; align-items: baseline; gap: .55rem; flex-wrap: wrap; }
  .row-title { font-size: .92rem; font-weight: 600; color: #e8e8e8; }
  .row-rank {
    font-size: .65rem;
    text-transform: uppercase;
    letter-spacing: .04em;
    padding: 0 .4rem;
    border-radius: 4px;
    border: 1px solid;
    line-height: 1.5;
  }
  /* Capability ramp: rank-5 (most capable) is the strongest accent;
   * rank-1 (lightest) is the dimmest. Same palette is reused on the
   * detail-view tier rows. */
  .rank-5 { color: #b3b3ff; background: #1a1a2a; border-color: #2a2a55; }
  .rank-4 { color: #a3a8ff; background: #181826; border-color: #28284a; }
  .rank-3 { color: #8888aa; background: #16161e; border-color: #22222e; }
  .rank-2 { color: #777; background: #14141a; border-color: #1d1d24; }
  .rank-1 { color: #666; background: #121218; border-color: #1a1a20; }
  .row-desc { font-size: .76rem; color: #888; line-height: 1.45; }
  .row-picked { font-size: .73rem; color: #888; }
  .row-picked strong { color: #aaa; font-weight: 500; }
  .row-picked .dim { color: #666; }
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
    padding: .55rem .85rem;
    font-size: .76rem;
    border-top: 1px solid #181820;
  }
  .tier:first-child { border-top: none; }
  .tier-main { flex: 1; display: flex; flex-direction: column; gap: .25rem; min-width: 0; }
  .tier-row1 {
    display: flex; align-items: center; gap: .55rem; flex-wrap: wrap;
  }
  .tier-row2 {
    display: flex; align-items: center; gap: .35rem; flex-wrap: wrap;
    font-size: .76rem;
  }
  /* Capability pill — same palette ramp as the family-list `.row-rank`.
   * Uses `.tier-rank.rank-N` to compose with the existing `.rank-N`
   * classes defined above. The pill sits at the head of the row so the
   * user reads "Most capable" / "Lightest" first — that's the question
   * end users actually want answered. */
  .tier-rank {
    font-size: .68rem;
    text-transform: uppercase;
    letter-spacing: .04em;
    padding: .1rem .5rem;
    border-radius: 5px;
    border: 1px solid;
    font-weight: 600;
    line-height: 1.5;
    flex-shrink: 0;
  }
  .tier-mem {
    color: #ccc; font-size: .76rem;
  }
  .tier-sep { color: #444; }
  .tier-size { color: #888; font-size: .74rem; }
  .tier-size.dim { color: #555; font-style: italic; }
  /* Raw model tag (e.g. `gemma4:e4b`) — kept visible so power users and
   * docs can still reference it, but de-emphasised so it doesn't read
   * as the headline. */
  .tier-model-tag {
    font-family: monospace; color: #555; font-size: .68rem;
    margin-left: auto; padding-left: .5rem;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    max-width: 14rem;
  }
  .tier-err { color: #f88; font-size: .7rem; margin-top: .15rem; }

  .tier.recommended { background: #15151c; }
  .tier.recommended .tier-mem { color: #d8d8d8; }
  .tier.current {
    background: #16162a;
    border-left: 3px solid #6e6ef7;
  }
  .tier.current .tier-mem { color: #e8e8e8; font-weight: 500; }
  .tier.switched {
    background: #1a1626;
    border-left: 3px solid #b3b3ff;
  }
  .tier.switched .tier-mem { color: #f0eaff; font-weight: 500; }
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
  .delete-btn { color: #f88; border-color: #3a1f1f; }
  .delete-btn:hover:not(:disabled) { color: #faa; background: #2a1414; border-color: #4a2424; }
  .tier-ready {
    padding: .3rem .55rem;
    font-size: .72rem;
    color: #6c6;
    background: transparent;
    border: 1px solid transparent;
    white-space: nowrap;
  }

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
  .cs-danger { background: #5a2424; color: #ffd6d6; border-color: #7a3434; }
  .cs-danger:hover:not(:disabled) { background: #6a2c2c; }
  .cs-danger:disabled { opacity: .5; cursor: default; }
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
