<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import FirstRun from "./FirstRun.svelte";
  import Chat from "./Chat.svelte";
  import { loadConfig, updateConfig } from "../config";
  import { getActiveManifest } from "../providers";
  import { resolveModel, pickFamily, familyModes } from "../manifest";
  import { runCleanup } from "../model-lifecycle";
  import { onModeSwap } from "../watcher";
  import type { HardwareProfile, Mode } from "../types";

  let unsubSwap: (() => void) | null = null;
  let unsubRemote: UnlistenFn | null = null;
  let heartbeatTimer: ReturnType<typeof setInterval> | null = null;

  /** True when another device is using the UI over the LAN. While true the
   *  local UI is curtained off and a non-dismissable toast is shown — single
   *  user only, so the desktop sits out until the remote disconnects. */
  let remoteActive = $state(false);
  let kicking = $state(false);

  async function kickRemote(disable: boolean) {
    if (kicking) return;
    kicking = true;
    try {
      const status = await invoke<{ remote_active: boolean }>("remote_ui_kick", { disable });
      // The backend already drops remote sessions and refuses heartbeats
      // for KICK_HOLDOFF; surface the resulting flag immediately so the
      // curtain doesn't linger an extra event-loop tick.
      remoteActive = !!status.remote_active;
    } catch (e) {
      console.error("kick failed:", e);
    } finally {
      kicking = false;
    }
  }

  /** Stable per-process session id so the tracker can distinguish multiple
   *  Tauri windows (rare but possible) from the genuine remote browsers. */
  const localSessionId =
    "local-" + Math.random().toString(36).slice(2, 10) + "-" + Date.now().toString(36);

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

      // Local heartbeat + remote-active subscription. Run alongside the chat
      // session: the heartbeat keeps the tracker from misclassifying the
      // local window as gone, and the listener flips the curtain in <1s when
      // a phone hits the LAN URL.
      try {
        await invoke("remote_ui_local_heartbeat", { sessionId: localSessionId });
      } catch {}
      heartbeatTimer = setInterval(() => {
        invoke("remote_ui_local_heartbeat", { sessionId: localSessionId }).catch(() => {});
      }, 5000);
      try {
        unsubRemote = await listen<boolean>("anyai://remote-active-changed", (evt) => {
          remoteActive = !!evt.payload;
        });
        // Seed initial state so we don't need to wait for the first event.
        const status = await invoke<{ remote_active: boolean }>("remote_ui_status");
        remoteActive = !!status.remote_active;
      } catch {}

      unsubSwap = await onModeSwap(async (e) => {
        if (!hardware) return;
        if (e.mode !== activeMode) return;
        const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
        activeFamilyName = config.active_family;
        supportedModes = modesForActiveFamily(manifest, activeFamilyName);
        activeModel = resolveModel(
          hardware,
          manifest,
          activeMode,
          config.mode_overrides,
          activeFamilyName,
        );
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
    unsubRemote?.();
    if (heartbeatTimer) clearInterval(heartbeatTimer);
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
    activeModel = resolveModel(
      hardware,
      manifest,
      activeMode,
      config.mode_overrides,
      activeFamilyName,
    );
  }
</script>

<div class="app" class:curtained={remoteActive}>
  {#if view === "loading"}
    <div class="splash">
      <div class="spinner"></div>
      <p>Detecting hardware…</p>
    </div>
  {:else if view === "first-run"}
    <FirstRun {hardware} {activeModel} onComplete={onFirstRunComplete} />
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
      {onModeChange}
      {onProviderChange}
    />
  {/if}

  {#if remoteActive}
    <!--
      Curtain renders above everything in the app so accidental clicks /
      keystrokes don't reach the chat while a remote device drives it. We
      don't offer multi-user yet, so two people typing into the same chat
      would interleave and silently corrupt history.
    -->
    <div class="remote-curtain" role="dialog" aria-modal="true" aria-label="In use remotely">
      <div class="remote-toast">
        <div class="remote-head">
          <span class="remote-dot"></span>
          <div>
            <div class="remote-title">In use remotely</div>
            <div class="remote-sub">
              Another device on your network is using AnyAI. Single-user, so this window is paused
              until they disconnect.
            </div>
          </div>
        </div>
        <div class="remote-actions">
          <button class="kick" onclick={() => kickRemote(false)} disabled={kicking}>
            Kick
          </button>
          <button class="kick-hide" onclick={() => kickRemote(true)} disabled={kicking}>
            Kick &amp; Hide
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }
  :global(body) {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    background: #0f0f0f;
    color: #e8e8e8;
    height: 100vh;
    overflow: hidden;
  }
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .error-banner {
    background: #3a1717;
    color: #ffb4b4;
    border-bottom: 1px solid #5a2424;
    padding: 0.5rem 0.85rem;
    font-size: 0.8rem;
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
    width: 28px;
    height: 28px;
    border: 3px solid #333;
    border-top-color: #6e6ef7;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Curtain: full-bleed scrim that swallows pointer + keyboard while a
     remote device is driving the UI. Sits above the settings panel too
     so opening Settings → Remote on the desktop doesn't accidentally
     punch through. */
  .remote-curtain {
    position: fixed;
    inset: 0;
    background: rgba(7, 7, 12, 0.82);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    z-index: 9999;
    display: flex;
    align-items: center;
    justify-content: center;
    animation: curtain-in 0.18s ease-out;
  }
  @keyframes curtain-in {
    from {
      opacity: 0;
      backdrop-filter: blur(0);
    }
    to {
      opacity: 1;
    }
  }
  .remote-toast {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    padding: 1rem 1.15rem;
    background: #131320;
    border: 1px solid #2a2a55;
    border-radius: 12px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    color: #e8e8e8;
    max-width: 32rem;
    margin: 1rem;
  }
  .remote-head {
    display: flex;
    align-items: flex-start;
    gap: 0.85rem;
  }
  .remote-actions {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
    flex-wrap: wrap;
  }
  .remote-actions button {
    padding: 0.45rem 0.85rem;
    border-radius: 7px;
    font: inherit;
    font-size: 0.8rem;
    cursor: pointer;
    border: 1px solid;
  }
  .remote-actions button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .remote-actions .kick {
    background: #1a1a2a;
    border-color: #2a2a3a;
    color: #e8e8e8;
  }
  .remote-actions .kick:hover:not(:disabled) {
    background: #22223a;
    border-color: #3a3a55;
  }
  .remote-actions .kick-hide {
    background: #2a1818;
    border-color: #4a2222;
    color: #ffb4b4;
  }
  .remote-actions .kick-hide:hover:not(:disabled) {
    background: #381e1e;
    border-color: #5a2a2a;
  }
  .remote-dot {
    width: 10px;
    height: 10px;
    background: #6e6ef7;
    border-radius: 50%;
    margin-top: 0.35rem;
    box-shadow: 0 0 12px #6e6ef7aa;
    animation: pulse 1.6s ease-in-out infinite;
    flex-shrink: 0;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.55;
      transform: scale(0.85);
    }
  }
  .remote-title {
    font-size: 0.92rem;
    font-weight: 600;
  }
  .remote-sub {
    font-size: 0.78rem;
    color: #9a9ab8;
    margin-top: 0.25rem;
    line-height: 1.5;
  }
</style>
