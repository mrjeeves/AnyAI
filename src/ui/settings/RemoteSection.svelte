<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  interface RemoteUiStatus {
    enabled: boolean;
    running: boolean;
    port: number;
    lan_ips: string[];
    remote_active: boolean;
  }

  let status = $state<RemoteUiStatus | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state("");
  let portInput = $state(1474);
  let qrSvg = $state<string>("");
  let qrFor = $state<string>("");

  let pollTimer: ReturnType<typeof setInterval> | null = null;

  onMount(async () => {
    await refresh();
    // Cheap poll so the "remote_active" indicator on this very page
    // reflects sessions that come and go while the user is reading.
    pollTimer = setInterval(refresh, 3000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });

  async function refresh() {
    try {
      status = await invoke<RemoteUiStatus>("remote_ui_status");
      portInput = status.port;
      await regenerateQr();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function primaryUrl(s: RemoteUiStatus): string | null {
    const ip = s.lan_ips[0];
    if (!ip) return null;
    return `http://${ip}:${s.port}`;
  }

  async function regenerateQr() {
    if (!status) return;
    const url = primaryUrl(status);
    if (!url) {
      qrSvg = "";
      qrFor = "";
      return;
    }
    if (qrFor === url && qrSvg) return;
    try {
      qrSvg = await invoke<string>("remote_ui_qr", { text: url });
      qrFor = url;
    } catch {
      qrSvg = "";
      qrFor = "";
    }
  }

  async function toggle() {
    if (!status || busy) return;
    busy = true;
    error = "";
    try {
      const next = !status.enabled;
      status = await invoke<RemoteUiStatus>("remote_ui_set_enabled", {
        enabled: next,
        port: portInput,
      });
      portInput = status.port;
      await regenerateQr();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function applyPort() {
    if (!status || busy) return;
    if (portInput === status.port) return;
    busy = true;
    error = "";
    try {
      // Re-applying with the same `enabled` value rebinds on the new port.
      status = await invoke<RemoteUiStatus>("remote_ui_set_enabled", {
        enabled: status.enabled,
        port: portInput,
      });
      await regenerateQr();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function copyUrl() {
    if (!status) return;
    const url = primaryUrl(status);
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
    } catch {
      // Clipboard unavailable in some Tauri webview contexts; ignore.
    }
  }
</script>

<div class="section">
  {#if loading}
    <div class="loading">Loading…</div>
  {:else if status}
    <div class="content">
      <div class="header-row">
        <div>
          <div class="title">Remote access</div>
          <div class="meta">
            Open MyOwnLLM from another device on your local network — phone, tablet, laptop.
            Single-user: while a remote session is active, this window will be curtained off.
          </div>
        </div>
        <button
          class="toggle"
          class:on={status.enabled}
          onclick={toggle}
          disabled={busy}
          aria-pressed={status.enabled}
        >
          <span class="track"><span class="thumb"></span></span>
          <span class="label">{status.enabled ? "On" : "Off"}</span>
        </button>
      </div>

      {#if status.enabled}
        {#if status.lan_ips.length === 0}
          <div class="notice warn">
            Couldn't determine a LAN IPv4 address for this machine. Check that you're connected to a
            network — the server is bound on every interface, but there's nothing to share.
          </div>
        {:else}
          <div class="card">
            <div class="card-row">
              <div class="card-col grow">
                <div class="dt">Open in a browser on the same network</div>
                <div class="urls">
                  {#each status.lan_ips as ip, i}
                    <div class="url-row">
                      <code class="url">http://{ip}:{status.port}</code>
                      {#if i === 0}
                        <button class="copy" onclick={copyUrl}>Copy</button>
                      {/if}
                    </div>
                  {/each}
                </div>
                <div class="hint">
                  Phones can scan the QR. The link is reachable only from devices on this network.
                </div>
              </div>
              {#if qrSvg}
                <div class="qr" aria-label="QR code for the remote URL">
                  {@html qrSvg}
                </div>
              {/if}
            </div>
          </div>
        {/if}

        <div class="status-row">
          <span class="dot" class:active={status.remote_active}></span>
          <span>
            {status.remote_active ? "In use remotely right now" : "No remote sessions"}
          </span>
        </div>
      {:else}
        <div class="notice">
          Turn on remote access to expose a minimal chat UI on your LAN. Off by default.
        </div>
      {/if}

      <div class="port-row">
        <label for="port-input">Port</label>
        <input
          id="port-input"
          type="number"
          min="1024"
          max="65535"
          bind:value={portInput}
          onchange={applyPort}
          disabled={busy}
        />
        <span class="port-hint">
          {#if status.enabled && status.running}
            Listening on 0.0.0.0:{status.port}
          {:else}
            Will bind 0.0.0.0:{portInput} when enabled
          {/if}
        </span>
      </div>

      {#if error}
        <div class="error">{error}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .loading {
    padding: 2rem;
    text-align: center;
    color: #555;
    font-size: 0.85rem;
  }
  .content {
    padding: 1rem 1.1rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .header-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
  }
  .title {
    font-size: 1rem;
    color: #e8e8e8;
    font-weight: 600;
  }
  .meta {
    font-size: 0.76rem;
    color: #777;
    margin-top: 0.25rem;
    line-height: 1.5;
    max-width: 28rem;
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    background: none;
    border: none;
    cursor: pointer;
    color: #888;
    font-size: 0.8rem;
    padding: 0.25rem 0.35rem;
  }
  .toggle:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .toggle .track {
    position: relative;
    width: 36px;
    height: 20px;
    background: #232323;
    border-radius: 999px;
    transition: background 0.15s;
  }
  .toggle .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    background: #888;
    border-radius: 50%;
    transition:
      transform 0.15s,
      background 0.15s;
  }
  .toggle.on .track {
    background: #2a3a55;
  }
  .toggle.on .thumb {
    transform: translateX(16px);
    background: #6e6ef7;
  }
  .toggle.on .label {
    color: #b9b9ee;
  }

  .notice {
    padding: 0.55rem 0.8rem;
    border-radius: 7px;
    font-size: 0.78rem;
    line-height: 1.5;
    background: #131820;
    border: 1px solid #1e2530;
    color: #99a;
  }
  .notice.warn {
    background: #2a220e;
    color: #d6b25a;
    border: 1px solid #3a2e0e;
  }

  .card {
    background: #131313;
    border: 1px solid #1e1e1e;
    border-radius: 7px;
    padding: 0.85rem;
  }
  .card-row {
    display: flex;
    gap: 1rem;
    align-items: stretch;
  }
  .card-col {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    min-width: 0;
  }
  .card-col.grow {
    flex: 1;
  }
  .dt {
    font-size: 0.68rem;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .urls {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .url-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .url {
    font-family: monospace;
    font-size: 0.85rem;
    color: #cfeacf;
    background: #0d0d0d;
    padding: 0.25rem 0.5rem;
    border-radius: 5px;
    border: 1px solid #1e1e1e;
    user-select: all;
    word-break: break-all;
  }
  .copy {
    background: #1a1a2a;
    border: 1px solid #2a2a3a;
    color: #b9b9ee;
    padding: 0.2rem 0.55rem;
    border-radius: 5px;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .copy:hover {
    background: #22223a;
  }
  .hint {
    font-size: 0.72rem;
    color: #666;
    line-height: 1.45;
  }

  .qr {
    flex-shrink: 0;
    background: #0d0d0d;
    border: 1px solid #1e1e1e;
    border-radius: 6px;
    padding: 0.35rem;
    width: 132px;
    height: 132px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .qr :global(svg) {
    width: 100%;
    height: 100%;
    display: block;
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.8rem;
    color: #888;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #333;
  }
  .dot.active {
    background: #6c6;
    box-shadow: 0 0 6px #6c6a;
  }

  .port-row {
    display: flex;
    align-items: center;
    gap: 0.65rem;
    font-size: 0.78rem;
    color: #888;
  }
  .port-row label {
    color: #aaa;
  }
  .port-row input {
    width: 6rem;
    background: #131313;
    border: 1px solid #222;
    color: #e8e8e8;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.3rem 0.5rem;
    border-radius: 5px;
  }
  .port-row input:focus {
    outline: none;
    border-color: #3a3a55;
  }
  .port-hint {
    color: #666;
  }

  .error {
    color: #d66;
    font-size: 0.78rem;
  }
</style>
