<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import FirstRun from "./FirstRun.svelte";
  import Chat from "./Chat.svelte";
  import { loadConfig, updateConfig } from "../config";
  import { getActiveManifest } from "../providers";
  import { resolveModel, pickFamily, familyModes } from "../manifest";
  import { runCleanup } from "../model-lifecycle";
  import { onModeSwap } from "../watcher";
  import type { HardwareProfile, Mode } from "../types";

  let unsubSwap: (() => void) | null = null;

  type View = "loading" | "first-run" | "chat";

  let view = $state<View>("loading");
  let hardware = $state<HardwareProfile | null>(null);
  let activeModel = $state("");
  let activeMode = $state<Mode>("text");
  let activeFamilyName = $state("");
  let supportedModes = $state<Set<Mode>>(new Set(["text", "vision", "code", "transcribe"]));
  let error = $state("");

  /**
   * Modes the active family inside the active manifest actually has tiers
   * for. Falls back to all four before the manifest loads so the bar isn't
   * briefly all-disabled.
   */
  function modesForActiveFamily(
    manifest: Awaited<ReturnType<typeof getActiveManifest>> | null,
    familyName: string,
  ): Set<Mode> {
    if (!manifest) return new Set(["text", "vision", "code", "transcribe"]);
    const picked = pickFamily(manifest, familyName);
    if (!picked) return new Set();
    return familyModes(picked.family);
  }

  onMount(async () => {
    try {
      const [hw, config] = await Promise.all([
        invoke<HardwareProfile>("detect_hardware"),
        loadConfig(),
      ]);
      hardware = hw;
      activeMode = config.active_mode;
      activeFamilyName = config.active_family;

      // Background cleanup of stale models
      runCleanup().catch(() => {});

      const manifest = await getActiveManifest();
      const picked = pickFamily(manifest, config.active_family);
      activeFamilyName = picked?.name ?? manifest.default_family ?? "";
      supportedModes = modesForActiveFamily(manifest, activeFamilyName);
      activeModel = resolveModel(hw, manifest, activeMode, config.mode_overrides, activeFamilyName);

      const ollamaInstalled = await invoke<boolean>("ollama_installed");
      if (!ollamaInstalled) {
        view = "first-run";
      } else {
        // Check if the model needs pulling
        const pulled = await invoke<Array<{ name: string }>>("ollama_list_models");
        const hasCurrent = pulled.some((m) => m.name === activeModel);
        if (!hasCurrent) {
          view = "first-run";
        } else {
          await invoke("ollama_ensure_running");
          view = "chat";
        }
      }

      unsubSwap = await onModeSwap(async (e) => {
        if (!hardware) return;
        if (e.mode !== activeMode) return;
        const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
        activeFamilyName = config.active_family;
        supportedModes = modesForActiveFamily(manifest, activeFamilyName);
        activeModel = resolveModel(hardware, manifest, activeMode, config.mode_overrides, activeFamilyName);
      });
    } catch (e) {
      // Surface the silenced startup error. Without this it's invisible:
      // the catch sets `error` and falls into the chat view with
      // `activeModel = ""`, so Ollama responds "model is required" and
      // there's no clue why. Log it AND show it in the UI banner.
      console.error("AnyAI startup failed:", e);
      error = String(e);
      view = "chat"; // Show chat anyway with whatever we have
    }
  });

  onDestroy(() => {
    unsubSwap?.();
  });

  async function onFirstRunComplete() {
    await invoke("ollama_ensure_running");
    view = "chat";
  }

  async function onModeChange(mode: Mode) {
    activeMode = mode;
    if (!hardware) return;
    const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
    activeFamilyName = config.active_family;
    supportedModes = modesForActiveFamily(manifest, activeFamilyName);
    activeModel = resolveModel(hardware, manifest, mode, config.mode_overrides, activeFamilyName);

    await updateConfig({ active_mode: mode });
  }

  async function onProviderChange() {
    if (!hardware) return;
    const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
    activeFamilyName = config.active_family;
    supportedModes = modesForActiveFamily(manifest, activeFamilyName);
    activeModel = resolveModel(hardware, manifest, activeMode, config.mode_overrides, activeFamilyName);
  }
</script>

<div class="app">
  {#if view === "loading"}
    <div class="splash">
      <div class="spinner"></div>
      <p>Detecting hardware…</p>
    </div>
  {:else if view === "first-run"}
    <FirstRun
      {hardware}
      {activeModel}
      onComplete={onFirstRunComplete}
    />
  {:else}
    {#if error}
      <div class="error-banner">⚠ Startup failed: {error}</div>
    {/if}
    <Chat
      {activeModel}
      {activeMode}
      activeFamily={activeFamilyName}
      {supportedModes}
      {hardware}
      onModeChange={onModeChange}
      onProviderChange={onProviderChange}
    />
  {/if}
</div>

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; }
  :global(body) {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    background: #0f0f0f;
    color: #e8e8e8;
    height: 100vh;
    overflow: hidden;
  }
  .app { height: 100vh; display: flex; flex-direction: column; }
  .error-banner {
    background: #3a1717;
    color: #ffb4b4;
    border-bottom: 1px solid #5a2424;
    padding: .5rem .85rem;
    font-size: .8rem;
    font-family: -apple-system, BlinkMacSystemFont, monospace;
    word-break: break-all;
  }
  .splash {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 1rem;
    color: #888;
  }
  .spinner {
    width: 28px; height: 28px;
    border: 3px solid #333;
    border-top-color: #6e6ef7;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
