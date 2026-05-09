<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  interface PendingUpdate {
    version: string;
    staged_at: string;
  }

  interface UpdateStatus {
    current_version: string;
    install_kind: "raw" | "package_manager";
    enabled: boolean;
    channel: string;
    auto_apply: string;
    check_interval_hours: number;
    last_check_unix: number | null;
    pending: PendingUpdate | null;
  }

  type CheckOutcome =
    | { kind: "disabled" }
    | { kind: "package_manager" }
    | { kind: "up_to_date"; current: string; latest: string }
    | { kind: "staged"; version: string }
    | { kind: "policy_blocked"; current: string; latest: string; policy: string };

  let status = $state<UpdateStatus | null>(null);
  let loading = $state(true);
  let checking = $state(false);
  let outcome = $state<CheckOutcome | null>(null);
  let error = $state<string>("");

  onMount(refresh);

  async function refresh() {
    loading = true;
    error = "";
    try {
      status = await invoke<UpdateStatus>("update_status");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function checkNow() {
    checking = true;
    outcome = null;
    error = "";
    try {
      outcome = await invoke<CheckOutcome>("update_check_now");
      status = await invoke<UpdateStatus>("update_status");
    } catch (e) {
      error = String(e);
    } finally {
      checking = false;
    }
  }

  async function applyNow() {
    try {
      await invoke("update_apply_now");
    } catch (e) {
      error = String(e);
    }
  }

  function formatTimestamp(unix: number | null): string {
    if (!unix) return "never";
    const ms = Date.now() - unix * 1000;
    const mins = Math.floor(ms / 60_000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }

  function formatStagedAt(iso: string): string {
    if (!iso || iso === "?") return "?";
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }

  function installKindLabel(kind: string): string {
    return kind === "package_manager" ? "package manager" : "raw binary";
  }
</script>

<div class="section">
  {#if loading}
    <div class="loading">Loading…</div>
  {:else if error && !status}
    <div class="error">{error}</div>
  {:else if status}
    <div class="content">
      <div class="header-row">
        <div>
          <div class="version">anyai {status.current_version}</div>
          <div class="meta">
            installed via {installKindLabel(status.install_kind)} ·
            {status.channel} channel
          </div>
        </div>
        <button class="check-btn" onclick={checkNow} disabled={checking || !status.enabled}>
          {checking ? "Checking…" : "Check for updates"}
        </button>
      </div>

      {#if !status.enabled}
        <div class="notice warn">
          Auto-update is disabled in <code>~/.anyai/config.json</code>
          (<code>auto_update.enabled = false</code>). Re-enable it to use this tab.
        </div>
      {/if}

      {#if status.install_kind === "package_manager"}
        <div class="notice warn">
          AnyAI was installed via a package manager (Homebrew, apt, rpm, MSI, Chocolatey).
          Use your package manager to upgrade — self-update is intentionally disabled here.
        </div>
      {/if}

      <dl class="info">
        <div>
          <dt>Last checked</dt>
          <dd>{formatTimestamp(status.last_check_unix)}</dd>
        </div>
        <div>
          <dt>Auto-apply policy</dt>
          <dd><code>{status.auto_apply}</code></dd>
        </div>
        <div>
          <dt>Check interval</dt>
          <dd>{status.check_interval_hours}h</dd>
        </div>
      </dl>

      {#if status.pending}
        <div class="pending">
          <div class="pending-head">
            <span class="badge">Update staged</span>
            <strong>{status.pending.version}</strong>
          </div>
          <div class="pending-meta">
            staged {formatStagedAt(status.pending.staged_at)} — restart AnyAI to apply.
          </div>
          <button class="apply-btn" onclick={applyNow}>Restart &amp; apply now</button>
        </div>
      {/if}

      {#if outcome}
        <div class="outcome">
          {#if outcome.kind === "disabled"}
            Self-update is disabled.
          {:else if outcome.kind === "package_manager"}
            Package-manager install — self-update deferred to the system updater.
          {:else if outcome.kind === "up_to_date"}
            {#if outcome.current === outcome.latest}
              Already on the latest version ({outcome.latest}).
            {:else}
              You're on <strong>{outcome.current}</strong> — ahead of latest published
              ({outcome.latest}).
            {/if}
          {:else if outcome.kind === "staged"}
            <strong>{outcome.version}</strong> downloaded and staged. Restart to apply.
          {:else if outcome.kind === "policy_blocked"}
            <strong>{outcome.latest}</strong> is available but
            <code>auto_apply = "{outcome.policy}"</code> doesn't permit this jump from
            {outcome.current}. Edit <code>~/.anyai/config.json</code> to allow it.
          {/if}
        </div>
      {/if}

      {#if error}
        <div class="error">{error}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .section { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  .loading, .error { padding: 2rem; text-align: center; color: #555; font-size: .85rem; }
  .error { color: #d66; }
  .content {
    padding: 1rem 1.1rem;
    overflow-y: auto;
    display: flex; flex-direction: column; gap: 1rem;
  }
  .header-row {
    display: flex; align-items: center; justify-content: space-between; gap: 1rem;
  }
  .version { font-size: 1rem; color: #e8e8e8; font-weight: 600; }
  .meta { font-size: .75rem; color: #666; margin-top: .15rem; }
  .check-btn {
    padding: .45rem .85rem;
    background: #1a1a2a;
    border: 1px solid #2a2a3a;
    color: #e8e8e8;
    border-radius: 6px;
    font-size: .8rem;
    cursor: pointer;
  }
  .check-btn:hover:not(:disabled) { background: #22223a; border-color: #3a3a55; }
  .check-btn:disabled { opacity: .4; cursor: default; }
  .notice {
    padding: .55rem .8rem;
    border-radius: 7px;
    font-size: .78rem;
    line-height: 1.45;
  }
  .notice.warn { background: #2a220e; color: #d6b25a; border: 1px solid #3a2e0e; }
  .notice code { font-family: monospace; font-size: .76rem; color: inherit; }
  .info {
    margin: 0;
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: .75rem;
    padding: .75rem;
    background: #131313;
    border: 1px solid #1e1e1e;
    border-radius: 7px;
  }
  .info > div { display: flex; flex-direction: column; gap: .15rem; min-width: 0; }
  dt { font-size: .68rem; color: #666; text-transform: uppercase; letter-spacing: .03em; }
  dd { margin: 0; font-size: .82rem; color: #ccc; }
  dd code { font-family: monospace; font-size: .76rem; color: #9a7; }
  .pending {
    background: #14221a;
    border: 1px solid #1e3325;
    border-radius: 7px;
    padding: .75rem .85rem;
    display: flex; flex-direction: column; gap: .4rem;
  }
  .pending-head { display: flex; align-items: center; gap: .55rem; }
  .pending-head strong { font-family: monospace; color: #6c6; font-size: .9rem; }
  .badge {
    background: #1f3a26; color: #6c6; font-size: .68rem;
    padding: .12rem .45rem; border-radius: 4px;
    text-transform: uppercase; letter-spacing: .04em;
  }
  .pending-meta { font-size: .74rem; color: #788; }
  .apply-btn {
    align-self: flex-start;
    margin-top: .25rem;
    padding: .4rem .85rem;
    background: #1f3a26;
    border: 1px solid #2c5135;
    color: #cfeacf;
    border-radius: 6px;
    font-size: .78rem;
    cursor: pointer;
  }
  .apply-btn:hover { background: #28492f; }
  .outcome {
    padding: .6rem .8rem;
    background: #131820;
    border: 1px solid #1e2530;
    border-radius: 7px;
    font-size: .8rem;
    color: #aac;
    line-height: 1.5;
  }
  .outcome strong { color: #e8e8e8; font-family: monospace; }
  .outcome code { font-family: monospace; font-size: .76rem; color: #d6b25a; }
</style>
