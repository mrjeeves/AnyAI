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

export interface Manifest {
  name: string;
  version: string;
  ttl_minutes?: number;
  default_mode: Mode;
  /** URLs of other manifests whose modes/tiers are merged into this one. */
  imports?: string[];
  modes: Record<string, ManifestMode>;
}

export interface Source {
  name: string;
  url: string;
}

export interface Provider {
  name: string;
  url: string;
  source: string | null;
}

export interface CatalogEntry {
  name: string;
  url: string;
  description?: string;
  /** URL of the file this entry was sourced from. Set by the resolver during merge. */
  origin?: string;
}

export interface ProviderCatalog {
  name: string;
  description?: string;
  ttl_minutes?: number;
  /** URLs of other catalogs whose providers are merged into this one. */
  imports?: string[];
  providers: CatalogEntry[];
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
  active_mode: Mode;
  model_cleanup_days: number;
  kept_models: string[];
  mode_overrides: Partial<Record<Mode, string | null>>;
  tracked_modes: Mode[];
  api: ApiConfig;
  auto_update: AutoUpdateConfig;
  sources: Source[];
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
