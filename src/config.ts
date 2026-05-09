import { readTextFile, writeTextFile, exists, mkdir } from "@tauri-apps/plugin-fs";
import { homeDir } from "@tauri-apps/api/path";
import type { Config, ApiConfig, AutoUpdateConfig } from "./types";

async function configPath(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/config.json`;
}

/** Default location for persisted chats / artifacts. Lives under the same
 *  `~/.anyai/` tree as the rest of AnyAI's state so a single directory holds
 *  everything the user might want to back up or wipe. */
async function defaultConversationDir(): Promise<string> {
  const home = await homeDir();
  return `${home}/.anyai/conversations`;
}

const DEFAULT_API: ApiConfig = {
  enabled: true,
  host: "127.0.0.1",
  port: 1473,
  cors_allow_all: false,
  bearer_token: null,
};

const DEFAULT_AUTO_UPDATE: AutoUpdateConfig = {
  enabled: true,
  channel: "stable",
  auto_apply: "patch",
  check_interval_hours: 6,
};

const DEFAULT_CONFIG: Config = {
  active_provider: "AnyAI Default",
  active_family: "gemma4",
  active_mode: "text",
  model_cleanup_days: 1,
  kept_models: [],
  mode_overrides: {},
  tracked_modes: ["text"],
  // Filled at first load via defaultConversationDir() — needs an async homeDir().
  conversation_dir: "",
  api: { ...DEFAULT_API },
  auto_update: { ...DEFAULT_AUTO_UPDATE },
  providers: [
    {
      name: "AnyAI Default",
      url: "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json",
    },
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
      if (!_cached.conversation_dir) {
        _cached.conversation_dir = await defaultConversationDir();
      }
      // Persist any defaults we filled in so subsequent loads are consistent.
      await saveConfig(_cached);
      return _cached;
    }
  } catch {
    // Corrupt config — reset.
  }
  _cached = structuredClone(DEFAULT_CONFIG);
  _cached.conversation_dir = await defaultConversationDir();
  await saveConfig(_cached);
  return _cached;
}

/** Pre-1.0 builds shipped this URL for the AnyAI Default provider. The host
 *  no longer serves the manifest; rewrite it on load so users with an older
 *  config don't see a dead URL in the Providers tab. Cheap; runs once per
 *  load and persists via saveConfig. */
const LEGACY_ANYAI_RUN_HOST = "anyai.run";
const CANONICAL_DEFAULT_URL =
  "https://raw.githubusercontent.com/mrjeeves/AnyAI/main/manifests/default.json";

function rewriteLegacyProviderUrls(providers: Config["providers"]): Config["providers"] {
  return providers.map((p) => {
    try {
      const host = new URL(p.url).hostname;
      if (host === LEGACY_ANYAI_RUN_HOST) {
        return { ...p, url: CANONICAL_DEFAULT_URL };
      }
    } catch {
      // Malformed URL — leave it alone; the user can edit/remove via the UI.
    }
    return p;
  });
}

function mergeDefaults(raw: Record<string, unknown>): Config {
  const merged: Config = {
    ...DEFAULT_CONFIG,
    ...(raw as Partial<Config>),
    api: { ...DEFAULT_API, ...((raw as { api?: Partial<ApiConfig> }).api ?? {}) },
    auto_update: { ...DEFAULT_AUTO_UPDATE, ...((raw as { auto_update?: Partial<AutoUpdateConfig> }).auto_update ?? {}) },
    mode_overrides: (raw as { mode_overrides?: Config["mode_overrides"] }).mode_overrides ?? {},
    kept_models: (raw as { kept_models?: string[] }).kept_models ?? [],
    tracked_modes: (raw as { tracked_modes?: Config["tracked_modes"] }).tracked_modes ?? [],
    providers: rewriteLegacyProviderUrls(
      (raw as { providers?: Config["providers"] }).providers ?? DEFAULT_CONFIG.providers,
    ),
  };
  // Strip removed legacy fields so they don't linger in the saved config.
  delete (merged as unknown as { sources?: unknown }).sources;
  // One-shot upgrade: seed tracked_modes from active_mode for legacy configs.
  if (!merged.tracked_modes || merged.tracked_modes.length === 0) {
    merged.tracked_modes = [merged.active_mode];
  }
  // Older configs predate active_family; default to the schema's gemma4.
  if (!merged.active_family) {
    merged.active_family = DEFAULT_CONFIG.active_family;
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
