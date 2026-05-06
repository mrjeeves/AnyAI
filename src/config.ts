import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type { Config, ApiConfig } from "./types";

async function configPath(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/config.json`;
}

const DEFAULT_API: ApiConfig = {
  enabled: true,
  host: "127.0.0.1",
  port: 1473,
  cors_allow_all: false,
  bearer_token: null,
};

const DEFAULT_CONFIG: Config = {
  active_provider: "AnyAI Default",
  active_mode: "text",
  model_cleanup_days: 1,
  kept_models: [],
  mode_overrides: {},
  tracked_modes: ["text"],
  api: { ...DEFAULT_API },
  sources: [{ name: "AnyAI", url: "https://anyai.run/sources/index.json" }],
  providers: [
    { name: "AnyAI Default", url: "https://anyai.run/manifest/default.json", source: "AnyAI" },
  ],
};

let _cached: Config | null = null;

export async function loadConfig(): Promise<Config> {
  if (_cached) return _cached;
  const path = await configPath();
  try {
    if (await exists(path)) {
      const raw = JSON.parse(await readTextFile(path));
      _cached = mergeDefaults(raw);
      // Persist any defaults we filled in so subsequent loads are consistent.
      await saveConfig(_cached);
      return _cached;
    }
  } catch {
    // Corrupt config — reset.
  }
  _cached = structuredClone(DEFAULT_CONFIG);
  await saveConfig(_cached);
  return _cached;
}

function mergeDefaults(raw: Partial<Config>): Config {
  const merged: Config = {
    ...DEFAULT_CONFIG,
    ...raw,
    api: { ...DEFAULT_API, ...(raw.api ?? {}) },
    mode_overrides: raw.mode_overrides ?? {},
    kept_models: raw.kept_models ?? [],
    tracked_modes: raw.tracked_modes ?? [],
    sources: raw.sources ?? DEFAULT_CONFIG.sources,
    providers: raw.providers ?? DEFAULT_CONFIG.providers,
  };
  // One-shot upgrade: seed tracked_modes from active_mode for legacy configs.
  if (!merged.tracked_modes || merged.tracked_modes.length === 0) {
    merged.tracked_modes = [merged.active_mode];
  }
  return merged;
}

export async function saveConfig(config: Config): Promise<void> {
  _cached = config;
  const path = await configPath();
  const dir = path.substring(0, path.lastIndexOf("/"));
  await mkdir(dir, { recursive: true });
  await writeTextFile(path, JSON.stringify(config, null, 2));
}

export async function updateConfig(patch: Partial<Config>): Promise<Config> {
  const config = await loadConfig();
  const updated = { ...config, ...patch };
  await saveConfig(updated);
  return updated;
}

export function invalidateConfigCache(): void {
  _cached = null;
}
