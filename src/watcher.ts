import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ModeSwapEvent {
  mode: string;
  from: string | null;
  to: string;
}

/**
 * Subscribe to background mode-swap notifications emitted by the Rust watcher.
 * Returns an unsubscribe function.
 */
export async function onModeSwap(handler: (e: ModeSwapEvent) => void): Promise<UnlistenFn> {
  return listen<ModeSwapEvent>("myownllm://mode-swap", (msg) => handler(msg.payload));
}
