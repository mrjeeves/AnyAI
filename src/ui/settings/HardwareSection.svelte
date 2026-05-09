<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { loadConfig, updateConfig } from "../../config";
  import type { HardwareProfile, GpuType, MicConfig } from "../../types";

  type Tab = "providers" | "families" | "models" | "storage" | "updates" | "hardware";

  let { setActive } = $props<{ setActive: (tab: Tab) => void }>();

  let hardware = $state<HardwareProfile | null>(null);
  let conversationDir = $state("");
  let loading = $state(true);
  let error = $state("");

  // Microphone config + live device list. We populate `micDevices` lazily
  // (after the user clicks "Allow access") so a never-used transcribe install
  // doesn't ask for the mic permission on every Settings open.
  let mic = $state<MicConfig | null>(null);
  let micDevices = $state<MediaDeviceInfo[]>([]);
  let micError = $state("");
  let micPermission = $state<"unknown" | "granted" | "denied">("unknown");

  // VU meter state for the Test button — last RMS sample (0..1) and the
  // refs we need to release the WebAudio graph cleanly when the user stops.
  let testing = $state(false);
  let level = $state(0);
  let testStream: MediaStream | null = null;
  let testCtx: AudioContext | null = null;
  let testRaf = 0;

  onMount(async () => {
    try {
      const [hw, config] = await Promise.all([
        invoke<HardwareProfile>("detect_hardware"),
        loadConfig(),
      ]);
      hardware = hw;
      conversationDir = config.conversation_dir ?? "";
      mic = { ...config.mic };
      // If the OS already granted access in a previous session, the device
      // list comes back populated with labels — list it eagerly so the
      // dropdown shows real device names instead of "Microphone (default)".
      await refreshDevices(false);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  onDestroy(() => stopTest());

  /** Read the device list from the browser. When `prompt` is true we first
   *  call getUserMedia to coerce the OS permission dialog — without that the
   *  enumerated MediaDeviceInfo entries come back with empty labels. */
  async function refreshDevices(prompt: boolean) {
    try {
      if (prompt) {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        // Drop the temp stream straight away — we only needed it to unlock
        // the device labels. The Test button opens its own stream.
        stream.getTracks().forEach((t) => t.stop());
        micPermission = "granted";
      }
      const all = await navigator.mediaDevices.enumerateDevices();
      micDevices = all.filter((d) => d.kind === "audioinput");
      // Heuristic: at least one device with a non-empty label means we have
      // permission already; all-empty labels means we haven't asked yet.
      if (micPermission === "unknown" && micDevices.some((d) => d.label)) {
        micPermission = "granted";
      }
      micError = "";
    } catch (e: unknown) {
      const err = e as { name?: string; message?: string };
      if (err?.name === "NotAllowedError" || err?.name === "PermissionDeniedError") {
        micPermission = "denied";
        micError = "Microphone access was blocked. Allow it in your OS / browser settings.";
      } else {
        micError = String(err?.message ?? e);
      }
    }
  }

  async function patchMic(patch: Partial<MicConfig>) {
    if (!mic) return;
    const next = { ...mic, ...patch };
    mic = next;
    await updateConfig({ mic: next });
    // Restart the running test with the new constraints so the user can
    // hear their toggle take effect immediately.
    if (testing) {
      stopTest();
      await startTest();
    }
  }

  async function startTest() {
    if (!mic) return;
    try {
      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: mic.device_id ? { exact: mic.device_id } : undefined,
          sampleRate: mic.sample_rate,
          echoCancellation: mic.echo_cancellation,
          noiseSuppression: mic.noise_suppression,
          autoGainControl: mic.auto_gain_control,
        },
      };
      testStream = await navigator.mediaDevices.getUserMedia(constraints);
      micPermission = "granted";
      // After the first grant, the device list grows labels — refresh so the
      // dropdown shows the real names without forcing a second prompt.
      navigator.mediaDevices.enumerateDevices().then((all) => {
        micDevices = all.filter((d) => d.kind === "audioinput");
      });
      const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      testCtx = new Ctx();
      const src = testCtx.createMediaStreamSource(testStream);
      const analyser = testCtx.createAnalyser();
      analyser.fftSize = 1024;
      src.connect(analyser);
      const buf = new Float32Array(analyser.fftSize);
      const tick = () => {
        analyser.getFloatTimeDomainData(buf);
        // RMS over the buffer → smooth, log-ish meter that doesn't jitter
        // wildly between frames.
        let sum = 0;
        for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
        const rms = Math.sqrt(sum / buf.length);
        level = Math.min(1, rms * 4);
        testRaf = requestAnimationFrame(tick);
      };
      testing = true;
      micError = "";
      tick();
    } catch (e: unknown) {
      const err = e as { name?: string; message?: string };
      if (err?.name === "NotAllowedError" || err?.name === "PermissionDeniedError") {
        micPermission = "denied";
        micError = "Microphone access was blocked. Allow it in your OS / browser settings.";
      } else {
        micError = String(err?.message ?? e);
      }
      stopTest();
    }
  }

  function stopTest() {
    testing = false;
    level = 0;
    if (testRaf) cancelAnimationFrame(testRaf);
    testRaf = 0;
    testCtx?.close().catch(() => {});
    testCtx = null;
    testStream?.getTracks().forEach((t) => t.stop());
    testStream = null;
  }

  function gpuLabel(g: GpuType): string {
    switch (g) {
      case "nvidia": return "NVIDIA";
      case "amd":    return "AMD";
      case "apple":  return "Apple Silicon";
      case "none":   return "None detected";
    }
  }

  function gbLabel(gb: number | null | undefined): string {
    if (gb == null) return "—";
    return `${gb.toFixed(1)} GB`;
  }
</script>

<div class="section">
  <div class="head">
    <p class="lede">
      What AnyAI sees on this machine. The resolver picks model tiers against
      <strong>VRAM</strong> and <strong>RAM</strong>; storage limits how many
      models you can keep pulled.
    </p>
  </div>

  {#if loading}
    <p class="loading">Loading…</p>
  {:else if error && !hardware}
    <p class="error">{error}</p>
  {:else if hardware}
    <div class="cards">
      <div class="group-label">Compute</div>

      <div class="card">
        <div class="card-title">Accelerator</div>
        <dl class="info">
          <div>
            <dt>GPU</dt>
            <dd>
              <span class="badge gpu-{hardware.gpu_type}">{gpuLabel(hardware.gpu_type)}</span>
            </dd>
          </div>
          <div>
            <dt>VRAM</dt>
            <dd>
              {gbLabel(hardware.vram_gb)}
              {#if hardware.gpu_type === "apple" && hardware.vram_gb != null}
                <span class="dim">(unified)</span>
              {/if}
            </dd>
          </div>
          <div>
            <dt>Used for</dt>
            <dd class="dim">tier selection in picks</dd>
          </div>
        </dl>
        {#if hardware.gpu_type === "none"}
          <p class="card-meta">
            No discrete GPU detected — picks fall back to CPU-friendly tiers
            sized against RAM.
          </p>
        {/if}
      </div>

      <div class="card">
        <div class="card-title">CPU &amp; system memory</div>
        <dl class="info">
          <div>
            <dt>Architecture</dt>
            <dd><code>{hardware.arch ?? "unknown"}</code></dd>
          </div>
          <div>
            <dt>RAM</dt>
            <dd>{gbLabel(hardware.ram_gb)}</dd>
          </div>
          {#if hardware.soc}
            <div>
              <dt>Board</dt>
              <dd>{hardware.soc}</dd>
            </div>
          {/if}
        </dl>
      </div>

      <div class="group-label">Storage</div>

      <div class="card">
        <div class="card-title">Disk</div>
        <dl class="info">
          <div>
            <dt>Free space</dt>
            <dd>{gbLabel(hardware.disk_free_gb)}</dd>
          </div>
          <div>
            <dt>Conversations</dt>
            <dd>
              {#if conversationDir}
                <code class="path">{conversationDir}</code>
              {:else}
                <span class="dim">default under ~/.anyai/</span>
              {/if}
            </dd>
          </div>
        </dl>
        <div class="card-actions">
          <button class="link-btn" onclick={() => setActive("storage")}>
            Manage in Storage →
          </button>
          <button class="link-btn" onclick={() => setActive("models")}>
            Manage in Models →
          </button>
        </div>
      </div>

      <div class="group-label">Audio input</div>

      <div class="card">
        <div class="card-title">Microphone</div>
        <p class="card-meta">
          Used by Transcribe mode. Settings apply the next time you start a
          session.
        </p>

        {#if mic}
          <dl class="info">
            <div class="full">
              <dt>Device</dt>
              <dd>
                {#if micDevices.length === 0}
                  <button class="link-btn" onclick={() => refreshDevices(true)}>
                    Allow microphone access
                  </button>
                  <span class="dim">to list devices</span>
                {:else}
                  <select
                    value={mic.device_id}
                    onchange={(e) => patchMic({ device_id: (e.currentTarget as HTMLSelectElement).value })}
                  >
                    <option value="">System default</option>
                    {#each micDevices as d (d.deviceId)}
                      <option value={d.deviceId}>
                        {d.label || `Microphone ${d.deviceId.slice(0, 6)}`}
                      </option>
                    {/each}
                  </select>
                {/if}
              </dd>
            </div>

            <div>
              <dt>Sample rate</dt>
              <dd>
                <select
                  value={String(mic.sample_rate)}
                  onchange={(e) =>
                    patchMic({ sample_rate: parseInt((e.currentTarget as HTMLSelectElement).value, 10) })}
                >
                  <option value="16000">16 kHz (speech)</option>
                  <option value="22050">22.05 kHz</option>
                  <option value="44100">44.1 kHz</option>
                  <option value="48000">48 kHz</option>
                </select>
              </dd>
            </div>

            <div>
              <dt>Test level</dt>
              <dd>
                {#if testing}
                  <button class="link-btn" onclick={stopTest}>Stop</button>
                {:else}
                  <button class="link-btn" onclick={startTest}>Test</button>
                {/if}
                <div class="vu" aria-label="Microphone level">
                  <div class="vu-fill" style="width: {Math.round(level * 100)}%"></div>
                </div>
              </dd>
            </div>
          </dl>

          <div class="toggles">
            <label>
              <input
                type="checkbox"
                checked={mic.echo_cancellation}
                onchange={(e) =>
                  patchMic({ echo_cancellation: (e.currentTarget as HTMLInputElement).checked })}
              />
              Echo cancellation
            </label>
            <label>
              <input
                type="checkbox"
                checked={mic.noise_suppression}
                onchange={(e) =>
                  patchMic({ noise_suppression: (e.currentTarget as HTMLInputElement).checked })}
              />
              Noise suppression
            </label>
            <label>
              <input
                type="checkbox"
                checked={mic.auto_gain_control}
                onchange={(e) =>
                  patchMic({ auto_gain_control: (e.currentTarget as HTMLInputElement).checked })}
              />
              Auto gain control
            </label>
          </div>

          {#if micError}
            <p class="card-meta error-text">{micError}</p>
          {:else if micPermission === "unknown" && micDevices.length === 0}
            <p class="card-meta">
              Click <em>Allow microphone access</em> to populate the device list.
            </p>
          {/if}
        {/if}
      </div>

      <p class="footnote">
        Speakers, camera, GPU grouping, and CPU/GPU-only modes will surface
        here as multimodal support lands.
      </p>
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .head { padding: .75rem 1rem; border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
  .lede { font-size: .78rem; color: #888; line-height: 1.5; }
  .lede strong { color: #ccc; font-weight: 600; }

  .loading, .error { padding: 2rem; text-align: center; color: #555; font-size: .82rem; }
  .error { color: #d66; }

  .cards { flex: 1; overflow-y: auto; padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-height: 0; }
  .group-label {
    font-size: .68rem; color: #666; text-transform: uppercase;
    letter-spacing: .06em; margin: .35rem .15rem -.1rem;
  }
  .group-label:first-child { margin-top: 0; }

  .card {
    border: 1px solid #1e1e1e;
    background: #131318;
    border-radius: 8px;
    padding: .75rem .9rem;
    display: flex; flex-direction: column; gap: .5rem;
  }
  .card-title { font-size: .9rem; font-weight: 600; color: #e8e8e8; }
  .card-meta { font-size: .76rem; color: #888; line-height: 1.5; margin: 0; }

  .info {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: .65rem;
  }
  .info > div { display: flex; flex-direction: column; gap: .2rem; min-width: 0; }
  dt { font-size: .68rem; color: #666; text-transform: uppercase; letter-spacing: .03em; }
  dd { margin: 0; font-size: .82rem; color: #ccc; display: flex; align-items: center; gap: .35rem; flex-wrap: wrap; }
  dd .dim { color: #555; font-size: .74rem; }
  dd code { font-family: monospace; font-size: .76rem; color: #9a7; }

  .badge {
    font-size: .72rem;
    padding: .12rem .5rem;
    border-radius: 4px;
    border: 1px solid;
  }
  .badge.gpu-nvidia { background: #14221a; border-color: #1e3325; color: #6c6; }
  .badge.gpu-amd    { background: #221414; border-color: #331e1e; color: #e88; }
  .badge.gpu-apple  { background: #181822; border-color: #25253a; color: #aab; }
  .badge.gpu-none   { background: #1a1a1a; border-color: #2a2a2a; color: #888; }

  .path {
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    max-width: 100%;
  }

  .card-actions { display: flex; gap: .35rem; flex-wrap: wrap; }
  .link-btn {
    background: none; border: 1px solid #2a2a3a; color: #6e6ef7;
    padding: .35rem .65rem; border-radius: 6px; font-size: .78rem; cursor: pointer;
  }
  .link-btn:hover { background: #1a1a2a; }

  .footnote {
    font-size: .72rem; color: #555; line-height: 1.5;
    padding: .35rem .15rem 0; margin: 0;
  }

  .info > div.full { grid-column: 1 / -1; }

  select {
    background: #0f0f12;
    color: #e8e8e8;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: .3rem .4rem;
    font-size: .8rem;
    font-family: inherit;
    max-width: 100%;
  }
  select:focus { outline: none; border-color: #6e6ef7; }

  .toggles {
    display: flex;
    flex-wrap: wrap;
    gap: .85rem;
    padding-top: .15rem;
  }
  .toggles label {
    display: inline-flex;
    align-items: center;
    gap: .35rem;
    font-size: .78rem;
    color: #ccc;
    cursor: pointer;
  }
  .toggles input { accent-color: #6e6ef7; }

  .vu {
    display: inline-block;
    width: 90px;
    height: 6px;
    background: #1a1a1a;
    border-radius: 3px;
    overflow: hidden;
    vertical-align: middle;
  }
  .vu-fill {
    height: 100%;
    background: linear-gradient(90deg, #4caf50 0%, #d49a3b 70%, #e35a5a 100%);
    transition: width .05s linear;
  }
  .error-text { color: #d66; }
</style>
