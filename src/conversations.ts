import {
  readTextFile,
  writeTextFile,
  exists,
  mkdir,
  readDir,
  remove,
  rename,
  type DirEntry,
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

/** A whole conversation as it lives on disk (one JSON file per conversation).
 *  The folder it lives in is the source of truth for its grouping — we don't
 *  store a `folder` field, the directory it sits under IS the folder. */
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
  /** Folder path from the conversations root (POSIX-style, no leading slash).
   *  Empty string = root. e.g. "Work/Projects". */
  path: string;
}

/** Folder entry, derived from the on-disk directory tree. */
export interface FolderMeta {
  /** Path from the conversations root, POSIX-style. e.g. "Work/Projects". */
  path: string;
}

async function conversationsDir(): Promise<string> {
  const cfg = await loadConfig();
  if (cfg.conversation_dir) return cfg.conversation_dir;
  const home = await homeDir();
  return `${home}/.myownllm/conversations`;
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

/** Reject path components that would escape the conversations root or break
 *  the sidebar (`..`, separators, hidden dotfiles). Names are otherwise free
 *  so users can drag in human-readable folder labels. */
export function sanitizeFolderName(name: string): string {
  return name
    .replace(/[/\\\x00-\x1f]/g, "")
    .replace(/^\.+/, "")
    .trim()
    .slice(0, 80);
}

/** Split a POSIX-style path into components, dropping empties and `..`s. */
function splitPath(path: string): string[] {
  return path
    .split("/")
    .map((p) => p.trim())
    .filter((p) => p && p !== "." && p !== "..");
}

function joinPath(parts: string[]): string {
  return parts.join("/");
}

interface WalkEntry {
  /** Absolute filesystem path of the .json file. */
  fullPath: string;
  /** Folder path from root (POSIX-style, "" for root). */
  folderPath: string;
}

/** Depth-first walk of the conversations tree. Skips entries we can't read
 *  rather than throwing — a single malformed subdir shouldn't break listing. */
async function walkTree(root: string): Promise<{
  files: WalkEntry[];
  folders: string[];
}> {
  const files: WalkEntry[] = [];
  const folders: string[] = [];
  async function visit(absDir: string, relPath: string) {
    let entries: DirEntry[];
    try {
      entries = await readDir(absDir);
    } catch {
      return;
    }
    for (const e of entries) {
      if (!e.name) continue;
      // Skip dotfiles / dotdirs so users editing on disk can stash notes
      // (`.DS_Store`, `.git`, etc.) without polluting the sidebar.
      if (e.name.startsWith(".")) continue;
      const childAbs = `${absDir}/${e.name}`;
      const childRel = relPath ? `${relPath}/${e.name}` : e.name;
      if (e.isDirectory) {
        folders.push(childRel);
        await visit(childAbs, childRel);
      } else if (e.isFile && e.name.endsWith(".json")) {
        files.push({ fullPath: childAbs, folderPath: relPath });
      }
    }
  }
  await visit(root, "");
  return { files, folders };
}

/** Sidebar feed. Returns most-recent first across the whole tree. Each row
 *  carries its folder path so the sidebar can render the nested layout
 *  without a second walk. */
export async function listConversations(): Promise<{
  conversations: ConversationMeta[];
  folders: FolderMeta[];
}> {
  let dir: string;
  try {
    dir = await ensureDir();
  } catch {
    return { conversations: [], folders: [] };
  }
  const { files, folders } = await walkTree(dir);
  const conversations: ConversationMeta[] = [];
  for (const f of files) {
    try {
      const raw = await readTextFile(f.fullPath);
      const c = JSON.parse(raw) as Conversation;
      if (!c.id) continue;
      conversations.push({
        id: c.id,
        title: c.title || "Untitled",
        mode: c.mode,
        updated_at: c.updated_at || c.created_at || "",
        path: f.folderPath,
      });
    } catch {
      // Skip unreadable / unparseable files.
    }
  }
  conversations.sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1));
  folders.sort();
  return {
    conversations,
    folders: folders.map((path) => ({ path })),
  };
}

/** Find the on-disk path of a conversation by id. We walk because the file
 *  may live in any subfolder; the index is small enough that walking on
 *  every load is cheap, and avoids a stale-cache class of bug. */
async function findConversationPath(id: string): Promise<string | null> {
  let dir: string;
  try {
    dir = await conversationsDir();
  } catch {
    return null;
  }
  const target = `${id}.json`;
  const { files } = await walkTree(dir);
  for (const f of files) {
    if (f.fullPath.endsWith(`/${target}`)) return f.fullPath;
  }
  return null;
}

/** Resolve `{convDir}/{folder}/{id}.json`, creating the folder if needed. */
async function pathFor(folder: string, id: string): Promise<string> {
  const root = await ensureDir();
  const parts = splitPath(folder);
  if (parts.length === 0) return `${root}/${id}.json`;
  const folderAbs = `${root}/${joinPath(parts)}`;
  await mkdir(folderAbs, { recursive: true });
  return `${folderAbs}/${id}.json`;
}

export async function loadConversation(id: string): Promise<Conversation | null> {
  try {
    const path = await findConversationPath(id);
    if (!path) return null;
    return JSON.parse(await readTextFile(path)) as Conversation;
  } catch {
    return null;
  }
}

/** Persist `c` under its current folder, falling back to `targetFolder` (or
 *  the root) if no existing file is found. Existing files keep their folder
 *  unless the caller explicitly moves them via `moveConversation`. */
export async function saveConversation(
  c: Conversation,
  targetFolder = "",
): Promise<void> {
  c.updated_at = new Date().toISOString();
  const existing = await findConversationPath(c.id);
  let path: string;
  if (existing) {
    path = existing;
  } else {
    path = await pathFor(targetFolder, c.id);
  }
  // Pretty-printed: these files are small and users may want to grep them.
  await writeTextFile(path, JSON.stringify(c, null, 2));
}

export async function deleteConversation(id: string): Promise<void> {
  try {
    const path = await findConversationPath(id);
    if (path && (await exists(path))) await remove(path);
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

/** Move a conversation file into the target folder (POSIX path from root,
 *  empty string for root). Creates the folder if needed; no-ops when the
 *  file is already there. */
export async function moveConversation(id: string, targetFolder: string): Promise<void> {
  const current = await findConversationPath(id);
  if (!current) return;
  const targetPath = await pathFor(targetFolder, id);
  if (current === targetPath) return;
  await rename(current, targetPath);
}

/** Create an empty folder at `path` (POSIX, from root). Components are
 *  sanitized to keep filesystem-hostile names out of the tree. */
export async function createFolder(path: string): Promise<void> {
  const root = await ensureDir();
  const parts = splitPath(path).map(sanitizeFolderName).filter(Boolean);
  if (parts.length === 0) return;
  await mkdir(`${root}/${joinPath(parts)}`, { recursive: true });
}

/** Rename / move a folder. Children move along with it because they live
 *  under the directory inode. */
export async function renameFolder(oldPath: string, newPath: string): Promise<void> {
  const root = await ensureDir();
  const oldParts = splitPath(oldPath);
  const newParts = splitPath(newPath).map(sanitizeFolderName).filter(Boolean);
  if (oldParts.length === 0 || newParts.length === 0) return;
  const oldAbs = `${root}/${joinPath(oldParts)}`;
  const newAbs = `${root}/${joinPath(newParts)}`;
  if (oldAbs === newAbs) return;
  // Make sure the destination's parent exists so we can drop it in.
  if (newParts.length > 1) {
    await mkdir(`${root}/${joinPath(newParts.slice(0, -1))}`, { recursive: true });
  }
  await rename(oldAbs, newAbs);
}

/** Delete a folder and everything under it. Use with caution — there's no
 *  trash; the sidebar gates this behind a confirm dialog. */
export async function deleteFolder(path: string): Promise<void> {
  const root = await ensureDir();
  const parts = splitPath(path);
  if (parts.length === 0) return; // Refuse to delete the root itself.
  const abs = `${root}/${joinPath(parts)}`;
  try {
    await remove(abs, { recursive: true });
  } catch {
    // Best-effort: caller already updated the UI.
  }
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
