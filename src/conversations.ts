import {
  readTextFile,
  writeTextFile,
  exists,
  mkdir,
  readDir,
  remove,
} from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import { invoke } from "@tauri-apps/api/core";
import { loadConfig } from "./config";
import type { Mode } from "./types";

/**
 * One turn of a chat. Mirrors the in-memory `Message` shape used by the
 * chat panel; persisted to JSON so reloading a conversation is a verbatim
 * round-trip (including `thinking` blocks from reasoning models).
 */
export interface StoredMessage {
  role: "user" | "assistant";
  content: string;
  thinking?: string;
}

/** A whole conversation as it lives on disk (one JSON file per conversation). */
export interface Conversation {
  id: string;
  title: string;
  /** `text` or `transcribe` — the only modes the post-redesign UI exposes. */
  mode: Mode;
  /** Last model used. Stored for display / future reuse, not for routing. */
  model: string;
  /** Family at the time of last write. */
  family: string;
  created_at: string;
  updated_at: string;
  messages: StoredMessage[];
  /** Transcribe-mode artifacts. Empty / absent for text-mode conversations. */
  transcript?: string;
  talking_points?: string[];
}

/** Lightweight projection used by the sidebar list — avoids reading every
 *  message body just to render N rows of titles. */
export interface ConversationMeta {
  id: string;
  title: string;
  mode: Mode;
  updated_at: string;
}

async function conversationsDir(): Promise<string> {
  const cfg = await loadConfig();
  if (cfg.conversation_dir) return cfg.conversation_dir;
  const home = await homeDir();
  return `${home}/.anyai/conversations`;
}

async function ensureDir(): Promise<string> {
  const dir = await conversationsDir();
  await mkdir(dir, { recursive: true });
  return dir;
}

/** Crockford-ish base36 id. Time-prefixed so directory listings sort
 *  chronologically without a separate index file. */
export function newConversationId(): string {
  return Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 8);
}

/** Sidebar feed. Returns most-recent first. Bad / partial files are skipped
 *  silently — a corrupt conversation shouldn't take down the list. */
export async function listConversations(): Promise<ConversationMeta[]> {
  let dir: string;
  try {
    dir = await ensureDir();
  } catch {
    return [];
  }
  let entries: Awaited<ReturnType<typeof readDir>>;
  try {
    entries = await readDir(dir);
  } catch {
    return [];
  }
  const out: ConversationMeta[] = [];
  for (const e of entries) {
    if (!e.name || !e.name.endsWith(".json")) continue;
    try {
      const raw = await readTextFile(`${dir}/${e.name}`);
      const c = JSON.parse(raw) as Conversation;
      if (!c.id) continue;
      out.push({
        id: c.id,
        title: c.title || "Untitled",
        mode: c.mode,
        updated_at: c.updated_at || c.created_at || "",
      });
    } catch {
      // Skip unreadable / unparseable files.
    }
  }
  out.sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1));
  return out;
}

export async function loadConversation(id: string): Promise<Conversation | null> {
  try {
    const dir = await conversationsDir();
    const path = `${dir}/${id}.json`;
    if (!(await exists(path))) return null;
    return JSON.parse(await readTextFile(path)) as Conversation;
  } catch {
    return null;
  }
}

export async function saveConversation(c: Conversation): Promise<void> {
  const dir = await ensureDir();
  c.updated_at = new Date().toISOString();
  // Pretty-printed: these files are small and users may want to grep them.
  await writeTextFile(`${dir}/${c.id}.json`, JSON.stringify(c, null, 2));
}

export async function deleteConversation(id: string): Promise<void> {
  try {
    const dir = await conversationsDir();
    const path = `${dir}/${id}.json`;
    if (await exists(path)) await remove(path);
  } catch {
    // Silent — caller already removed the row from the sidebar.
  }
}

export async function renameConversation(id: string, title: string): Promise<void> {
  const c = await loadConversation(id);
  if (!c) return;
  c.title = title.trim().slice(0, 80) || "Untitled";
  await saveConversation(c);
}

/**
 * Read the shared active-conversation pointer from the backend. Mirror of
 * what the LAN remote pings via `GET /api/active-conversation` — both
 * surfaces use the same pointer so the two can hand off seamlessly.
 */
export async function getActiveConversationId(): Promise<string | null> {
  try {
    const id = await invoke<string | null>("get_active_conversation");
    return id ?? null;
  } catch {
    return null;
  }
}

/** Push a new active-conversation id to the backend (or `null` to clear).
 *  Idempotent on the backend — duplicate sets don't refire the change event. */
export async function setActiveConversationId(id: string | null): Promise<void> {
  try {
    await invoke("set_active_conversation", { id });
  } catch {
    // Best-effort: a transient backend hiccup shouldn't block the UI.
  }
}

export function newConversation(mode: Mode, model: string, family: string): Conversation {
  const now = new Date().toISOString();
  return {
    id: newConversationId(),
    title: "New chat",
    mode,
    model,
    family,
    created_at: now,
    updated_at: now,
    messages: [],
  };
}

/**
 * Coax a 3-5 word title out of the active model from the user's first
 * message. Tight `num_predict` ceiling and a low temperature so the daemon
 * doesn't spend visible seconds generating a heading nobody reads — title
 * generation is best-effort and falls back to a truncated message preview.
 */
export async function generateTitle(model: string, firstMessage: string): Promise<string> {
  const seed = firstMessage.trim().slice(0, 240);
  if (!seed) return "New chat";
  try {
    const reply = await invoke<string>("ollama_chat", {
      model,
      messages: [
        {
          role: "user",
          content:
            "Write a 3-5 word title for a chat that opens with the message below. " +
            "Reply with ONLY the title — no quotes, no punctuation, no preamble.\n\n" +
            seed,
        },
      ],
      options: { num_predict: 16, temperature: 0.3 },
    });
    return cleanTitle(reply) || fallbackTitle(seed);
  } catch {
    return fallbackTitle(seed);
  }
}

/** Strip thinking/reasoning leakage and surrounding punctuation, then clamp. */
function cleanTitle(raw: string): string {
  let t = raw.replace(/<think>[\s\S]*?<\/think>/gi, "").trim();
  // Some models prepend "Title:" or wrap in quotes — peel them off.
  t = t.replace(/^title\s*[:\-]\s*/i, "").trim();
  t = t.replace(/^["'`]+|["'`]+$/g, "").trim();
  // First line only — multi-line titles look broken in the sidebar.
  t = t.split(/\r?\n/, 1)[0]!.trim();
  if (t.length > 60) t = t.slice(0, 60).trimEnd() + "…";
  return t;
}

function fallbackTitle(seed: string): string {
  const flat = seed.replace(/\s+/g, " ").trim();
  return flat.length > 48 ? flat.slice(0, 48).trimEnd() + "…" : flat || "New chat";
}
