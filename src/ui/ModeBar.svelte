<script lang="ts">
  import type { Mode } from "../types";

  let { current, supported, tokensUsed, contextSize, onChange } = $props<{
    current: Mode;
    /** Modes the active manifest defines tiers for. Modes outside this set
     *  render disabled with an "(unsupported)" hint. */
    supported: Set<Mode>;
    /** Estimated tokens currently in context (history + draft). The bar
     *  shows it as `used / total` with a small ring, no tooltips needed. */
    tokensUsed: number;
    /** Model's reported context window. 0 means "not yet known" — we hide
     *  the saturation block in that case rather than render `0 / 0`. */
    contextSize: number;
    onChange: (mode: Mode) => void;
  }>();

  // Trimmed to text + transcribe to match the redesigned mode bar — vision
  // and code aren't surfaced in the GUI right now.
  const modes: Array<{ id: Mode; label: string }> = [
    { id: "text", label: "Text" },
    { id: "transcribe", label: "Transcribe" },
  ];

  const ratio = $derived(contextSize > 0 ? Math.min(1, tokensUsed / contextSize) : 0);

  // SVG ring geometry: circumference = 2πr. r=6 on a 16x16 canvas keeps the
  // stroke from clipping the bbox while leaving a 1px stroke ring readable.
  const RADIUS = 6;
  const CIRC = 2 * Math.PI * RADIUS;
  const dash = $derived(CIRC * ratio);

  /** Saturation-aware ring colour: green → amber → red as the context fills.
   *  Same thresholds the macOS battery icon uses, for familiarity. */
  const ringColor = $derived(
    ratio < 0.6 ? "#4caf50" : ratio < 0.85 ? "#d49a3b" : "#e35a5a",
  );

  /** Compact display: 1234 → "1.2k". Keeps the bar a fixed-ish width so
   *  the mode buttons don't shift as the conversation grows. */
  function fmt(n: number): string {
    if (n < 1000) return String(n);
    if (n < 10_000) return (n / 1000).toFixed(1).replace(/\.0$/, "") + "k";
    return Math.round(n / 1000) + "k";
  }
</script>

<div class="mode-bar">
  <div class="modes">
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

  {#if contextSize > 0}
    <div
      class="ctx"
      title="Context: {tokensUsed} / {contextSize} tokens"
      aria-label="Context saturation: {tokensUsed} of {contextSize} tokens"
    >
      <svg class="ring" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
        <circle cx="8" cy="8" r={RADIUS} fill="none" stroke="#2a2a2a" stroke-width="2" />
        <circle
          cx="8"
          cy="8"
          r={RADIUS}
          fill="none"
          stroke={ringColor}
          stroke-width="2"
          stroke-linecap="round"
          stroke-dasharray="{dash} {CIRC}"
          transform="rotate(-90 8 8)"
        />
      </svg>
      <span class="num">{fmt(tokensUsed)}</span>
      <span class="sep">/</span>
      <span class="den">{fmt(contextSize)}</span>
    </div>
  {/if}
</div>

<style>
  .mode-bar {
    display: flex;
    align-items: center;
    gap: .5rem;
    padding: .45rem .75rem;
    background: #0f0f0f;
    border-top: 1px solid #1a1a1a;
  }
  .modes { display: flex; gap: .35rem; flex: 1; min-width: 0; }
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
  .ctx {
    display: inline-flex;
    align-items: center;
    gap: .3rem;
    color: #777;
    font-size: .72rem;
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    user-select: none;
    flex-shrink: 0;
  }
  .ring { display: block; }
  .num { color: #aaa; }
  .sep { color: #444; }
  .den { color: #666; }
</style>
