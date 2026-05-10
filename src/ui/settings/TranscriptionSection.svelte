<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getActiveManifest } from "../../providers";
  import { resolveModelEx } from "../../manifest";
  import { loadConfig, updateConfig } from "../../config";
  import type { HardwareProfile } from "../../types";

  interface WhisperModelInfo {
    name: string;
    approx_size_bytes: number;
    installed: boolean;
    installed_size_bytes: number | null;
  }

  interface PullProgress {
    name: string;
    bytes: number;
    total: number;
    done: boolean;
    error: string | null;
  }

  let models = $state<WhisperModelInfo[]>([]);
  let loading = $state(true);
  let error = $state("");
  let pulling = $state<Record<string, PullProgress>>({});
  let unlisteners: UnlistenFn[] = [];

  /** What the family/tier resolver would pick on this hardware right now,
   *  and what (if anything) the user has set as `mode_overrides.transcribe`.
   *  Both surface in the UI so the user knows what's automatic vs. manual. */
  let recommended = $state<string>("");
  let recommendedSource = $state<"family" | "override">("family");
  let activeFamily = $state("");
  let override = $state<string>("");

  onMount(async () => {
    try {
      const [list, manifest, config, hw] = await Promise.all([
        invoke<WhisperModelInfo[]>("whisper_models_list"),
        getActiveManifest(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
      ]);
      models = list;
      activeFamily = config.active_family;
      override = config.mode_overrides.transcribe ?? "";
      const r = resolveModelEx(hw, manifest, "transcribe", config.mode_overrides, activeFamily);
      recommended = r.model;
      recommendedSource = r.override ? "override" : "family";
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  async function setOverride(name: string) {
    const next = name || null;
    override = name;
    const cfg = await loadConfig();
    await updateConfig({
      mode_overrides: { ...cfg.mode_overrides, transcribe: next },
    });
    if (next) {
      recommended = next;
      recommendedSource = "override";
    } else {
      // Re-derive from the family/tier ladder.
      const [manifest, config, hw] = await Promise.all([
        getActiveManifest(),
        loadConfig(),
        invoke<HardwareProfile>("detect_hardware"),
      ]);
      const r = resolveModelEx(hw, manifest, "transcribe", config.mode_overrides, activeFamily);
      recommended = r.model;
      recommendedSource = "family";
    }
  }

  onDestroy(() => {
    for (const u of unlisteners) u();
  });

  async function refresh() {
    try {
      models = await invoke<WhisperModelInfo[]>("whisper_models_list");
    } catch (e) {
      error = String(e);
    }
  }

  async function pull(name: string) {
    if (pulling[name]) return;
    pulling = { ...pulling, [name]: { name, bytes: 0, total: 0, done: false, error: null } };
    try {
      const un = await listen<PullProgress>(`anyai://whisper-pull/${name}`, (evt) => {
        pulling = { ...pulling, [name]: evt.payload };
        if (evt.payload.done) {
          // Drop the spinner row + refresh the catalogue when the pull
          // either lands cleanly or surfaces an error.
          setTimeout(() => {
            const next = { ...pulling };
            delete next[name];
            pulling = next;
            refresh();
          }, evt.payload.error ? 4000 : 600);
        }
      });
      unlisteners.push(un);
      await invoke("whisper_model_pull", { name });
    } catch (e) {
      pulling = {
        ...pulling,
        [name]: { name, bytes: 0, total: 0, done: true, error: String(e) },
      };
    }
  }

  async function remove(name: string) {
    if (!confirm(`Delete the ${name} whisper model? It can be re-downloaded later.`)) return;
    try {
      await invoke("whisper_model_remove", { name });
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }
</script>

<div class="section">
  <div class="head">
    <p class="lede">
      Whisper models for local transcription. Files live under
      <code>~/.anyai/whisper/</code> and download from
      <code>huggingface.co/ggerganov/whisper.cpp</code> on demand. Smaller
      models start faster and use less RAM; larger ones are more accurate.
    </p>
  </div>

  {#if loading}
    <p class="loading">Loading…</p>
  {:else if error && models.length === 0}
    <p class="error">{error}</p>
  {:else}
    <div class="cards">
      <div class="card">
        <div class="card-title">Currently selected</div>
        <div class="selected">
          <div class="selected-row">
            <code class="model-name big">{recommended || "—"}</code>
            {#if recommendedSource === "override"}
              <span class="badge override">manual override</span>
            {:else}
              <span class="badge auto">picked by {activeFamily} for your hardware</span>
            {/if}
          </div>
          <label class="override-row">
            <span class="override-label">Override pick:</span>
            <select
              value={override}
              onchange={(e) => setOverride((e.currentTarget as HTMLSelectElement).value)}
            >
              <option value="">— use family / tier resolver —</option>
              {#each models as m (m.name)}
                <option value={m.name}>
                  {m.name} {m.installed ? "(installed)" : ""}
                </option>
              {/each}
            </select>
          </label>
        </div>
      </div>

      <div class="card">
        <div class="card-title">Available models</div>
        <div class="rows">
          {#each models as m (m.name)}
            <div class="row">
              <div class="row-meta">
                <code class="model-name">{m.name}</code>
                <span class="size">
                  {m.installed && m.installed_size_bytes != null
                    ? fmtBytes(m.installed_size_bytes)
                    : `~${fmtBytes(m.approx_size_bytes)}`}
                </span>
                {#if m.installed}
                  <span class="badge installed">installed</span>
                {/if}
              </div>
              <div class="row-actions">
                {#if pulling[m.name]}
                  {@const p = pulling[m.name]}
                  {#if p.error}
                    <span class="error-inline">Failed: {p.error}</span>
                  {:else}
                    <progress
                      max={p.total || 1}
                      value={p.bytes}
                      title={p.total ? `${fmtBytes(p.bytes)} / ${fmtBytes(p.total)}` : "Downloading…"}
                    ></progress>
                    <span class="dim">{p.total ? Math.round((p.bytes / p.total) * 100) : 0}%</span>
                  {/if}
                {:else if m.installed}
                  <button class="link-btn danger" onclick={() => remove(m.name)}>Delete</button>
                {:else}
                  <button class="link-btn" onclick={() => pull(m.name)}>Download</button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
        {#if error}
          <p class="error-text">{error}</p>
        {/if}
      </div>

      <p class="footnote">
        Model files are stored as <code>ggml-&lt;name&gt;.bin</code>. Live
        transcription captures via the OS audio API and runs whisper
        in-process on 5-second chunks — nothing leaves the machine.
      </p>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede code { color: #9a7; font-size: .74rem; }

  .loading, .error { padding: 2rem; text-align: center; color: #555; font-size: .82rem; }
  .error { color: #d66; }

  .cards { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-height: 0; }

  .card {
    border: 1px solid #1e1e1e;
    background: #131318;
    border-radius: 8px;
    padding: .75rem .9rem;
    display: flex; flex-direction: column; gap: .5rem;
  }
  .card-title { font-size: .9rem; font-weight: 600; color: #e8e8e8; }

  .rows { display: flex; flex-direction: column; gap: .25rem; }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: .75rem;
    padding: .5rem .25rem;
    border-bottom: 1px solid #181820;
  }
  .row:last-child { border-bottom: none; }
  .row-meta { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  .model-name { color: #e8e8e8; font-size: .82rem; }
  .size { color: #888; font-size: .76rem; }
  .badge {
    font-size: .68rem;
    padding: .12rem .5rem;
    border-radius: 4px;
    border: 1px solid;
  }
  .badge.installed {
    background: #14221a;
    border-color: #1e3325;
    color: #6c6;
  }
  .badge.override {
    background: #2a1a14;
    border-color: #4a3325;
    color: #d49a3b;
  }
  .badge.auto {
    background: #14182a;
    border-color: #1e2545;
    color: #8a8af0;
  }
  .selected { display: flex; flex-direction: column; gap: .55rem; }
  .selected-row { display: flex; align-items: center; gap: .55rem; flex-wrap: wrap; }
  .model-name.big { font-size: 1rem; color: #e8e8e8; }
  .override-row { display: flex; align-items: center; gap: .55rem; flex-wrap: wrap; }
  .override-label { font-size: .76rem; color: #888; }
  .override-row select {
    background: #0f0f12;
    color: #e8e8e8;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: .3rem .4rem;
    font-size: .8rem;
    font-family: inherit;
  }
  .override-row select:focus { outline: none; border-color: #6e6ef7; }
  .row-actions { display: flex; align-items: center; gap: .5rem; }
  .link-btn {
    background: none; border: 1px solid #2a2a3a; color: #6e6ef7;
    padding: .3rem .65rem; border-radius: 6px; font-size: .78rem; cursor: pointer;
  }
  .link-btn:hover { background: #1a1a2a; }
  .link-btn.danger { color: #ff8b8b; border-color: #3a1f1f; }
  .link-btn.danger:hover { background: #2a1818; }
  progress {
    width: 130px;
    height: 6px;
    accent-color: #6e6ef7;
  }
  .dim { color: #888; font-size: .76rem; }
  .error-inline { color: #d66; font-size: .76rem; }
  .error-text { color: #d66; font-size: .78rem; margin: 0; }
  .footnote {
    font-size: .72rem; color: #555; line-height: 1.5;
    padding: .35rem .15rem 0; margin: 0;
  }
  code { font-family: monospace; }
</style>
