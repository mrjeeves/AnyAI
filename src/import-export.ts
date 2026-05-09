import { fetch } from "@tauri-apps/plugin-http";
import { loadConfig, saveConfig } from "./config";
import type { Provider } from "./types";

interface ExportBundle {
  providers?: Provider[];
}

export async function importFromUrl(url: string): Promise<{ providers: string[] }> {
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

export async function importBundle(bundle: ExportBundle): Promise<{ providers: string[] }> {
  const config = await loadConfig();
  const addedProviders: string[] = [];

  if (bundle.providers) {
    for (const p of bundle.providers) {
      if (!config.providers.find((e) => e.name === p.name)) {
        config.providers.push(p);
        addedProviders.push(p.name);
      }
    }
  }

  await saveConfig(config);
  return { providers: addedProviders };
}

export async function exportBundle(): Promise<ExportBundle> {
  const config = await loadConfig();
  return { providers: config.providers };
}

export async function exportAsUrl(): Promise<string> {
  const bundle = await exportBundle();
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
