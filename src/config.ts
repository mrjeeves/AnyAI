import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type { Config } from "./types";

async function configPath(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/config.json`;
}

const DEFAULT_CONFIG: Config = {
  active_provider: "AnyAI Default",
  active_mode: "text",
  model_cleanup_days: 1,
  kept_models: [],
  mode_overrides: {},
  sources: [{ name: "AnyAI", url: "https://anyai.run/sources/index.json" }],
  providers: [{ name: "AnyAI Default", url: "https://anyai.run/manifest/default.json", source: "AnyAI" }],
};

let _cached: Config | null = null;

export async function loadConfig(): Promise<Config> {
  if (_cached) return _cached;
  const path = await configPath();
  try {
    if (await exists(path)) {
      _cached = JSON.parse(await readTextFile(path));
      return _cached!;
    }
  } catch {
    // Corrupt config — reset.
  }
  _cached = structuredClone(DEFAULT_CONFIG);
  await saveConfig(_cached);
  return _cached;
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
