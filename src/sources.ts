import { fetch } from "@tauri-apps/plugin-http";
import { readTextFile, writeTextFile, exists, mkdir, remove } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import { loadConfig, saveConfig } from "./config";
import type { Source, ProviderCatalog, CatalogEntry } from "./types";

const DEFAULT_SOURCE_TTL = 1440; // 24 hours

async function sourceCacheDir(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/cache/sources`;
}

function cacheKey(url: string): string {
  let h = 5381;
  for (let i = 0; i < url.length; i++) {
    h = ((h * 33) ^ url.charCodeAt(i)) >>> 0;
  }
  return h.toString(16);
}

interface CachedCatalog {
  fetched_at: string;
  catalog: ProviderCatalog;
}

async function readSourceCache(url: string): Promise<CachedCatalog | null> {
  try {
    const dir = await sourceCacheDir();
    const path = `${dir}/${cacheKey(url)}.json`;
    if (!(await exists(path))) return null;
    return JSON.parse(await readTextFile(path));
  } catch {
    return null;
  }
}

async function writeSourceCache(url: string, catalog: ProviderCatalog): Promise<void> {
  try {
    const dir = await sourceCacheDir();
    await mkdir(dir, { recursive: true });
    const path = `${dir}/${cacheKey(url)}.json`;
    const entry: CachedCatalog = { fetched_at: new Date().toISOString(), catalog };
    await writeTextFile(path, JSON.stringify(entry, null, 2));
  } catch {}
}

function isStale(cachedAt: string, ttlMinutes: number): boolean {
  return (Date.now() - new Date(cachedAt).getTime()) / 60_000 > ttlMinutes;
}

/**
 * Fetch a single catalog file (no import recursion). Honours its own ttl_minutes
 * via the on-disk cache. The shape returned is whatever the URL serves.
 */
async function fetchOne(url: string): Promise<ProviderCatalog> {
  const cached = await readSourceCache(url);
  if (cached) {
    const ttl = cached.catalog.ttl_minutes ?? DEFAULT_SOURCE_TTL;
    if (!isStale(cached.fetched_at, ttl)) return cached.catalog;
  }
  try {
    const resp = await fetch(url, { method: "GET", connectTimeout: 10000 });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const catalog = (await resp.json()) as ProviderCatalog;
    await writeSourceCache(url, catalog);
    return catalog;
  } catch {
    if (cached) return cached.catalog;
    throw new Error(`Could not fetch source at ${url}`);
  }
}

/**
 * Fetch a catalog and recursively merge in any `imports`. Each imported file is
 * fetched + cached against ITS OWN ttl_minutes — imports do not inherit a TTL
 * from the importing file. Cycles are detected by URL and broken silently.
 *
 * Merge order: imports first (in document order), then the importing file's own
 * providers. Later entries with the same `name` are skipped, so the closest
 * importer wins on collision. Each entry's `origin` is set to the URL of the
 * file that contributed it (for UI attribution).
 */
export async function fetchSourceCatalog(url: string): Promise<ProviderCatalog> {
  const visited = new Set<string>();
  const merged = await walk(url, visited);
  return merged;
}

async function walk(url: string, visited: Set<string>): Promise<ProviderCatalog> {
  if (visited.has(url)) {
    return { name: "", providers: [] };
  }
  visited.add(url);

  const raw = await fetchOne(url);

  const seenNames = new Set<string>();
  const out: CatalogEntry[] = [];

  // Imports first, depth-first.
  for (const importUrl of raw.imports ?? []) {
    let imported: ProviderCatalog;
    try {
      imported = await walk(importUrl, visited);
    } catch {
      continue; // Failure to fetch an import is non-fatal — skip and merge the rest.
    }
    for (const p of imported.providers ?? []) {
      if (seenNames.has(p.name)) continue;
      seenNames.add(p.name);
      out.push({ ...p, origin: p.origin ?? importUrl });
    }
  }

  // Then own providers (override imports on name collision — closer importer wins).
  for (const p of raw.providers ?? []) {
    if (seenNames.has(p.name)) {
      // Replace the imported entry: importing file is closer to user and wins.
      const idx = out.findIndex((e) => e.name === p.name);
      if (idx >= 0) out[idx] = { ...p, origin: url };
      continue;
    }
    seenNames.add(p.name);
    out.push({ ...p, origin: url });
  }

  return {
    name: raw.name,
    description: raw.description,
    ttl_minutes: raw.ttl_minutes,
    providers: out,
  };
}

export async function getSources(): Promise<Source[]> {
  const config = await loadConfig();
  return config.sources;
}

export async function addSource(source: Source): Promise<void> {
  const config = await loadConfig();
  const existing = config.sources.findIndex((s) => s.name === source.name);
  if (existing >= 0) {
    config.sources[existing].url = source.url;
  } else {
    config.sources.push(source);
  }
  await saveConfig(config);
}

export async function removeSource(name: string): Promise<void> {
  const config = await loadConfig();
  config.sources = config.sources.filter((s) => s.name !== name);
  await saveConfig(config);
}

export async function refreshSource(name: string): Promise<void> {
  const config = await loadConfig();
  const source = config.sources.find((s) => s.name === name);
  if (!source) throw new Error(`Source '${name}' not found`);
  // Invalidate cache by deleting the cached file. Imports are NOT recursively
  // invalidated — each obeys its own TTL and refreshes when due.
  try {
    const dir = await sourceCacheDir();
    const path = `${dir}/${cacheKey(source.url)}.json`;
    if (await exists(path)) await remove(path);
  } catch {}
  await fetchSourceCatalog(source.url);
}
