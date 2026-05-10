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

export type ModelRuntime = "ollama" | "whisper";

export interface ManifestTier {
  min_vram_gb: number;
  min_ram_gb?: number;
  /** Approximate on-disk size of the model file(s) in MB. Surfaced in the
   *  Settings → Family tier ladder so users can see what each rung costs
   *  before committing. Optional: tiers without it just hide the column. */
  disk_mb?: number;
  model: string;
  fallback: string;
}

export interface ManifestMode {
  label: string;
  input?: "audio";
  /** Which runtime executes models in this mode. Defaults to "ollama"
   *  (the LLM stack); transcribe modes set "whisper" so the resolver
   *  knows the `model` strings name files under `~/.anyai/whisper/`
   *  rather than Ollama tags. */
  runtime?: ModelRuntime;
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

/** Optional in-process server that exposes a minimal browser shell over the
 *  LAN so phones / other machines can chat with this AnyAI instance. Off by
 *  default — turning it on binds 0.0.0.0:port. Single-user: the local Tauri
 *  UI is curtained off while a remote session is active. */
export interface RemoteUiConfig {
  enabled: boolean;
  port: number;
}

/** Microphone capture settings used by transcribe mode. Audio capture
 *  runs through cpal on the Rust side; `device_name` is matched against
 *  `cpal::Device::name()`. Empty string = system default. The whisper
 *  model itself is picked by the active family's tier resolver — set
 *  `mode_overrides.transcribe` to override (e.g. "small.en"). */
export interface MicConfig {
  device_name: string;
  /** Target capture rate in Hz. 16000 is what whisper wants; the cpal
   *  capture path resamples to 16k regardless, so this is just a hint
   *  to any future browser-side fallback. */
  sample_rate: number;
  /** WebRTC echo cancellation — only applies if a future build uses the
   *  WebView mic path; cpal doesn't expose an equivalent. */
  echo_cancellation: boolean;
  /** WebRTC noise suppression — same caveat as above. */
  noise_suppression: boolean;
  /** WebRTC auto gain control — same caveat as above. */
  auto_gain_control: boolean;
}

export interface Config {
  active_provider: string;
  active_family: string;
  active_mode: Mode;
  model_cleanup_days: number;
  kept_models: string[];
  mode_overrides: Partial<Record<Mode, string | null>>;
  tracked_modes: Mode[];
  /** Where AnyAI persists conversations and generated artifacts. Defaults to
   *  `~/.anyai/conversations/`. Stored as an absolute path so exported
   *  configs are readable, though new machines re-default on first load. */
  conversation_dir: string;
  api: ApiConfig;
  auto_update: AutoUpdateConfig;
  remote_ui: RemoteUiConfig;
  mic: MicConfig;
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
