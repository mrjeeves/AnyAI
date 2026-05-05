<script lang="ts">
  import type { Mode } from "../types";

  let { model, mode, onOpenProviders, onOpenModels } = $props<{
    model: string;
    mode: Mode;
    onOpenProviders: () => void;
    onOpenModels: () => void;
  }>();
</script>

<div class="status-bar">
  <button class="provider-btn" onclick={onOpenProviders} title="Manage providers">
    <span class="dot"></span>
    <span class="model-name">{model}</span>
  </button>
  <div class="spacer"></div>
  <button class="models-btn" onclick={onOpenModels} title="Manage models">
    ⊞ Models
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
  }
  .provider-btn:hover { background: #1a1a1a; color: #ccc; }
  .dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: #4caf50;
    box-shadow: 0 0 4px #4caf50;
  }
  .model-name { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .spacer { flex: 1; }
  .models-btn {
    background: none;
    border: none;
    color: #555;
    font-size: .75rem;
    cursor: pointer;
    padding: .2rem .5rem;
    border-radius: 5px;
  }
  .models-btn:hover { background: #1a1a1a; color: #ccc; }
</style>
