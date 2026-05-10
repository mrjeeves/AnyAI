<script lang="ts">
  import ProvidersSection from "./settings/ProvidersSection.svelte";
  import FamiliesSection from "./settings/FamiliesSection.svelte";
  import ModelsSection from "./settings/ModelsSection.svelte";
  import StorageSection from "./settings/StorageSection.svelte";
  import HardwareSection from "./settings/HardwareSection.svelte";
  import UpdatesSection from "./settings/UpdatesSection.svelte";
  import RemoteSection from "./settings/RemoteSection.svelte";
  import TranscriptionSection from "./settings/TranscriptionSection.svelte";

  type Tab =
    | "providers"
    | "families"
    | "models"
    | "storage"
    | "hardware"
    | "remote"
    | "transcription"
    | "updates";

  let {
    initialTab = "families",
    onClose,
    onChanged,
  } = $props<{
    initialTab?: Tab;
    onClose: () => void;
    onChanged: () => void;
  }>();

  // svelte-ignore state_referenced_locally
  let active = $state<Tab>(initialTab);

  const tabs: Array<{ id: Tab; label: string }> = [
    { id: "families", label: "Family" },
    { id: "providers", label: "Providers" },
    { id: "models", label: "Models" },
    { id: "storage", label: "Storage" },
    { id: "hardware", label: "Hardware" },
    { id: "transcription", label: "Transcription" },
    { id: "remote", label: "Remote" },
    { id: "updates", label: "Updates" },
  ];
</script>

<div class="overlay" onclick={onClose} role="presentation"></div>
<div class="panel" role="dialog" aria-label="Settings">
  <div class="panel-header">
    <h2>Settings</h2>
    <button class="close" onclick={onClose} aria-label="Close">✕</button>
  </div>

  <div class="body">
    <nav class="v-tabs" aria-label="Settings sections">
      {#each tabs as t}
        <button class="v-tab" class:active={active === t.id} onclick={() => (active = t.id)}>
          {t.label}
        </button>
      {/each}
    </nav>

    <div class="content">
      {#if active === "families"}
        <FamiliesSection {onChanged} {onClose} />
      {:else if active === "providers"}
        <ProvidersSection {onChanged} />
      {:else if active === "models"}
        <ModelsSection />
      {:else if active === "storage"}
        <StorageSection setActive={(t) => (active = t)} />
      {:else if active === "hardware"}
        <HardwareSection setActive={(t) => (active = t)} />
      {:else if active === "transcription"}
        <TranscriptionSection />
      {:else if active === "remote"}
        <RemoteSection />
      {:else if active === "updates"}
        <UpdatesSection />
      {/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    z-index: 10;
  }
  .panel {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(820px, 92vw);
    height: min(620px, 88vh);
    background: #111;
    border: 1px solid #222;
    border-radius: 12px;
    z-index: 11;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
  }
  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid #1e1e1e;
    flex-shrink: 0;
  }
  h2 {
    font-size: 0.95rem;
    font-weight: 600;
  }
  .close {
    background: none;
    border: none;
    color: #666;
    font-size: 1rem;
    cursor: pointer;
    padding: 0.2rem 0.4rem;
    border-radius: 4px;
  }
  .close:hover {
    color: #ccc;
    background: #1a1a1a;
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .v-tabs {
    width: 160px;
    border-right: 1px solid #1e1e1e;
    background: #0d0d0d;
    display: flex;
    flex-direction: column;
    padding: 0.5rem 0.35rem;
    gap: 0.15rem;
    flex-shrink: 0;
  }
  .v-tab {
    text-align: left;
    background: none;
    border: none;
    color: #888;
    font-size: 0.85rem;
    cursor: pointer;
    padding: 0.5rem 0.65rem;
    border-radius: 6px;
    border-left: 2px solid transparent;
  }
  .v-tab:hover {
    background: #161616;
    color: #ccc;
  }
  .v-tab.active {
    color: #e8e8e8;
    background: #1a1a2a;
    border-left-color: #6e6ef7;
  }
  .content {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
</style>
