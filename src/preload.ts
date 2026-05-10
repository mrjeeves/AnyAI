import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Mode } from "./types";

export interface PreloadEvent {
  mode: string;
  model: string;
  status: "resolved" | "pulling" | "pulled" | "warming" | "ready" | "error";
  detail: string;
}

export interface PreloadOpts {
  track?: boolean;
  warm?: boolean;
  onEvent?: (evt: PreloadEvent) => void;
}

/**
 * Preload one or more modes. Pulls and (optionally) warms each model.
 * Streams progress via Tauri events; resolves once every mode is ready or errored.
 */
export async function preloadModes(modes: Mode[], opts: PreloadOpts = {}): Promise<void> {
  let unlisten: UnlistenFn | null = null;
  if (opts.onEvent) {
    unlisten = await listen<PreloadEvent>("myownllm://preload-progress", (e) => {
      opts.onEvent?.(e.payload);
    });
  }
  try {
    await invoke("preload_modes", {
      modes,
      track: !!opts.track,
      warm: opts.warm !== false,
    });
  } finally {
    if (unlisten) unlisten();
  }
}

export async function ensureTrackedModels(warm = true): Promise<string[]> {
  return invoke<string[]>("ensure_tracked_models", { warm });
}
