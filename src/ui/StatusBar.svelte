<script lang="ts">
  import { updateUi } from "../update-state.svelte";
  import type { Mode } from "../types";

  let { model, mode, family, sidebarOpen, onToggleSidebar, onOpenSettings } = $props<{
    model: string;
    mode: Mode;
    family: string;
    sidebarOpen: boolean;
    onToggleSidebar: () => void;
    /** `opts.detailFamily` opens the Families tab straight into that
     *  family's tier ladder (Switch / Un-switch / Download per tier).
     *  Only honoured when `tab === "families"`. */
    onOpenSettings: (
      tab: "providers" | "families" | "models" | "storage" | "updates",
      opts?: { detailFamily?: string },
    ) => void;
  }>();

  // If the update dot is showing, the user almost certainly clicked the
  // Settings button *because* of it — drop them on the Updates tab so they
  // don't have to dig through the sidebar to find what they came for.
  function openSettings() {
    onOpenSettings(updateUi.available ? "updates" : "providers");
  }

  /** Click handler for the "{family} · {model}" pill. When we know the
   *  active family, land the user directly on its tier ladder so
   *  switching the model is one click away. Without an active family
   *  (provider not picked yet, manifest unresolved, etc.) fall back to
   *  the family list so they can still pick one. */
  function openFamily() {
    if (family) {
      onOpenSettings("families", { detailFamily: family });
    } else {
      onOpenSettings("families");
    }
  }
</script>

<div class="status-bar">
  <button
    class="hamburger"
    onclick={onToggleSidebar}
    title={sidebarOpen ? "Hide conversations" : "Show conversations"}
    aria-label="Toggle conversations"
    aria-expanded={sidebarOpen}
  >
    <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
      <path
        fill="currentColor"
        d="M3 6h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2zm0 5h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2zm0 5h18a1 1 0 1 1 0 2H3a1 1 0 1 1 0-2z"
      />
    </svg>
  </button>
  <button class="provider-btn" onclick={openFamily} title="Switch the model used for this family">
    <span class="dot"></span>
    {#if family}
      <span class="family-name">{family}</span>
      <span class="separator">·</span>
    {/if}
    <span class="model-name">{model}</span>
  </button>

  <div class="spacer"></div>
  <button
    class="models-btn"
    onclick={openSettings}
    title={updateUi.available
      ? `Update ${updateUi.available.version} available`
      : "Open settings"}
  >
    <span class="grid-icon" aria-hidden="true">⊞</span>
    <span class="label">Models/Settings</span>
    <svg
      class="gear-icon"
      viewBox="0 0 24 24"
      width="13"
      height="13"
      aria-hidden="true"
    >
      <path
        fill="currentColor"
        d="M19.43 12.98a7.7 7.7 0 0 0 0-1.96l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.5 7.5 0 0 0-1.7-.98l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.5.42l-.36 2.54a7.5 7.5 0 0 0-1.7.98l-2.39-.96a.5.5 0 0 0-.6.22L2.8 8.8a.5.5 0 0 0 .12.64l2.03 1.58a7.7 7.7 0 0 0 0 1.96L2.92 14.56a.5.5 0 0 0-.12.64l1.92 3.32a.5.5 0 0 0 .6.22l2.39-.96a7.5 7.5 0 0 0 1.7.98l.36 2.54a.5.5 0 0 0 .5.42h3.84a.5.5 0 0 0 .5-.42l.36-2.54a7.5 7.5 0 0 0 1.7-.98l2.39.96a.5.5 0 0 0 .6-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58zM12 15.5a3.5 3.5 0 1 1 0-7 3.5 3.5 0 0 1 0 7z"
      />
    </svg>
    {#if updateUi.available}
      <span
        class="update-dot"
        aria-label="Update {updateUi.available.version} available"
      ></span>
    {/if}
  </button>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    padding: .4rem .75rem;
    border-bottom: 1px solid #1a1a1a;
    background: #0d0d0d;
    gap: .5rem;
  }
  .hamburger {
    background: none;
    border: none;
    color: #777;
    cursor: pointer;
    padding: .25rem .35rem;
    border-radius: 5px;
    display: flex;
    align-items: center;
  }
  .hamburger:hover { background: #1a1a1a; color: #ccc; }
  .provider-btn {
    display: flex;
    align-items: center;
    gap: .4rem;
    background: none;
    border: none;
    color: #888;
    font-size: .78rem;
    font-family: monospace;
    cursor: pointer;
    padding: .2rem .5rem;
    border-radius: 5px;
    max-width: 60%;
  }
  .provider-btn:hover { background: #1a1a1a; color: #ccc; }
  .dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: #4caf50;
    box-shadow: 0 0 4px #4caf50;
    flex-shrink: 0;
  }
  .family-name { color: #6e6ef7; flex-shrink: 0; }
  .separator { color: #444; flex-shrink: 0; }
  .model-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .spacer { flex: 1; }
  .models-btn {
    display: flex;
    align-items: center;
    gap: .4rem;
    background: none;
    border: none;
    color: #555;
    font-size: .75rem;
    cursor: pointer;
    padding: .2rem .5rem;
    border-radius: 5px;
  }
  .models-btn { position: relative; }
  .models-btn:hover { background: #1a1a1a; color: #ccc; }
  .grid-icon { font-size: .85rem; line-height: 1; }
  .gear-icon { display: block; }
  .label { line-height: 1; }
  .update-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #f59e0b;
    box-shadow: 0 0 6px rgba(245, 158, 11, 0.7);
    flex-shrink: 0;
    margin-left: .15rem;
  }
</style>
