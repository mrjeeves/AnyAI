export type GpuType = "nvidia" | "amd" | "apple" | "none";

export interface HardwareProfile {
  vram_gb: number | null;
  ram_gb: number;
  disk_free_gb: number;
  gpu_type: GpuType;
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

export interface ProviderCatalog {
  name: string;
  description?: string;
  ttl_minutes?: number;
  providers: Array<{ name: string; url: string; description?: string }>;
}

export interface ApiConfig {
  enabled: boolean;
  host: string;
  port: number;
  cors_allow_all: boolean;
  bearer_token: string | null;
}

export interface Config {
  active_provider: string;
  active_mode: Mode;
  model_cleanup_days: number;
  kept_models: string[];
  mode_overrides: Partial<Record<Mode, string | null>>;
  tracked_modes: Mode[];
  api: ApiConfig;
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
