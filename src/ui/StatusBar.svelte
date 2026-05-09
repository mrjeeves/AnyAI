<script lang="ts">
  import type { Mode } from "../types";

  let { model, mode, family, onOpenSettings } = $props<{
    model: string;
    mode: Mode;
    family: string;
    onOpenSettings: (tab: "providers" | "families" | "models") => void;
  }>();
</script>

<div class="status-bar">
  <button class="provider-btn" onclick={() => onOpenSettings("families")} title="Change family / model">
    <span class="dot"></span>
    {#if family}
      <span class="family-name">{family}</span>
      <span class="separator">·</span>
    {/if}
    <span class="model-name">{model}</span>
  </button>
  <div class="spacer"></div>
  <button class="models-btn" onclick={() => onOpenSettings("providers")} title="Open settings">
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
  .models-btn:hover { background: #1a1a1a; color: #ccc; }
  .grid-icon { font-size: .85rem; line-height: 1; }
  .gear-icon { display: block; }
  .label { line-height: 1; }
</style>
