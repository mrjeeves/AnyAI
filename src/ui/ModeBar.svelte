<script lang="ts">
  import type { Mode } from "../types";

  let { current, supported, onChange } = $props<{
    current: Mode;
    /** Modes the active manifest defines tiers for. Modes outside this set
     * render disabled with an "(unsupported)" hint — useful for transcribe,
     * which has no working Ollama model in the default manifest. */
    supported: Set<Mode>;
    onChange: (mode: Mode) => void;
  }>();

  const modes: Array<{ id: Mode; label: string }> = [
    { id: "text",      label: "Text"      },
    { id: "vision",    label: "Vision"    },
    { id: "code",      label: "Code"      },
    { id: "transcribe",label: "Transcribe"},
  ];
</script>

<div class="mode-bar">
  {#each modes as m}
    {@const ok = supported.has(m.id)}
    <button
      class:active={m.id === current}
      class:unsupported={!ok}
      disabled={!ok}
      title={ok ? "" : `${m.label} isn't in the active manifest — no model is recommended for it.`}
      onclick={() => ok && onChange(m.id)}
    >
      {m.label}{!ok ? " · unsupported" : ""}
    </button>
  {/each}
</div>

<style>
  .mode-bar {
    display: flex;
    gap: .35rem;
    padding: .45rem .75rem;
    background: #0f0f0f;
    border-top: 1px solid #1a1a1a;
  }
  button {
    padding: .3rem .75rem;
    background: none;
    border: 1px solid #2a2a2a;
    border-radius: 20px;
    color: #666;
    font-size: .8rem;
    cursor: pointer;
    transition: all .15s;
  }
  button:hover:not(:disabled) { border-color: #444; color: #ccc; }
  button.active { background: #6e6ef7; border-color: #6e6ef7; color: #fff; font-weight: 500; }
  button.unsupported {
    opacity: .45;
    cursor: not-allowed;
    font-style: italic;
  }
</style>
