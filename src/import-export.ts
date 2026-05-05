import { fetch } from "@tauri-apps/plugin-http";
import { loadConfig, saveConfig } from "./config";
import type { Config, Source, Provider } from "./types";

interface ExportBundle {
  sources?: Source[];
  providers?: Provider[];
}

export async function importFromUrl(url: string): Promise<{ sources: string[]; providers: string[] }> {
  let json: string;
  if (url.startsWith("anyai:import:")) {
    json = base64Decode(url.slice("anyai:import:".length));
  } else {
    const resp = await fetch(url, { method: "GET", connectTimeout: 10000 });
    if (!resp.ok) throw new Error(`HTTP ${resp.status} fetching ${url}`);
    json = await resp.text();
  }
  return importBundle(JSON.parse(json));
}

export async function importBundle(bundle: ExportBundle): Promise<{ sources: string[]; providers: string[] }> {
  const config = await loadConfig();
  const addedSources: string[] = [];
  const addedProviders: string[] = [];

  if (bundle.sources) {
    for (const s of bundle.sources) {
      if (!config.sources.find((e) => e.name === s.name)) {
        config.sources.push(s);
        addedSources.push(s.name);
      }
    }
  }

  if (bundle.providers) {
    for (const p of bundle.providers) {
      if (!config.providers.find((e) => e.name === p.name)) {
        config.providers.push(p);
        addedProviders.push(p.name);
      }
    }
  }

  await saveConfig(config);
  return { sources: addedSources, providers: addedProviders };
}

export async function exportBundle(opts: {
  sourcesOnly?: boolean;
  providersOnly?: boolean;
}): Promise<ExportBundle> {
  const config = await loadConfig();
  const bundle: ExportBundle = {};
  if (!opts.providersOnly) bundle.sources = config.sources;
  if (!opts.sourcesOnly) bundle.providers = config.providers;
  return bundle;
}

export async function exportAsUrl(opts: { sourcesOnly?: boolean; providersOnly?: boolean } = {}): Promise<string> {
  const bundle = await exportBundle(opts);
  return `anyai:import:${base64Encode(JSON.stringify(bundle))}`;
}

function base64Encode(str: string): string {
  return btoa(unescape(encodeURIComponent(str)))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function base64Decode(str: string): string {
  const padded = str.replace(/-/g, "+").replace(/_/g, "/");
  const pad = padded.length % 4 === 0 ? 0 : 4 - (padded.length % 4);
  return decodeURIComponent(escape(atob(padded + "=".repeat(pad))));
}
