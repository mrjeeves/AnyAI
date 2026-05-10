/**
 * Shared reactive state for the in-app update flow.
 *
 * Lives outside the component tree because three unrelated subtrees touch it:
 *   - App.svelte runs the startup check and renders the "apply now?" modal
 *   - Chat / TranscribeView own the SettingsPanel and need to be told when
 *     to open it on a particular tab (the user clicked "yes" on the modal)
 *   - SettingsPanel reads the available flag to draw an attention dot on the
 *     Updates tab, and clears it when the user opens that tab
 *
 * Keeping a single module-scoped $state object here avoids prop-drilling a
 * signal through App → Chat → StatusBar → SettingsPanel just for one dot.
 */

export type SettingsTab =
  | "providers"
  | "families"
  | "models"
  | "storage"
  | "hardware"
  | "remote"
  | "transcription"
  | "updates";

class UpdateUiState {
  /** Set when startup detects a release we can apply (already staged or just
   *  staged this session). Drives the attention dot on the Updates tab. */
  available = $state<{ version: string } | null>(null);

  /** Bumped to ask whichever view currently owns the SettingsPanel to open
   *  it on a specific tab. The nonce makes repeat requests for the same tab
   *  observable — without it $effect wouldn't re-fire if the user closes
   *  Settings and we try to re-open it on the same tab. */
  openSettingsRequest = $state<{ tab: SettingsTab; nonce: number } | null>(null);

  requestSettings(tab: SettingsTab) {
    this.openSettingsRequest = { tab, nonce: (this.openSettingsRequest?.nonce ?? 0) + 1 };
  }
}

export const updateUi = new UpdateUiState();
