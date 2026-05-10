import { fetch } from "@tauri-apps/plugin-http";
import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type { Manifest, ManifestFamily, HardwareProfile, Mode } from "./types";
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
 *   - The importing file's own families are merged last and OVERRIDE any
 *     conflicting family key from imports (closer publisher wins).
 *   - Top-level fields (`name`, `version`, `default_family`, `ttl_minutes`)
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
  const mergedFamilies: Record<string, ManifestFamily> = {};

  for (const importUrl of raw.imports ?? []) {
    let imported: Manifest;
    try {
      imported = await walk(importUrl, visited);
    } catch {
      continue;
    }
    for (const [key, family] of Object.entries(imported.families ?? {})) {
      mergedFamilies[key] = family;
    }
  }
  // Importing file wins on family-key collision.
  for (const [key, family] of Object.entries(raw.families ?? {})) {
    mergedFamilies[key] = family;
  }

  return {
    name: raw.name,
    version: raw.version,
    ttl_minutes: raw.ttl_minutes,
    default_family: raw.default_family,
    families: mergedFamilies,
  };
}

function emptyManifest(): Manifest {
  return { name: "", version: "1", default_family: "", families: {} };
}

/**
 * Pick a family from a manifest. Falls back to `default_family`, then to the
 * first family in document order — so the resolver never returns null even
 * when callers pass a stale/unknown family name.
 */
export function pickFamily(manifest: Manifest, requested?: string): { name: string; family: ManifestFamily } | null {
  const keys = Object.keys(manifest.families);
  if (keys.length === 0) return null;
  const candidates = [requested, manifest.default_family, keys[0]].filter(
    (k): k is string => typeof k === "string" && k.length > 0,
  );
  for (const k of candidates) {
    const family = manifest.families[k];
    if (family) return { name: k, family };
  }
  return { name: keys[0], family: manifest.families[keys[0]] };
}

export function resolveModel(
  hardware: HardwareProfile,
  manifest: Manifest,
  mode: Mode,
  modeOverrides?: Partial<Record<Mode, string | null>>,
  familyName?: string,
): string {
  const override = modeOverrides?.[mode];
  if (override) return override;

  const picked = pickFamily(manifest, familyName);
  if (!picked) return "tinyllama";

  const { family } = picked;
  const modeSpec = family.modes[mode] ?? family.modes[family.default_mode];
  if (!modeSpec) return "tinyllama";

  const vram = effectiveVramGb(hardware);
  const ram = hardware.ram_gb;

  for (const tier of modeSpec.tiers) {
    if (vram >= tier.min_vram_gb || ram >= (tier.min_ram_gb ?? 0)) {
      return tier.model;
    }
  }
  return modeSpec.tiers.at(-1)!.model;
}

/**
 * VRAM the resolver should credit toward `min_vram_gb` checks.
 *
 * Discrete GPUs (NVIDIA, AMD) own their VRAM separately from system RAM, so
 * a 12 GB card lets the model live entirely off-CPU and the tier check is
 * meaningful. On Apple Silicon and integrated GPUs, "VRAM" is just a slice
 * of the same physical pool `ram_gb` already counts — crediting it again
 * means an 8 GB Mac matches a tier wanting `vram>=6`, picks a 9 B model,
 * and grinds at ~1 token / 10 s while the OS swaps. Treat non-discrete
 * vram as 0 so those systems are tiered purely on `ram_gb`.
 */
function effectiveVramGb(hw: HardwareProfile): number {
  if (hw.gpu_type === "nvidia" || hw.gpu_type === "amd") return hw.vram_gb ?? 0;
  return 0;
}

/** All model tags recommended by a manifest across every family/mode/tier. */
export function allRecommendedModels(manifest: Manifest): Set<string> {
  const models = new Set<string>();
  for (const family of Object.values(manifest.families ?? {})) {
    for (const modeSpec of Object.values(family.modes ?? {})) {
      for (const tier of modeSpec.tiers) {
        models.add(tier.model);
        models.add(tier.fallback);
      }
    }
  }
  return models;
}

/** Modes a specific family in a manifest defines tiers for. Transcribe
 *  rides on a separate runtime (whisper-rs, models under
 *  `~/.anyai/whisper/`) so it's always available regardless of whether
 *  the family has Ollama-shaped tiers for it — manifest tiers for
 *  transcribe would falsely advertise Ollama tags that don't exist. */
export function familyModes(family: ManifestFamily): Set<Mode> {
  const out = new Set<Mode>();
  for (const m of ["text", "vision", "code"] as Mode[]) {
    if (family.modes[m]) out.add(m);
  }
  // Transcribe is built-in: whisper-rs ships with the binary and the
  // model lives under ~/.anyai/whisper/. Surface it regardless of the
  // family's Ollama tier table.
  out.add("transcribe");
  return out;
}
