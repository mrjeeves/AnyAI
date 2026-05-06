import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import { invoke } from "@tauri-apps/api/core";
import { loadConfig, saveConfig } from "./config";
import { getAllManifests } from "./providers";
import { allRecommendedModels } from "./manifest";
import type { ModelStatusCache, OllamaModel, Mode } from "./types";

async function statusCachePath(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/cache/model-status.json`;
}

async function readStatusCache(): Promise<ModelStatusCache> {
  try {
    const path = await statusCachePath();
    if (await exists(path)) return JSON.parse(await readTextFile(path));
  } catch {}
  return {};
}

async function writeStatusCache(cache: ModelStatusCache): Promise<void> {
  const path = await statusCachePath();
  const dir = path.substring(0, path.lastIndexOf("/"));
  await mkdir(dir, { recursive: true });
  await writeTextFile(path, JSON.stringify(cache, null, 2));
}

/**
 * Recompute which pulled models are recommended by any active provider.
 * Updates model-status.json. Called on startup, source/provider change.
 */
export async function recomputeRecommendedSet(): Promise<ModelStatusCache> {
  const [pulled, allManifests] = await Promise.all([
    invoke<OllamaModel[]>("ollama_list_models"),
    getAllManifests(),
  ]);

  const now = new Date().toISOString();
  const existing = await readStatusCache();

  // Build map: model tag → list of provider names that recommend it
  const recommendedBy = new Map<string, string[]>();
  for (const { provider, manifest } of allManifests) {
    for (const tag of allRecommendedModels(manifest)) {
      const list = recommendedBy.get(tag) ?? [];
      list.push(provider.name);
      recommendedBy.set(tag, list);
    }
  }

  const updated: ModelStatusCache = {};
  for (const model of pulled) {
    const providers = recommendedBy.get(model.name) ?? [];
    const wasRecommended = (existing[model.name]?.recommended_by ?? []).length > 0;
    const isNow = providers.length > 0;
    updated[model.name] = {
      recommended_by: providers,
      // Preserve last_recommended if still recommended; set to now if newly recommended;
      // keep old timestamp if became unrecommended (clock starts from when it last was recommended).
      last_recommended: isNow
        ? now
        : wasRecommended
        ? now
        : (existing[model.name]?.last_recommended ?? now),
    };
  }

  await writeStatusCache(updated);
  return updated;
}

/** Evict models that are no longer recommended by any provider beyond the cleanup threshold. */
export async function runCleanup(): Promise<string[]> {
  const config = await loadConfig();
  const thresholdMs = config.model_cleanup_days * 24 * 60 * 60 * 1000;
  const keepSet = new Set(config.kept_models);
  const overrideSet = new Set(
    Object.values(config.mode_overrides).filter((v): v is string => typeof v === "string")
  );

  const status = await recomputeRecommendedSet();
  const evicted: string[] = [];

  for (const [tag, info] of Object.entries(status)) {
    if (info.recommended_by.length > 0) continue; // still recommended
    if (keepSet.has(tag)) continue;              // user-pinned
    if (overrideSet.has(tag)) continue;          // user override → implicitly kept

    const age = Date.now() - new Date(info.last_recommended).getTime();
    if (age >= thresholdMs) {
      try {
        await invoke("ollama_delete_model", { name: tag });
        evicted.push(tag);
      } catch {
        // Model may already be gone; ignore.
      }
    }
  }
  return evicted;
}

/** Immediately evict all unrecommended, non-kept, non-override models (respects keep/override). */
export async function pruneNow(): Promise<string[]> {
  const config = await loadConfig();
  const keepSet = new Set(config.kept_models);
  const overrideSet = new Set(
    Object.values(config.mode_overrides).filter((v): v is string => typeof v === "string")
  );

  const status = await recomputeRecommendedSet();
  const evicted: string[] = [];

  for (const [tag, info] of Object.entries(status)) {
    if (info.recommended_by.length > 0) continue;
    if (keepSet.has(tag)) continue;
    if (overrideSet.has(tag)) continue;
    try {
      await invoke("ollama_delete_model", { name: tag });
      evicted.push(tag);
    } catch {}
  }
  return evicted;
}

export async function keepModel(tag: string): Promise<void> {
  const config = await loadConfig();
  if (!config.kept_models.includes(tag)) {
    config.kept_models.push(tag);
    await saveConfig(config);
  }
}

export async function unkeepModel(tag: string): Promise<void> {
  const config = await loadConfig();
  config.kept_models = config.kept_models.filter((m) => m !== tag);
  await saveConfig(config);
}

export async function setModeOverride(mode: Mode, modelTag: string | null): Promise<void> {
  const config = await loadConfig();
  config.mode_overrides[mode] = modelTag;
  await saveConfig(config);
}

/** Force a model into "evict on next runCleanup" by backdating its last_recommended. */
export async function markEvictedNow(tag: string): Promise<void> {
  const cache = await readStatusCache();
  cache[tag] = {
    recommended_by: [],
    last_recommended: new Date(0).toISOString(),
  };
  await writeStatusCache(cache);
}

export async function getModelStatusWithMeta(): Promise<
  Array<{
    name: string;
    size: number;
    recommended_by: string[];
    last_recommended: string;
    kept: boolean;
    override_for: Mode[];
  }>
> {
  const [pulled, status, config] = await Promise.all([
    invoke<OllamaModel[]>("ollama_list_models"),
    readStatusCache(),
    loadConfig(),
  ]);

  const keepSet = new Set(config.kept_models);
  const overrideMap = new Map<string, Mode[]>();
  for (const [mode, tag] of Object.entries(config.mode_overrides)) {
    if (typeof tag === "string") {
      const list = overrideMap.get(tag) ?? [];
      list.push(mode as Mode);
      overrideMap.set(tag, list);
    }
  }

  return pulled.map((m) => ({
    name: m.name,
    size: m.size,
    recommended_by: status[m.name]?.recommended_by ?? [],
    last_recommended: status[m.name]?.last_recommended ?? new Date().toISOString(),
    kept: keepSet.has(m.name),
    override_for: overrideMap.get(m.name) ?? [],
  }));
}
