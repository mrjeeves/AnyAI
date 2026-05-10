import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { transcribeUi } from "./transcribe-state.svelte";
import { loadConversation, saveConversation } from "../conversations";

/** Chat-model slot. Exactly one occupant — a streaming chat (`kind: "chat"`)
 *  or a Talking Points loop (`kind: "tp"`) — at any given time. The Text
 *  mode button surfaces this state; the rest of the app uses it to enforce
 *  the singleton (no two chats, no chat while TP runs, no TP while chat
 *  streams). */
export const chatSlot = $state({
  /** `null` when the slot is free. */
  kind: null as null | "chat" | "tp",
  /** Conversation that owns this occupancy. Lets the sidebar /
   *  ModeBar resolve a human-readable label without a round-trip. */
  conversationId: null as string | null,
  conversationTitle: "" as string,
  /** `running` while inference is in flight or a TP cycle is summarising;
   *  `paused` when the user clicked pause and we're holding the slot but
   *  not consuming the model. Pause is a soft mutex — it does not kill an
   *  in-flight stream. */
  status: "idle" as "idle" | "running" | "paused",
  /** Unix ms when this occupancy was claimed. */
  startedAt: 0,
  /** Wall-clock seconds since `startedAt`, paused-time excluded. */
  elapsed: 0,
  /** Active ollama stream id when `kind === "chat"`, so a force-stop from
   *  the ModeBar can call `ollama_chat_cancel` directly. */
  streamId: null as string | null,
});

let elapsedTimer: ReturnType<typeof setInterval> | null = null;

function clearTimers() {
  if (elapsedTimer) clearInterval(elapsedTimer);
  elapsedTimer = null;
}

function startTimer() {
  clearTimers();
  chatSlot.startedAt = Date.now();
  chatSlot.elapsed = 0;
  elapsedTimer = setInterval(() => {
    if (chatSlot.status !== "running") return;
    chatSlot.elapsed = Math.floor((Date.now() - chatSlot.startedAt) / 1000);
  }, 250);
}

/** Claim the slot for a chat. Caller is responsible for having checked the
 *  slot was free (via `chatSlot.kind === null`); this function is a state
 *  transition, not an enforcement point. */
export function claimChat(args: {
  conversationId: string;
  conversationTitle: string;
  streamId: string;
}): void {
  chatSlot.kind = "chat";
  chatSlot.conversationId = args.conversationId;
  chatSlot.conversationTitle = args.conversationTitle;
  chatSlot.streamId = args.streamId;
  chatSlot.status = "running";
  startTimer();
}

/** Force-cancel the active chat stream (if any). Returns true if a cancel
 *  was actually issued. The slot is released regardless so the UI unsticks
 *  immediately even if the Rust cmd errors. */
export async function forceStopChat(): Promise<boolean> {
  if (chatSlot.kind !== "chat") return false;
  const id = chatSlot.streamId;
  resetSlot();
  if (!id) return false;
  try {
    await invoke("ollama_chat_cancel", { streamId: id });
    return true;
  } catch {
    return false;
  }
}

/** Release the chat slot after a chat stream finishes (naturally or
 *  cancelled). No-op if the slot is held by something other than this
 *  conversation's chat — TP holding the slot must be released through
 *  `stopTalkingPoints`. */
export function releaseChat(conversationId: string): void {
  if (chatSlot.kind !== "chat") return;
  if (chatSlot.conversationId !== conversationId) return;
  resetSlot();
}

function resetSlot() {
  clearTimers();
  chatSlot.kind = null;
  chatSlot.conversationId = null;
  chatSlot.conversationTitle = "";
  chatSlot.streamId = null;
  chatSlot.status = "idle";
  chatSlot.startedAt = 0;
  chatSlot.elapsed = 0;
}

// ---------------------------------------------------------------------
// Talking Points loop. Continuously re-summarises the live transcript of
// whichever conversation owns the transcribe slot, writing into that
// conversation's `talking_points`. Holds the chat slot for its lifetime.
// ---------------------------------------------------------------------

/** Tick cadence. We poll the transcript every few seconds and decide on
 *  each tick whether the words- or silence-based trigger has fired. Set
 *  small enough that the silence window feels responsive without spamming
 *  whisper with re-reads of the conversation file. */
const TP_TICK_MS = 3_000;
/** Words-based trigger: re-summarise as soon as this many new chars have
 *  accrued since the last cycle. Catches the "user is still talking"
 *  case so a long monologue still gets summarised mid-stream. */
const TP_MIN_NEW_CHARS = 120;
/** Silence-based trigger: if there's *any* new content since the last
 *  cycle and the transcript hasn't grown for this long, run a cycle. The
 *  ingest loop now drops silent chunks (no whisper hallucinations), so a
 *  flat transcript here genuinely means the speaker paused — a natural
 *  sentence/turn boundary worth summarising at. */
const TP_SILENCE_MS = 6_000;

let tpModel = "";
let tpInterval: ReturnType<typeof setInterval> | null = null;
let tpInFlightStreamId: string | null = null;
let tpInFlightUnlisten: UnlistenFn | null = null;
let tpLastTranscriptLen = 0;
/** Wall-clock ms when we last observed the transcript grow. Drives the
 *  silence trigger — `now - tpLastGrowthAt > TP_SILENCE_MS` means "the
 *  speaker has stopped, summarise what we have". */
let tpLastGrowthAt = 0;
let tpObservedLen = 0;

interface ChatStreamFrame {
  delta?: string;
  thinking_delta?: string;
  done?: boolean;
  cancelled?: boolean;
}

/** Activate Talking Points against the conversation currently in the
 *  transcribe slot. Caller has already checked the chat slot is free. */
export function startTalkingPoints(args: { model: string }): void {
  const convId = transcribeUi.conversationId;
  if (!convId) return;
  chatSlot.kind = "tp";
  chatSlot.conversationId = convId;
  chatSlot.conversationTitle = "Talking Points";
  chatSlot.status = "running";
  tpModel = args.model;
  tpLastTranscriptLen = 0;
  tpObservedLen = 0;
  tpLastGrowthAt = Date.now();
  startTimer();
  // Kick one immediate cycle so the user sees points show up without
  // waiting a full interval, then settle into the periodic cadence.
  void runTpCycle();
  tpInterval = setInterval(() => {
    if (chatSlot.kind !== "tp") return;
    if (chatSlot.status !== "running") return;
    void maybeRunTpCycle();
  }, TP_TICK_MS);
}

export function pauseTalkingPoints(): void {
  if (chatSlot.kind !== "tp") return;
  chatSlot.status = "paused";
}

export function resumeTalkingPoints(): void {
  if (chatSlot.kind !== "tp") return;
  if (chatSlot.status !== "paused") return;
  chatSlot.status = "running";
  // Realign so paused time doesn't poison the elapsed counter.
  chatSlot.startedAt = Date.now() - chatSlot.elapsed * 1000;
}

export async function stopTalkingPoints(): Promise<void> {
  if (chatSlot.kind !== "tp") return;
  if (tpInterval) clearInterval(tpInterval);
  tpInterval = null;
  // Best-effort: cancel any in-flight TP inference so we don't keep the
  // ollama daemon spinning after the user said stop.
  if (tpInFlightStreamId) {
    try {
      await invoke("ollama_chat_cancel", { streamId: tpInFlightStreamId });
    } catch {}
    tpInFlightUnlisten?.();
    tpInFlightUnlisten = null;
    tpInFlightStreamId = null;
  }
  resetSlot();
}

/** Tick handler: read the on-disk transcript, update growth bookkeeping,
 *  and decide whether the words- or silence-based trigger has fired.
 *  Kept separate from `runTpCycle` so cycles don't double-fire while a
 *  previous one is still in flight. */
async function maybeRunTpCycle(): Promise<void> {
  if (chatSlot.kind !== "tp") return;
  if (tpInFlightStreamId) return;
  const convId = chatSlot.conversationId;
  if (!convId) return;
  let conv;
  try {
    conv = await loadConversation(convId);
  } catch {
    return;
  }
  if (!conv) return;
  const transcript = (conv.transcript ?? "").trim();
  const len = transcript.length;
  if (len > tpObservedLen) {
    tpObservedLen = len;
    tpLastGrowthAt = Date.now();
  }
  if (!transcript) return;
  const newChars = len - tpLastTranscriptLen;
  if (newChars <= 0) return;
  const wordsTrigger = newChars >= TP_MIN_NEW_CHARS;
  const silenceTrigger = Date.now() - tpLastGrowthAt >= TP_SILENCE_MS;
  if (!wordsTrigger && !silenceTrigger) return;
  await runTpCycle();
}

async function runTpCycle(): Promise<void> {
  if (chatSlot.kind !== "tp") return;
  if (tpInFlightStreamId) return; // previous cycle still running
  const convId = chatSlot.conversationId;
  if (!convId) return;
  let conv;
  try {
    conv = await loadConversation(convId);
  } catch {
    return;
  }
  if (!conv) return;
  const transcript = (conv.transcript ?? "").trim();
  if (!transcript) return;

  const streamId = crypto.randomUUID();
  tpInFlightStreamId = streamId;
  let collected = "";
  try {
    tpInFlightUnlisten = await listen<ChatStreamFrame>(
      `myownllm://chat-stream/${streamId}`,
      (e) => {
        const f = e.payload;
        if (f.delta) collected += f.delta;
      },
    );
    await invoke("ollama_chat_stream", {
      streamId,
      model: tpModel,
      messages: [
        {
          role: "system",
          content:
            "You extract concise talking points from a live meeting transcript. " +
            "Reply with a bullet list, one talking point per line, prefixed with '- '. " +
            "Keep points short (< 12 words). No preamble, no commentary, just the bullets.",
        },
        {
          role: "user",
          content:
            "Summarise the following transcript into 3-7 talking points. " +
            "Focus on decisions, action items, and key facts.\n\n" +
            transcript,
        },
      ],
    });
    const points = parseBullets(collected);
    if (points.length > 0) {
      const fresh = await loadConversation(convId);
      if (fresh) {
        fresh.talking_points = points;
        await saveConversation(fresh);
      }
    }
    tpLastTranscriptLen = transcript.length;
  } catch (e) {
    console.warn("TP cycle failed:", e);
  } finally {
    tpInFlightUnlisten?.();
    tpInFlightUnlisten = null;
    tpInFlightStreamId = null;
  }
}

function parseBullets(raw: string): string[] {
  return raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .map((line) => line.replace(/^[-*•]\s*/, ""))
    .map((line) => line.replace(/^\d+[.)]\s*/, ""))
    .filter((line) => line.length > 0)
    .slice(0, 12);
}
