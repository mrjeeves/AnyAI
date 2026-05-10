<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import FirstRun from "./FirstRun.svelte";
  import Chat from "./Chat.svelte";
  import TranscribeView from "./TranscribeView.svelte";
  import Sidebar from "./Sidebar.svelte";
  import { loadConfig, updateConfig } from "../config";
  import { getActiveManifest } from "../providers";
  import { resolveModelEx, pickFamily, familyModes } from "../manifest";
  import { runCleanup } from "../model-lifecycle";
  import { onModeSwap } from "../watcher";
  import {
    listConversations,
    deleteConversation,
    renameConversation,
    moveConversation,
    createFolder,
    renameFolder,
    deleteFolder,
    getActiveConversationId,
    setActiveConversationId,
    type ConversationMeta,
    type FolderMeta,
  } from "../conversations";
  import { updateUi } from "../update-state.svelte";
  import {
    transcribeUi,
    stopRecording,
    startDrain,
    clearLiveDelta,
    clearAfterPersist,
    type PendingStream,
  } from "./transcribe-state.svelte";
  import { newConversation, saveConversation } from "../conversations";
  import type { HardwareProfile, Mode } from "../types";

  let unsubSwap: (() => void) | null = null;
  let unsubRemote: UnlistenFn | null = null;
  let unsubActiveConv: UnlistenFn | null = null;
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
  /** What the family/tier resolver picks for transcribe with the
   *  current hardware. Stored separately from `activeModel` so we can
   *  pre-pull / re-pull whisper independently of the active mode. */
  let pendingWhisperModel = $state("");
  /** Tag handed to FirstRun when something needs pulling. Set during
   *  onMount based on what's actually missing on disk. */
  let firstRunTextModel = $state("");
  let firstRunWhisperModel = $state("");
  let supportedModes = $state<Set<Mode>>(new Set(["text", "vision", "code", "transcribe"]));
  let error = $state("");

  // Sidebar state. We keep the conversation list at App scope so a fresh
  // conversation created by Chat shows up across remounts.
  let sidebarOpen = $state(true);
  let conversations = $state<ConversationMeta[]>([]);
  let folders = $state<FolderMeta[]>([]);
  let activeConversationId = $state<string | null>(null);
  /** Bumped to ask Chat to create a fresh conversation. Plain counter so
   *  re-clicks of "New chat" still trigger a reset even when the chat is
   *  already empty. */
  let newChatCounter = $state(0);

  /**
   * Skip the next `anyai://active-conversation-changed` event because we
   * just fired the underlying setActive ourselves. Without this every
   * local sidebar click would round-trip through the backend → event →
   * effect and we'd reload state we already just set.
   */
  let suppressNextActiveEvent = false;

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
    return familyModes(manifest, picked.family);
  }

  /** What to display in the status bar / pass downstream as the "active
   *  model". The manifest now declares the runtime per mode, so
   *  transcribe and text both flow through the same resolver — we just
   *  prefix whisper picks so the UI can't confuse `tiny.en` (a whisper
   *  filename) with an Ollama tag. */
  function displayModelFor(
    mode: Mode,
    hw: HardwareProfile,
    manifest: Awaited<ReturnType<typeof getActiveManifest>>,
    config: Awaited<ReturnType<typeof loadConfig>>,
  ): string {
    const r = resolveModelEx(hw, manifest, mode, config.mode_overrides, config.active_family);
    return r.runtime === "whisper" ? `whisper:${r.model}` : r.model;
  }

  async function refreshConversations() {
    const list = await listConversations();
    conversations = list.conversations;
    folders = list.folders;
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
      const activeResolved = resolveModelEx(
        hw,
        manifest,
        activeMode,
        config.mode_overrides,
        activeFamilyName,
      );
      activeModel =
        activeResolved.runtime === "whisper"
          ? `whisper:${activeResolved.model}`
          : activeResolved.model;

      // Always resolve transcribe alongside whatever the user's active
      // mode is — we want the whisper model present too so a switch
      // into transcribe mode "just works" without a separate download
      // flow. Resolved here so FirstRun can pull both in parallel.
      const transcribeResolved = resolveModelEx(
        hw,
        manifest,
        "transcribe",
        config.mode_overrides,
        activeFamilyName,
      );
      pendingWhisperModel =
        transcribeResolved.runtime === "whisper" ? transcribeResolved.model : "";

      // We need both the active text model (Ollama) AND the picked
      // whisper transcribe model on disk. FirstRun pulls whichever is
      // missing in parallel; if everything is already present we skip
      // straight to chat.
      const ollamaInstalled = await invoke<boolean>("ollama_installed");
      const textModelToCheck =
        activeResolved.runtime === "ollama" ? activeResolved.model : "";
      let textPresent = textModelToCheck === ""; // non-Ollama modes don't need it
      if (textModelToCheck && ollamaInstalled) {
        const pulled = await invoke<Array<{ name: string }>>("ollama_list_models");
        textPresent = pulled.some((m) => m.name === textModelToCheck);
      }
      let whisperPresent = pendingWhisperModel === "";
      if (pendingWhisperModel) {
        try {
          const list = await invoke<Array<{ name: string; installed: boolean }>>(
            "whisper_models_list",
          );
          whisperPresent = list.some(
            (m) => m.name === pendingWhisperModel && m.installed,
          );
        } catch {
          whisperPresent = false;
        }
      }

      if (!ollamaInstalled || !textPresent || !whisperPresent) {
        // Only ask FirstRun to pull what's actually missing. Passing
        // empty strings tells FirstRun to skip that side — important
        // because we don't want a "Downloading text" row to flash on
        // screen for a model the user already has on disk.
        firstRunTextModel = textPresent ? "" : (textModelToCheck || resolveModelEx(
          hw,
          manifest,
          "text",
          config.mode_overrides,
          activeFamilyName,
        ).model);
        firstRunWhisperModel = whisperPresent ? "" : pendingWhisperModel;
        view = "first-run";
        console.info(
          "[anyai] first-run: text=%s whisper=%s",
          firstRunTextModel || "(present)",
          firstRunWhisperModel || "(present)",
        );
      } else {
        await invoke("ollama_ensure_running");
        view = "chat";
        kickUpdateCheck();
      }

      // Seed the sidebar early so it's ready when the chat view paints.
      refreshConversations().catch(() => {});

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
          const next = !!evt.payload;
          const wasActive = remoteActive;
          remoteActive = next;
          // The remote browser just disconnected. It may have created /
          // renamed / deleted conversations and may have left the active
          // pointer on a different one — refresh both so the desktop
          // lands on whatever the phone last had open.
          if (wasActive && !next) {
            refreshConversations().catch(() => {});
            getActiveConversationId()
              .then((id) => {
                if (id !== activeConversationId) {
                  // Mark the upcoming setActive as our own so we don't
                  // bounce through the event handler again.
                  suppressNextActiveEvent = true;
                  activeConversationId = id;
                }
              })
              .catch(() => {});
          }
        });
        // Seed initial state so we don't need to wait for the first event.
        const status = await invoke<{ remote_active: boolean }>("remote_ui_status");
        remoteActive = !!status.remote_active;
      } catch {}

      // Pick up active-conversation switches made by the remote (or by
      // any other process holding the same backend pointer). Local-driven
      // switches are filtered via `suppressNextActiveEvent` so they
      // don't trigger a redundant reload.
      try {
        unsubActiveConv = await listen<string | null>(
          "anyai://active-conversation-changed",
          (evt) => {
            if (suppressNextActiveEvent) {
              suppressNextActiveEvent = false;
              return;
            }
            const next = (evt.payload as string | null) ?? null;
            if (next !== activeConversationId) {
              activeConversationId = next;
              if (next === null) newChatCounter += 1;
              refreshConversations().catch(() => {});
            }
          },
        );
        // Restore the last active conversation on launch — feels nicer
        // than always landing on an empty New chat surface.
        const lastActive = await getActiveConversationId();
        if (lastActive) {
          suppressNextActiveEvent = true;
          activeConversationId = lastActive;
        }
      } catch {}

      unsubSwap = await onModeSwap(async (e) => {
        if (!hardware) return;
        if (e.mode !== activeMode) return;
        const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
        activeFamilyName = config.active_family;
        supportedModes = modesForActiveFamily(manifest, activeFamilyName);
        activeModel = displayModelFor(activeMode, hardware, manifest, config);
      });

      // After everything else is wired, see if a previous AnyAI process
      // left a transcribe buffer behind. Fire-and-forget — failure
      // shouldn't block the app from coming up.
      probeAndResumeBacklog().catch(() => {});
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
    unsubActiveConv?.();
    if (heartbeatTimer) clearInterval(heartbeatTimer);
  });

  async function onFirstRunComplete() {
    await invoke("ollama_ensure_running");
    view = "chat";
    kickUpdateCheck();
  }

  /**
   * Background probe for an available update right after the chat view
   * paints. We hit `update_status` first (purely local — reads the staged
   * marker on disk) so a relaunch with an already-staged update lights up
   * the Settings dot without a network round-trip. Only if nothing is
   * staged do we ask `update_check_now` to talk to GitHub.
   *
   * Result lands in `updateUi.available`, which the StatusBar's settings
   * button and the SettingsPanel's Updates tab both watch. We deliberately
   * never modal the user — they get a quiet attention dot they can act on
   * when they're ready.
   */
  let updateCheckStarted = false;
  function kickUpdateCheck() {
    if (updateCheckStarted) return;
    updateCheckStarted = true;
    void runUpdateCheck();
  }

  async function runUpdateCheck() {
    try {
      type Pending = { version: string; staged_at: string };
      const status = await invoke<{ pending: Pending | null; install_kind: string; enabled: boolean }>(
        "update_status",
      );
      if (status.pending) {
        updateUi.available = { version: status.pending.version };
        return;
      }
      // Nothing staged → ask GitHub. Skip for package-manager installs and
      // when self-update is disabled, since check_now will just bail and
      // we don't want a phantom dot either way.
      if (!status.enabled || status.install_kind === "package_manager") return;

      type CheckOutcome =
        | { kind: "disabled" }
        | { kind: "package_manager" }
        | { kind: "up_to_date"; current: string; latest: string }
        | { kind: "staged"; version: string }
        | { kind: "policy_blocked"; current: string; latest: string; policy: string };

      const outcome = await invoke<CheckOutcome>("update_check_now");
      if (outcome.kind === "staged") {
        updateUi.available = { version: outcome.version };
      } else if (outcome.kind === "policy_blocked") {
        // Auto-apply policy refused the jump — surface the dot so the user
        // can find it in Settings; the Updates tab itself explains what
        // they need to change to permit the upgrade.
        updateUi.available = { version: outcome.latest };
      }
    } catch (e) {
      // Network failures, GitHub rate limits, etc. — not worth disturbing
      // the user. The watcher's periodic tick will retry later.
      console.warn("startup update check skipped:", e);
    }
  }

  async function onModeChange(mode: Mode) {
    activeMode = mode;
    if (!hardware) return;
    const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
    activeFamilyName = config.active_family;
    supportedModes = modesForActiveFamily(manifest, activeFamilyName);
    activeModel = displayModelFor(mode, hardware, manifest, config);

    await updateConfig({ active_mode: mode });
    ensureWhisperPresent(hardware, manifest, config);
  }

  async function onProviderChange() {
    if (!hardware) return;
    const [config, manifest] = await Promise.all([loadConfig(), getActiveManifest()]);
    activeFamilyName = config.active_family;
    supportedModes = modesForActiveFamily(manifest, activeFamilyName);
    activeModel = displayModelFor(activeMode, hardware, manifest, config);
    ensureWhisperPresent(hardware, manifest, config);
  }

  /** Background-pull the family-resolved whisper model if it isn't on
   *  disk yet. Fire-and-forget so the user can keep using text mode
   *  while the whisper download runs; the next switch into transcribe
   *  mode lands on a ready model instead of erroring out. */
  function ensureWhisperPresent(
    hw: HardwareProfile,
    manifest: Awaited<ReturnType<typeof getActiveManifest>>,
    config: Awaited<ReturnType<typeof loadConfig>>,
  ) {
    const r = resolveModelEx(hw, manifest, "transcribe", config.mode_overrides, activeFamilyName);
    if (r.runtime !== "whisper" || !r.model) return;
    invoke<Array<{ name: string; installed: boolean }>>("whisper_models_list")
      .then((list) => {
        const installed = list.some((m) => m.name === r.model && m.installed);
        if (installed) return;
        console.info("[anyai] background whisper pull: %s", r.model);
        invoke("whisper_model_pull", { name: r.model }).catch((e) => {
          console.warn("[anyai] background whisper pull failed:", e);
        });
      })
      .catch(() => {});
  }

  function onSelectConversation(id: string) {
    if (activeConversationId === id) return;
    activeConversationId = id;
    suppressNextActiveEvent = true;
    setActiveConversationId(id);
  }

  function onNewConversation() {
    activeConversationId = null;
    newChatCounter += 1;
    suppressNextActiveEvent = true;
    setActiveConversationId(null);
  }

  async function onRenameConversation(id: string, title: string) {
    await renameConversation(id, title);
    await refreshConversations();
  }

  async function onDeleteConversation(id: string) {
    await deleteConversation(id);
    if (activeConversationId === id) {
      activeConversationId = null;
      newChatCounter += 1;
      suppressNextActiveEvent = true;
      setActiveConversationId(null);
    }
    await refreshConversations();
  }

  async function onMoveConversation(id: string, folder: string) {
    await moveConversation(id, folder);
    await refreshConversations();
  }

  async function onCreateFolder(path: string) {
    await createFolder(path);
    await refreshConversations();
  }

  async function onRenameFolder(oldPath: string, newPath: string) {
    await renameFolder(oldPath, newPath);
    await refreshConversations();
  }

  async function onDeleteFolder(path: string) {
    await deleteFolder(path);
    await refreshConversations();
  }

  function onConversationChanged(id: string) {
    if (activeConversationId !== id) {
      activeConversationId = id;
      suppressNextActiveEvent = true;
      setActiveConversationId(id);
    }
    refreshConversations().catch(() => {});
  }

  // ---------------------------------------------------------------------
  // Persistent transcription — App-level confirm dialog + auto-resume.
  // The StatusBar's stop button calls into here so the dialog is mounted
  // outside Chat / TranscribeView and survives mode switches.
  // ---------------------------------------------------------------------

  /** Set when the user clicks the stop button on the recording chip.
   *  When the chunk backlog is non-zero, we hold the user in this
   *  dialog until they decide whether to drain or discard the rest. */
  let stopConfirm = $state<{
    pendingChunks: number;
    paused: boolean;
    drainOnly: boolean;
  } | null>(null);

  function requestStopTranscribe() {
    if (!transcribeUi.active) return;
    stopConfirm = {
      pendingChunks: transcribeUi.pendingChunks,
      paused: transcribeUi.paused,
      drainOnly: transcribeUi.drainOnly,
    };
  }

  function jumpToTranscribe() {
    if (activeMode !== "transcribe") {
      onModeChange("transcribe").catch(() => {});
    }
    // If the active conversation isn't the one being recorded into, hop
    // to it so the user actually sees the live text.
    const recId = transcribeUi.conversationId;
    if (recId && recId !== activeConversationId) {
      activeConversationId = recId;
      suppressNextActiveEvent = true;
      setActiveConversationId(recId);
    }
  }

  async function confirmStopTranscribe(): Promise<void> {
    stopConfirm = null;
    await stopRecording();
    // Best-effort: clear the live delta so the next session starts
    // from a clean slate. The view that owns the conversation has
    // already flushed any text it cared about.
    clearLiveDelta();
    clearAfterPersist();
  }

  function cancelStopTranscribe() {
    stopConfirm = null;
  }

  /** Scan `~/.anyai/transcribe-buffer/` for chunks left over by a
   *  previous (crashed / force-quit) AnyAI process and offer to drain
   *  them. We only auto-start the drain — we don't resurrect a mic
   *  stream — so the user always sees a chip in the status bar with a
   *  clear "Recovering…" label, never silent reactivation. */
  let recoveryProbeStarted = false;
  async function probeAndResumeBacklog() {
    if (recoveryProbeStarted) return;
    recoveryProbeStarted = true;
    try {
      const pending = await invoke<PendingStream[]>("transcribe_pending_streams");
      // Pick the largest backlog — multiple orphans are possible if the
      // app crashed twice without cleanup, but only one drain runs at
      // a time. The others stay on disk and the user can clear them
      // from the Storage tab.
      const target = pending
        .filter((p) => p.pending_chunks > 0 && p.model)
        .sort((a, b) => b.pending_chunks - a.pending_chunks)[0];
      if (!target || !target.model) return;

      // Mint a "Recovered transcript" conversation so the drained text
      // has somewhere to land. We don't try to merge into a previous
      // conversation — there's no way to know which one was open when
      // the buffer was written.
      const conv = newConversation("transcribe", `whisper:${target.model}`, activeFamilyName || "");
      conv.title = `Recovered transcript ${new Date().toLocaleString()}`.slice(0, 80);
      await saveConversation(conv);
      await refreshConversations();

      console.info(
        "[anyai] resuming transcript backlog: stream=%s pending=%d model=%s",
        target.stream_id,
        target.pending_chunks,
        target.model,
      );
      await startDrain({
        streamId: target.stream_id,
        model: target.model,
        conversationId: conv.id,
      });
    } catch (e) {
      console.warn("[anyai] backlog probe failed:", e);
    }
  }
</script>

<div class="app" class:curtained={remoteActive}>
  {#if view === "loading"}
    <div class="splash">
      <div class="spinner"></div>
      <p>Detecting hardware…</p>
    </div>
  {:else if view === "first-run"}
    <FirstRun
      {hardware}
      activeModel={firstRunTextModel}
      whisperModel={firstRunWhisperModel}
      onComplete={onFirstRunComplete}
    />
  {:else}
    {#if error}
      <div class="error-banner">⚠ Startup failed: {error}</div>
    {/if}
    <div class="layout">
      <Sidebar
        open={sidebarOpen}
        items={conversations}
        folders={folders}
        activeId={activeConversationId}
        mode={activeMode}
        onSelect={onSelectConversation}
        onNew={onNewConversation}
        onRename={onRenameConversation}
        onDelete={onDeleteConversation}
        onMove={onMoveConversation}
        onCreateFolder={onCreateFolder}
        onRenameFolder={onRenameFolder}
        onDeleteFolder={onDeleteFolder}
        onClose={() => (sidebarOpen = false)}
      />
      {#if activeMode === "transcribe"}
        <TranscribeView
          {activeModel}
          {activeMode}
          activeFamily={activeFamilyName}
          {supportedModes}
          {hardware}
          {sidebarOpen}
          conversationId={activeConversationId}
          {newChatCounter}
          onToggleSidebar={() => (sidebarOpen = !sidebarOpen)}
          onModeChange={onModeChange}
          onProviderChange={onProviderChange}
          onConversationChanged={onConversationChanged}
          onNewSession={onNewConversation}
          onRequestStopTranscribe={requestStopTranscribe}
        />
      {:else}
        <Chat
          {activeModel}
          {activeMode}
          activeFamily={activeFamilyName}
          {supportedModes}
          {hardware}
          {sidebarOpen}
          conversationId={activeConversationId}
          {newChatCounter}
          onToggleSidebar={() => (sidebarOpen = !sidebarOpen)}
          onModeChange={onModeChange}
          onProviderChange={onProviderChange}
          onConversationChanged={onConversationChanged}
          onRequestStopTranscribe={requestStopTranscribe}
          onJumpToTranscribe={jumpToTranscribe}
        />
      {/if}
    </div>
  {/if}

  {#if stopConfirm}
    {@const c = stopConfirm}
    <div class="confirm-overlay" onclick={cancelStopTranscribe} role="presentation"></div>
    <div class="stop-confirm" role="dialog" aria-label="Stop transcription">
      <h3>Stop transcription?</h3>
      {#if c.drainOnly}
        <p class="stop-meta">
          This is a recovery drain — stopping discards
          <strong>{c.pendingChunks * 5} s</strong> of un-transcribed audio
          ({c.pendingChunks} {c.pendingChunks === 1 ? "chunk" : "chunks"})
          left over from a previous session.
        </p>
      {:else if c.pendingChunks > 0}
        <p class="stop-meta">
          Whisper is still
          <strong>{c.pendingChunks * 5} s</strong> behind realtime
          ({c.pendingChunks} {c.pendingChunks === 1 ? "chunk" : "chunks"} pending).
          Stopping now drops that audio without transcribing it.
        </p>
        <p class="stop-hint">
          To finish without losing it, pause the mic and let the backlog
          drain — the transcript chip stays in the bar until it's empty.
        </p>
      {:else}
        <p class="stop-meta">
          The session will end and the final transcript will be saved.
        </p>
      {/if}
      <div class="stop-actions">
        <button class="cancel" onclick={cancelStopTranscribe}>Cancel</button>
        <button class="confirm-stop" onclick={confirmStopTranscribe}>
          {c.pendingChunks > 0 ? "Stop & discard backlog" : "Stop"}
        </button>
      </div>
    </div>
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
  /* Always-on dark scrollbars. macOS overlay scrollbars hide by default,
     which made the Settings → Hardware list look like it ended at the
     viewport. We size the thumb up + brighten it so users can see at a
     glance that a panel is scrollable; settings panes also opt into
     `overflow-y: scroll` so the lane is reserved even when the thumb is
     fully covering the viewport. */
  :global(*) {
    scrollbar-width: auto;
    scrollbar-color: #6a6a85 #1a1a1a;
  }
  :global(*::-webkit-scrollbar) {
    width: 14px;
    height: 14px;
  }
  :global(*::-webkit-scrollbar-track) {
    background: #1a1a1a;
    border-left: 1px solid #242428;
  }
  :global(*::-webkit-scrollbar-thumb) {
    background: #6a6a85;
    border-radius: 7px;
    border: 1px solid #1a1a1a;
    min-height: 36px;
  }
  :global(*::-webkit-scrollbar-thumb:hover) {
    background: #6e6ef7;
  }
  :global(*::-webkit-scrollbar-corner) {
    background: #1a1a1a;
  }
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .layout {
    flex: 1;
    display: flex;
    min-height: 0;
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
  .confirm-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, .55);
    z-index: 9000;
  }
  .stop-confirm {
    position: fixed;
    top: 50%; left: 50%;
    transform: translate(-50%, -50%);
    width: min(420px, 90vw);
    background: #161616;
    border: 1px solid #2a2a2a;
    border-radius: 10px;
    padding: 1.1rem 1.2rem;
    z-index: 9001;
    box-shadow: 0 18px 48px rgba(0, 0, 0, .65);
  }
  .stop-confirm h3 {
    font-size: .95rem; font-weight: 600;
    color: #e8e8e8;
    margin-bottom: .65rem;
  }
  .stop-confirm .stop-meta {
    font-size: .82rem; color: #ccc; line-height: 1.55;
    margin-bottom: .55rem;
  }
  .stop-confirm .stop-meta strong { color: #ffd166; }
  .stop-confirm .stop-hint {
    font-size: .76rem; color: #888;
    line-height: 1.5;
    background: #131318;
    padding: .5rem .65rem;
    border-radius: 6px;
    margin-bottom: .85rem;
  }
  .stop-actions {
    display: flex; justify-content: flex-end; gap: .55rem;
    margin-top: .25rem;
  }
  .stop-actions button {
    padding: .45rem 1rem; border-radius: 6px;
    font-size: .82rem; cursor: pointer;
    border: 1px solid transparent;
  }
  .stop-actions .cancel {
    background: #1e1e1e; color: #ccc; border-color: #2a2a2a;
  }
  .stop-actions .cancel:hover { background: #252525; }
  .stop-actions .confirm-stop {
    background: #5a2424; color: #ffd6d6; border-color: #7a3434;
  }
  .stop-actions .confirm-stop:hover { background: #6a2c2c; }

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
