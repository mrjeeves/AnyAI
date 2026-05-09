export type GpuType = "nvidia" | "amd" | "apple" | "none";

export interface HardwareProfile {
  vram_gb: number | null;
  ram_gb: number;
  disk_free_gb: number;
  gpu_type: GpuType;
  /** CPU architecture the binary was built for, e.g. "x86_64", "aarch64". */
  arch?: string;
  /** Friendly board / SoC label when known, e.g. "Raspberry Pi 5 Model B". */
  soc?: string | null;
}

export type Mode = "text" | "vision" | "code" | "transcribe";

export interface ManifestTier {
  min_vram_gb: number;
  min_ram_gb?: number;
  model: string;
  fallback: string;
}

export interface ManifestMode {
  label: string;
  input?: "audio";
  tiers: ManifestTier[];
}

/**
 * A model family — e.g. "gemma4", "qwen3". Owns its own per-mode tier table:
 * a family is the unit of "what models do I run, sized to my hardware". Users
 * pick an active family inside an active provider; the resolver walks
 * `families[active_family].modes[mode].tiers` against the local hardware.
 */
export interface ManifestFamily {
  /** Human-readable name shown in the UI ("Gemma 4"). */
  label: string;
  /** One-line blurb shown in the family picker. Optional. */
  description?: string;
  /** Mode picked when the user hasn't chosen one. */
  default_mode: Mode;
  modes: Record<string, ManifestMode>;
}

export interface Manifest {
  name: string;
  version: string;
  ttl_minutes?: number;
  /** Family picked when the user hasn't chosen one. */
  default_family: string;
  /** URLs of other manifests whose families are merged into this one. */
  imports?: string[];
  families: Record<string, ManifestFamily>;
}

export interface Provider {
  name: string;
  url: string;
}

export interface ApiConfig {
  enabled: boolean;
  host: string;
  port: number;
  cors_allow_all: boolean;
  bearer_token: string | null;
}

export type AutoUpdateChannel = "stable" | "beta";
export type AutoApplyPolicy = "patch" | "minor" | "all" | "none";

export interface AutoUpdateConfig {
  enabled: boolean;
  channel: AutoUpdateChannel;
  auto_apply: AutoApplyPolicy;
  check_interval_hours: number;
}

export interface Config {
  active_provider: string;
  active_family: string;
  active_mode: Mode;
  model_cleanup_days: number;
  kept_models: string[];
  mode_overrides: Partial<Record<Mode, string | null>>;
  tracked_modes: Mode[];
  api: ApiConfig;
  auto_update: AutoUpdateConfig;
  providers: Provider[];
}

export interface ModelStatus {
  recommended_by: string[];
  last_recommended: string;
}

export interface ModelStatusCache {
  [modelTag: string]: ModelStatus;
}

export interface OllamaModel {
  name: string;
  size: number;
}
