import { fetch } from "@tauri-apps/plugin-http";
import { readTextFile, writeTextFile, exists, mkdir, remove } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import { loadConfig, saveConfig } from "./config";
import type { Source, ProviderCatalog } from "./types";

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

export async function fetchSourceCatalog(url: string): Promise<ProviderCatalog> {
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
  // Invalidate cache by deleting the cached file.
  try {
    const dir = await sourceCacheDir();
    const path = `${dir}/${cacheKey(source.url)}.json`;
    if (await exists(path)) await remove(path);
  } catch {}
  await fetchSourceCatalog(source.url);
}
