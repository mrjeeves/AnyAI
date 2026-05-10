<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { mkdir, exists } from "@tauri-apps/plugin-fs";
  import { loadConfig, saveConfig } from "../../config";
  import type { HardwareProfile, OllamaModel } from "../../types";

  interface WhisperInfo {
    name: string;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  type Tab = "providers" | "families" | "models" | "storage" | "updates";

  let { setActive } = $props<{ setActive: (tab: Tab) => void }>();

  let totalBytes = $state(0);
  let modelCount = $state(0);
  let conversationDir = $state("");
  let savedConvDir = $state("");
  let saving = $state(false);
  let savedFlash = $state(false);
  let saveError = $state("");
  let diskFreeGb = $state<number | null>(null);
  let dirExists = $state<boolean | null>(null);
  let loading = $state(true);
  /** Bytes parked under `~/.anyai/transcribe-buffer/`. > 0 when whisper
   *  fell behind realtime and audio is spilling to disk; the inference
   *  loop drains the dir as it catches up. The card stays hidden when
   *  there's nothing pending — there's no useful "0 GB" reading. */
  let transcribeBacklogBytes = $state(0);

  onMount(async () => {
    try {
      const [pulled, whisper, hw, config, backlog] = await Promise.all([
        invoke<OllamaModel[]>("ollama_list_models").catch(() => [] as OllamaModel[]),
        invoke<WhisperInfo[]>("whisper_models_list").catch(() => [] as WhisperInfo[]),
        invoke<HardwareProfile>("detect_hardware").catch(() => null),
        loadConfig(),
        invoke<number>("transcribe_buffer_size_bytes").catch(() => 0),
      ]);
      // Whisper models live under ~/.anyai/whisper/, not in Ollama's library,
      // so the on-disk total has to sum both backends or it under-reports
      // every transcribe install. Only count installed entries with a known
      // size — pending downloads and unknown-size rows would inflate the
      // total without representing real bytes on disk.
      const ollamaBytes = pulled.reduce((acc, m) => acc + m.size, 0);
      const whisperInstalled = whisper.filter(
        (w) => w.installed && (w.installed_size_bytes ?? 0) > 0,
      );
      const whisperBytes = whisperInstalled.reduce(
        (acc, w) => acc + (w.installed_size_bytes ?? 0),
        0,
      );
      totalBytes = ollamaBytes + whisperBytes;
      modelCount = pulled.length + whisperInstalled.length;
      diskFreeGb = hw?.disk_free_gb ?? null;
      transcribeBacklogBytes = backlog;
      conversationDir = config.conversation_dir ?? "";
      savedConvDir = conversationDir;
      if (conversationDir) {
        try { dirExists = await exists(conversationDir); } catch { dirExists = null; }
      }
    } finally {
      loading = false;
    }
  });

  function backlogSeconds(bytes: number): number {
    // 16 kHz mono f32 → 64 KB per second of audio. We round to the
    // nearest second; the goal is "you have ~12 s pending", not lab-grade
    // precision.
    return Math.round(bytes / (16000 * 4));
  }

  function bytesLabel(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / 1024 / 1024 / 1024).toFixed(2) + " GB";
    if (bytes >= 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + " MB";
    if (bytes >= 1024) return Math.round(bytes / 1024) + " KB";
    return bytes + " B";
  }

  async function saveConvDir() {
    if (saving) return;
    saving = true;
    saveError = "";
    try {
      const trimmed = conversationDir.trim();
      const config = await loadConfig();
      config.conversation_dir = trimmed;
      await saveConfig(config);
      // Best-effort directory creation so future writes don't ENOENT.
      // Failure is non-fatal — the user may be pointing at a network share
      // they'll mount later, or a path their OS will create lazily.
      try { await mkdir(trimmed, { recursive: true }); } catch {}
      try { dirExists = await exists(trimmed); } catch { dirExists = null; }
      savedConvDir = trimmed;
      savedFlash = true;
      setTimeout(() => (savedFlash = false), 1600);
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  function gbLabel(bytes: number): string {
    if (bytes <= 0) return "0 GB";
    return (bytes / 1024 / 1024 / 1024).toFixed(1) + " GB";
  }

  const isDirty = $derived(conversationDir.trim() !== savedConvDir);
</script>

<div class="section">
  <div class="head">
    <p class="lede">
      Where AnyAI's data lives on this machine. Models are managed by Ollama;
      conversations and artifacts live under <code>~/.anyai/</code> by default.
    </p>
  </div>

  {#if loading}
    <p class="loading">Loading…</p>
  {:else}
    <div class="cards scroll-fade">
      <div class="card">
        <div class="card-row">
          <div class="card-info">
            <div class="card-title">Models</div>
            <div class="card-meta">
              {modelCount === 0 ? "no models pulled" : `${modelCount} pulled · ${gbLabel(totalBytes)}`}
              {#if diskFreeGb != null}
                <span class="dim"> · {diskFreeGb.toFixed(1)} GB free on disk</span>
              {/if}
            </div>
          </div>
          <button class="link-btn" onclick={() => setActive("models")}>
            Manage in Models →
          </button>
        </div>
      </div>

      {#if transcribeBacklogBytes > 0}
        <div class="card">
          <div class="card-row">
            <div class="card-info">
              <div class="card-title">Transcription backlog</div>
              <div class="card-meta">
                <span class="warn">
                  {bytesLabel(transcribeBacklogBytes)}
                  ({backlogSeconds(transcribeBacklogBytes)} s of audio)
                </span>
                pending whisper inference under
                <code>~/.anyai/transcribe-buffer/</code>.
                Drains automatically while AnyAI is open.
              </div>
            </div>
          </div>
        </div>
      {/if}

      <div class="card">
        <div class="card-title">Conversations &amp; artifacts</div>
        <p class="card-meta">
          Saved chats and any files generated during them. Defaults to a folder under
          <code>~/.anyai/</code>; change it to a synced folder if you want them backed up.
        </p>
        <div class="path-row">
          <input
            class="path-input"
            bind:value={conversationDir}
            spellcheck="false"
            placeholder="/path/to/conversations"
          />
          <button
            class="save-btn"
            disabled={saving || !isDirty || !conversationDir.trim()}
            onclick={saveConvDir}
          >
            {saving ? "Saving…" : isDirty ? "Save" : "Saved"}
          </button>
        </div>
        <div class="path-meta">
          {#if savedFlash}
            <span class="ok">✓ Saved.</span>
          {:else if saveError}
            <span class="err">{saveError}</span>
          {:else if !isDirty && conversationDir}
            {#if dirExists === true}
              <span class="dim">Folder exists.</span>
            {:else if dirExists === false}
              <span class="dim">Folder will be created on first write.</span>
            {/if}
          {/if}
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede code { font-family: monospace; font-size: .76rem; color: #aaa; background: #1a1a22; padding: 0 .25rem; border-radius: 3px; }

  .cards { flex: 1; overflow-y: scroll; padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-height: 0; --scroll-fade-bg: #111; }
  .card {
    border: 1px solid #1e1e1e;
    background: #131318;
    border-radius: 8px;
    padding: .75rem .9rem;
    display: flex; flex-direction: column; gap: .35rem;
  }
  .card-row {
    display: flex; align-items: center; gap: .75rem;
  }
  .card-info { flex: 1; display: flex; flex-direction: column; gap: .15rem; }
  .card-title { font-size: .9rem; font-weight: 600; color: #e8e8e8; }
  .card-meta { font-size: .76rem; color: #888; line-height: 1.5; }
  .card-meta .dim { color: #555; }
  .card-meta .warn { color: #ffd166; font-weight: 600; }
  code { font-family: monospace; font-size: .76rem; color: #aaa; background: #1a1a22; padding: 0 .25rem; border-radius: 3px; }

  .link-btn {
    background: none; border: 1px solid #2a2a3a; color: #6e6ef7;
    padding: .35rem .65rem; border-radius: 6px; font-size: .78rem; cursor: pointer;
  }
  .link-btn:hover { background: #1a1a2a; }

  .path-row { display: flex; gap: .35rem; margin-top: .35rem; }
  .path-input {
    flex: 1;
    background: #1a1a1a; border: 1px solid #2a2a2a; border-radius: 6px;
    color: #e8e8e8; padding: .4rem .55rem; font-size: .8rem;
    font-family: monospace;
  }
  .path-input:focus { outline: none; border-color: #6e6ef7; }
  .save-btn {
    padding: .4rem .8rem; background: #6e6ef7; color: #fff; border: none;
    border-radius: 6px; cursor: pointer; font-size: .78rem;
  }
  .save-btn:hover:not(:disabled) { background: #5a5ae0; }
  .save-btn:disabled { opacity: .4; cursor: default; }
  .path-meta { min-height: 1em; font-size: .72rem; }
  .path-meta .ok  { color: #6a6; }
  .path-meta .err { color: #f88; }
  .path-meta .dim { color: #555; }

  .loading { padding: 1.5rem; text-align: center; color: #555; font-size: .82rem; }
</style>
