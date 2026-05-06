import { fetch } from "@tauri-apps/plugin-http";
import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type { Manifest, ManifestMode, HardwareProfile, Mode } from "./types";
import BUNDLED_MANIFEST_JSON from "../manifests/default.json";

const DEFAULT_TTL_MINUTES = 360;

async function cacheDir(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/cache/manifests`;
}

function cacheKey(url: string): string {
  let h = 5381;
  for (let i = 0; i < url.length; i++) {
    h = ((h * 33) ^ url.charCodeAt(i)) >>> 0;
  }
  return h.toString(16);
}

interface CachedManifest {
  fetched_at: string;
  manifest: Manifest;
}

async function readCache(url: string): Promise<CachedManifest | null> {
  try {
    const dir = await cacheDir();
    const path = `${dir}/${cacheKey(url)}.json`;
    if (!(await exists(path))) return null;
    return JSON.parse(await readTextFile(path));
  } catch {
    return null;
  }
}

async function writeCache(url: string, manifest: Manifest): Promise<void> {
  try {
    const dir = await cacheDir();
    await mkdir(dir, { recursive: true });
    const path = `${dir}/${cacheKey(url)}.json`;
    const entry: CachedManifest = { fetched_at: new Date().toISOString(), manifest };
    await writeTextFile(path, JSON.stringify(entry, null, 2));
  } catch {
    // Cache write failure is non-fatal.
  }
}

function isStale(cachedAt: string, ttlMinutes: number): boolean {
  return (Date.now() - new Date(cachedAt).getTime()) / 60_000 > ttlMinutes;
}

async function fetchManifestRaw(url: string): Promise<Manifest> {
  const response = await fetch(url, { method: "GET", connectTimeout: 10000 });
  if (!response.ok) throw new Error(`HTTP ${response.status} fetching ${url}`);
  return response.json() as Promise<Manifest>;
}

/** Fetch a single manifest URL, honouring its own ttl_minutes. */
async function fetchOne(url: string): Promise<Manifest> {
  if (url.startsWith("bundled://")) return BUNDLED_MANIFEST_JSON as unknown as Manifest;

  const cached = await readCache(url);
  if (cached) {
    const ttl = cached.manifest.ttl_minutes ?? DEFAULT_TTL_MINUTES;
    if (!isStale(cached.fetched_at, ttl)) return cached.manifest;
  }
  try {
    const manifest = await fetchManifestRaw(url);
    await writeCache(url, manifest);
    return manifest;
  } catch {
    if (cached) return cached.manifest;
    return BUNDLED_MANIFEST_JSON as unknown as Manifest;
  }
}

/**
 * Fetch a manifest and recursively merge in any `imports`. Each imported file
 * is fetched + cached against ITS OWN ttl_minutes — imports do not inherit
 * the importing file's TTL. Cycles are detected by URL and broken silently.
 *
 * Merge semantics:
 *   - Imports are merged first (depth-first, document order).
 *   - The importing file's own modes/tiers are merged last and OVERRIDE any
 *     conflicting mode key from imports (closer publisher wins).
 *   - Top-level fields (`name`, `version`, `default_mode`, `ttl_minutes`)
 *     always come from the importing file.
 */
export async function getManifest(url: string): Promise<Manifest> {
  const visited = new Set<string>();
  return walk(url, visited);
}

async function walk(url: string, visited: Set<string>): Promise<Manifest> {
  if (visited.has(url)) {
    return emptyManifest();
  }
  visited.add(url);

  const raw = await fetchOne(url);
  const mergedModes: Record<string, ManifestMode> = {};

  for (const importUrl of raw.imports ?? []) {
    let imported: Manifest;
    try {
      imported = await walk(importUrl, visited);
    } catch {
      continue;
    }
    for (const [key, mode] of Object.entries(imported.modes ?? {})) {
      mergedModes[key] = mode;
    }
  }
  // Importing file wins on mode-key collision.
  for (const [key, mode] of Object.entries(raw.modes ?? {})) {
    mergedModes[key] = mode;
  }

  return {
    name: raw.name,
    version: raw.version,
    ttl_minutes: raw.ttl_minutes,
    default_mode: raw.default_mode,
    modes: mergedModes,
  };
}

function emptyManifest(): Manifest {
  return { name: "", version: "1", default_mode: "text", modes: {} };
}

export function resolveModel(
  hardware: HardwareProfile,
  manifest: Manifest,
  mode: Mode,
  modeOverrides?: Partial<Record<Mode, string | null>>,
): string {
  const override = modeOverrides?.[mode];
  if (override) return override;

  const modeSpec = manifest.modes[mode] ?? manifest.modes[manifest.default_mode];
  if (!modeSpec) return "tinyllama";

  const vram = hardware.vram_gb ?? 0;
  const ram = hardware.ram_gb;

  for (const tier of modeSpec.tiers) {
    if (vram >= tier.min_vram_gb || ram >= (tier.min_ram_gb ?? 0)) {
      return tier.model;
    }
  }
  return modeSpec.tiers.at(-1)!.model;
}

/** All model tags recommended by a manifest across all tiers and modes. */
export function allRecommendedModels(manifest: Manifest): Set<string> {
  const models = new Set<string>();
  for (const modeSpec of Object.values(manifest.modes)) {
    for (const tier of modeSpec.tiers) {
      models.add(tier.model);
      models.add(tier.fallback);
    }
  }
  return models;
}
