import { fetch } from "@tauri-apps/plugin-http";
import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type {
  Manifest,
  ManifestFamily,
  ManifestMode,
  ManifestTier,
  ModelRuntime,
  HardwareProfile,
  Mode,
} from "./types";
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

/** Compare manifest schema versions. Newer-bundled means the binary
 *  understands a manifest format the cached file might predate, so we
 *  refuse to use the cache and re-fetch (or fall back to the bundled
 *  copy). Versions are simple integers stringified — `parseInt` does
 *  the right thing across "6" / "7" / "8" / "9" / "10". */
function bundledVersionIsNewer(cached: Manifest | null | undefined): boolean {
  if (!cached) return false;
  const bundledV = parseInt((BUNDLED_MANIFEST_JSON as Manifest).version ?? "", 10);
  const cachedV = parseInt(cached.version ?? "", 10);
  if (Number.isNaN(bundledV) || Number.isNaN(cachedV)) return false;
  return bundledV > cachedV;
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
    // Cache is OK if it's still fresh AND the bundled binary doesn't
    // already know about a newer schema. The version-bump escape hatch
    // keeps `just dev` rebuilds from staring at a stale cached manifest
    // for up to TTL hours after bumping the manifest version.
    if (!isStale(cached.fetched_at, ttl) && !bundledVersionIsNewer(cached.manifest)) {
      return cached.manifest;
    }
  }
  try {
    const manifest = await fetchManifestRaw(url);
    await writeCache(url, manifest);
    return manifest;
  } catch {
    // Network failed — prefer the cache, but if our bundled is newer
    // than the cache, the bundled manifest is the more accurate source.
    if (cached && !bundledVersionIsNewer(cached.manifest)) return cached.manifest;
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
  const mergedSharedModes: Record<string, ManifestMode> = {};

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
    for (const [key, m] of Object.entries(imported.shared_modes ?? {})) {
      mergedSharedModes[key] = m;
    }
  }
  // Importing file wins on key collision (closer publisher overrides).
  for (const [key, family] of Object.entries(raw.families ?? {})) {
    mergedFamilies[key] = family;
  }
  for (const [key, m] of Object.entries(raw.shared_modes ?? {})) {
    mergedSharedModes[key] = m;
  }

  return {
    name: raw.name,
    version: raw.version,
    ttl_minutes: raw.ttl_minutes,
    default_family: raw.default_family,
    shared_modes: mergedSharedModes,
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

/** Detailed resolution result. `model` is the bare tag/name (e.g.
 *  `gemma4:e2b` or `tiny.en`); `runtime` tells the caller which engine
 *  to use. The picked tier comes through too so callers can show
 *  hardware cost in the UI. */
export interface ResolvedModel {
  model: string;
  runtime: ModelRuntime;
  /** The matched tier, or null if the resolver fell back to the bottom
   *  of the ladder without any min_*_gb threshold being satisfied. */
  tier: ManifestTier | null;
  /** Whether the model came from `mode_overrides` rather than the
   *  hardware-walked tier ladder. */
  override: boolean;
}

/** Default runtime for a mode when the manifest doesn't declare one.
 *  Transcribe always uses whisper-rs in this app; everything else routes
 *  through Ollama. Centralised so the frontend, Rust resolver, and Rust
 *  preload loop can stay in sync. Crucial when a user's cached manifest
 *  predates the `runtime` field — without this fallback the resolver
 *  would inherit the fallback mode's runtime (usually `ollama`) and hand
 *  whisper-shaped names to `ollama pull`. */
export function defaultRuntimeFor(mode: Mode): ModelRuntime {
  return mode === "transcribe" ? "whisper" : "ollama";
}

/** Look up a mode block, preferring the family's own declaration but
 *  falling back to `manifest.shared_modes`. The shared-modes pattern
 *  lets the manifest publish a canonical `transcribe` block once and
 *  every family inherit it without redeclaring six tiers each. Family-
 *  level overrides win — a family can publish its own `transcribe` to
 *  customise the whisper picks. */
export function modeFor(
  manifest: Manifest,
  family: ManifestFamily,
  mode: Mode,
): ManifestMode | undefined {
  return family.modes[mode] ?? manifest.shared_modes?.[mode];
}

export function resolveModelEx(
  hardware: HardwareProfile,
  manifest: Manifest,
  mode: Mode,
  modeOverrides?: Partial<Record<Mode, string | null>>,
  familyName?: string,
): ResolvedModel {
  const picked = pickFamily(manifest, familyName);
  const family = picked?.family;
  // Look up the mode in the family first, then fall back to
  // manifest.shared_modes (the canonical whisper transcribe ladder
  // lives there). Never inherit from the family's `default_mode` —
  // that mode runs on a different runtime and its tier ladder is
  // incompatible.
  const exactSpec = family ? modeFor(manifest, family, mode) : undefined;
  const runtime: ModelRuntime =
    exactSpec?.runtime ?? defaultRuntimeFor(mode);

  const override = modeOverrides?.[mode];
  if (override) {
    return { model: override, runtime, tier: null, override: true };
  }

  // No exact OR shared block AND we're on a non-Ollama runtime — fall
  // back to a safe well-known whisper model rather than crossing tier
  // ladders with text mode (which would surface nonsense like
  // `whisper:gemma4:e2b` and trip whisper-rs).
  if (!exactSpec && runtime === "whisper") {
    return { model: "tiny.en", runtime, tier: null, override: false };
  }

  const tierSpec = exactSpec
    ?? (family ? family.modes[family.default_mode] : null);

  if (!tierSpec) {
    return { model: "tinyllama", runtime, tier: null, override: false };
  }

  const vram = effectiveVramGb(hardware);
  const ram = hardware.ram_gb;

  for (const tier of tierSpec.tiers) {
    if (vram >= tier.min_vram_gb || ram >= (tier.min_ram_gb ?? 0)) {
      return { model: tier.model, runtime, tier, override: false };
    }
  }
  const last = tierSpec.tiers.at(-1) ?? null;
  return {
    model: last?.model ?? "tinyllama",
    runtime,
    tier: last,
    override: false,
  };
}

/** Backwards-compatible string-only resolution used by call sites that
 *  don't need the runtime/tier breakdown. New code should prefer
 *  `resolveModelEx`. */
export function resolveModel(
  hardware: HardwareProfile,
  manifest: Manifest,
  mode: Mode,
  modeOverrides?: Partial<Record<Mode, string | null>>,
  familyName?: string,
): string {
  return resolveModelEx(hardware, manifest, mode, modeOverrides, familyName).model;
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

/** Modes a family advertises. Reads both the family's own declared
 *  `modes` and the manifest's `shared_modes` so a family that just
 *  inherits the canonical transcribe block still surfaces it on the
 *  mode bar. Transcribe is always advertised because whisper-rs ships
 *  with the binary — exposing it even when a cached manifest predates
 *  the `transcribe` block keeps the mode bar usable across upgrades. */
export function familyModes(manifest: Manifest, family: ManifestFamily): Set<Mode> {
  const out = new Set<Mode>();
  for (const m of ["text", "vision", "code", "transcribe"] as Mode[]) {
    if (modeFor(manifest, family, m)) out.add(m);
  }
  out.add("transcribe");
  return out;
}

/** True if a mode resolves to whisper-rs (rather than Ollama) under the
 *  active family. Reads the family's spec first, then `shared_modes`,
 *  then falls back to `defaultRuntimeFor(mode)` so a stale
 *  pre-`runtime`-field cached manifest still flags transcribe as
 *  whisper. */
export function isWhisperMode(
  manifest: Manifest,
  family: ManifestFamily,
  mode: Mode,
): boolean {
  const declared = modeFor(manifest, family, mode)?.runtime;
  return (declared ?? defaultRuntimeFor(mode)) === "whisper";
}
