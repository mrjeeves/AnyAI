import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Frame shape from the Rust side. Mirror of `transcribe::TranscribeFrame`
 *  in src-tauri — keep these in sync. */
interface TranscribeFrame {
  delta: string;
  elapsed_ms: number;
  final: boolean;
  pending_chunks?: number;
  /** Ephemeral status message ("Loading whisper model…", "Low mic level",
   *  whisper errors, …). Present only when something noteworthy is
   *  happening — a normal text frame omits it, which clears the rendered
   *  status. */
  status?: string | null;
}

/** Per-stream pending entry returned by the recovery probe. Mirror of
 *  `transcribe::PendingStream`. */
export interface PendingStream {
  stream_id: string;
  pending_chunks: number;
  model: string | null;
}

/** Global transcribe state lives at module scope so it survives any one
 *  view's mount/unmount cycle. The status bar reads it from every mode
 *  so users can see + control a running session even when they've
 *  switched away from Transcribe. Svelte 5 `$state` makes this reactive
 *  without explicit subscribe wiring. */
export const transcribeUi = $state({
  /** True while a session is in flight (capturing or post-stop draining). */
  active: false,
  /** True when the user has explicitly paused mic capture. The inference
   *  loop keeps draining the backlog regardless. */
  paused: false,
  /** Drain-only sessions never had a mic — used by the StatusBar to
   *  hide pause/resume controls and the "MM:SS" capture timer that
   *  would lie about how long this session has been "running". */
  drainOnly: false,
  /** Upload-only sessions are file-driven, no mic. Same hiding rules
   *  as drainOnly but the StatusBar wording is "Transcribing…" instead
   *  of "Recovering…". The two flags are mutually exclusive. */
  uploadOnly: false,
  streamId: null as string | null,
  /** Whisper model name without the "whisper:" prefix. We need it to
   *  start a drain session and to label the pending state in the bar. */
  model: "" as string,
  /** Conversation that receives delta text. When the active view
   *  conversation matches, TranscribeView appends `liveDelta` to the
   *  rendered transcript so the user sees text arrive even after a
   *  mode-switch round trip. */
  conversationId: null as string | null,
  startedAt: 0,
  /** Capture wall-clock seconds since `startedAt`. The status bar shows
   *  it next to the rec dot the same way the in-pane chrome used to. */
  elapsed: 0,
  /** Whisper backlog. > 0 means inference is behind realtime — surface
   *  to the user as "X s behind" so they don't think we're stuck. */
  pendingChunks: 0,
  /** Text that has streamed in since `liveDelta` was last consumed by
   *  TranscribeView. The view appends + clears on each frame so the
   *  transcript stays the canonical store-of-truth and we don't have to
   *  buffer per-conversation here. */
  liveDelta: "",
  /** True for one tick after every frame so consumers can $effect on
   *  it without having to inspect string length changes that race
   *  against same-text reappends. */
  framePulse: 0,
  /** Ephemeral subtitle ("Loading whisper model…", "Low mic level…",
   *  whisper errors). Empty when the session is producing normal text;
   *  rendered under the transcript so the user can see WHY the
   *  transcript is idle. */
  status: "" as string,
  error: "" as string,
});

let unlistenStream: UnlistenFn | null = null;
let elapsedTimer: ReturnType<typeof setInterval> | null = null;
/** Resolver for the in-flight stop() promise. The Rust worker emits a
 *  `final` frame after teardown; we hold the caller in `await` until
 *  that arrives so a follow-up persist() can't race the last delta. */
let stopResolver: (() => void) | null = null;

function clearTimers() {
  if (elapsedTimer) clearInterval(elapsedTimer);
  elapsedTimer = null;
}

function resetState() {
  transcribeUi.active = false;
  transcribeUi.paused = false;
  transcribeUi.drainOnly = false;
  transcribeUi.uploadOnly = false;
  transcribeUi.streamId = null;
  transcribeUi.model = "";
  transcribeUi.conversationId = null;
  transcribeUi.startedAt = 0;
  transcribeUi.elapsed = 0;
  transcribeUi.pendingChunks = 0;
  transcribeUi.liveDelta = "";
  transcribeUi.status = "";
}

async function attachListener(streamId: string) {
  unlistenStream = await listen<TranscribeFrame>(
    `myownllm://transcribe-stream/${streamId}`,
    (e) => {
      const f = e.payload;
      if (f.delta) {
        transcribeUi.liveDelta = transcribeUi.liveDelta + f.delta;
        transcribeUi.framePulse++;
      }
      if (typeof f.pending_chunks === "number") {
        transcribeUi.pendingChunks = f.pending_chunks;
      }
      // A frame with no `status` field clears the subtitle — the Rust
      // side omits `status` on normal text frames specifically so the
      // "Loading whisper model…" / "Low mic level" line disappears once
      // real transcription starts flowing.
      transcribeUi.status = typeof f.status === "string" ? f.status : "";
      if (f.final) {
        // Worker unwound (cancel or natural end). Tear down our side.
        clearTimers();
        unlistenStream?.();
        unlistenStream = null;
        // Hold the live transcript in place for one tick so the view
        // can flush it; the stopRecording / drainStart caller is
        // responsible for resetting once it's persisted.
        transcribeUi.active = false;
        const r = stopResolver;
        stopResolver = null;
        r?.();
      }
    },
  );
}

export interface StartArgs {
  model: string;
  device: string | null;
  conversationId: string | null;
}

export async function startRecording(args: StartArgs): Promise<void> {
  if (transcribeUi.active) return;
  transcribeUi.error = "";
  const streamId = crypto.randomUUID();
  await attachListener(streamId);
  try {
    await invoke("transcribe_start", {
      streamId,
      model: args.model,
      device: args.device,
    });
  } catch (e) {
    unlistenStream?.();
    unlistenStream = null;
    transcribeUi.error = String(e);
    throw e;
  }
  transcribeUi.active = true;
  transcribeUi.paused = false;
  transcribeUi.drainOnly = false;
  transcribeUi.uploadOnly = false;
  transcribeUi.streamId = streamId;
  transcribeUi.model = args.model;
  transcribeUi.conversationId = args.conversationId;
  transcribeUi.startedAt = Date.now();
  transcribeUi.elapsed = 0;
  transcribeUi.pendingChunks = 0;
  transcribeUi.liveDelta = "";
  elapsedTimer = setInterval(() => {
    if (transcribeUi.paused) return;
    transcribeUi.elapsed = Math.floor((Date.now() - transcribeUi.startedAt) / 1000);
  }, 250);
}

/** Spin up an inference-only session against an audio file the user
 *  picked. The mic is never touched; the Rust side decodes the file with
 *  symphonia and runs whisper on each 5-second chunk. */
export async function startUpload(args: {
  model: string;
  filePath: string;
  conversationId: string | null;
}): Promise<void> {
  if (transcribeUi.active) return;
  transcribeUi.error = "";
  const streamId = crypto.randomUUID();
  await attachListener(streamId);
  try {
    await invoke("transcribe_upload_start", {
      streamId,
      model: args.model,
      filePath: args.filePath,
    });
  } catch (e) {
    unlistenStream?.();
    unlistenStream = null;
    transcribeUi.error = String(e);
    throw e;
  }
  transcribeUi.active = true;
  transcribeUi.paused = false;
  transcribeUi.drainOnly = false;
  transcribeUi.uploadOnly = true;
  transcribeUi.streamId = streamId;
  transcribeUi.model = args.model;
  transcribeUi.conversationId = args.conversationId;
  transcribeUi.startedAt = Date.now();
  transcribeUi.elapsed = 0;
  transcribeUi.pendingChunks = 0;
  transcribeUi.liveDelta = "";
}

export async function pauseRecording(): Promise<void> {
  if (!transcribeUi.active || transcribeUi.paused || transcribeUi.drainOnly) return;
  if (!transcribeUi.streamId) return;
  await invoke("transcribe_pause", { streamId: transcribeUi.streamId });
  transcribeUi.paused = true;
}

export async function resumeRecording(): Promise<void> {
  if (!transcribeUi.active || !transcribeUi.paused) return;
  if (!transcribeUi.streamId) return;
  await invoke("transcribe_resume", { streamId: transcribeUi.streamId });
  transcribeUi.paused = false;
  // Realign the wall-clock so paused time doesn't show in the elapsed
  // counter — without this, the timer jumps forward by however long the
  // user was paused the first time it ticks after resume.
  transcribeUi.startedAt = Date.now() - transcribeUi.elapsed * 1000;
}

/** Cancel the running session. Resolves once the Rust worker has emitted
 *  its final frame, so callers can safely persist the transcript right
 *  after `await`. */
export async function stopRecording(): Promise<void> {
  const id = transcribeUi.streamId;
  if (!id) return;
  const done = new Promise<void>((resolve) => {
    stopResolver = resolve;
  });
  try {
    await invoke("transcribe_stop", { streamId: id });
  } catch (e) {
    console.warn("transcribe_stop failed:", e);
    // Backend may already be gone — treat as done so the UI unsticks.
    const r = stopResolver;
    stopResolver = null;
    r?.();
  }
  await done;
}

/** Spin up an inference-only session against a stream id whose buffer
 *  dir already has chunks (from a previous MyOwnLLM process that crashed
 *  or was force-quit). The mic is never touched. */
export async function startDrain(args: {
  streamId: string;
  model: string;
  conversationId: string | null;
}): Promise<void> {
  if (transcribeUi.active) return;
  transcribeUi.error = "";
  await attachListener(args.streamId);
  try {
    await invoke("transcribe_drain_start", {
      streamId: args.streamId,
      model: args.model,
    });
  } catch (e) {
    unlistenStream?.();
    unlistenStream = null;
    transcribeUi.error = String(e);
    throw e;
  }
  transcribeUi.active = true;
  transcribeUi.paused = false;
  transcribeUi.drainOnly = true;
  transcribeUi.uploadOnly = false;
  transcribeUi.streamId = args.streamId;
  transcribeUi.model = args.model;
  transcribeUi.conversationId = args.conversationId;
  transcribeUi.startedAt = Date.now();
  transcribeUi.elapsed = 0;
  transcribeUi.pendingChunks = 0;
  transcribeUi.liveDelta = "";
}

export function clearLiveDelta(): void {
  transcribeUi.liveDelta = "";
}

export function clearAfterPersist(): void {
  resetState();
}
