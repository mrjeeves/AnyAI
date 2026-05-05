import { loadConfig, saveConfig } from "./config";
import { getManifest } from "./manifest";
import type { Provider, Manifest } from "./types";

export async function getProviders(): Promise<Provider[]> {
  const config = await loadConfig();
  return config.providers;
}

export async function getActiveProvider(): Promise<Provider | null> {
  const config = await loadConfig();
  return config.providers.find((p) => p.name === config.active_provider) ?? null;
}

export async function getActiveManifest(): Promise<Manifest> {
  const provider = await getActiveProvider();
  if (!provider) return getManifest("bundled://default");
  return getManifest(provider.url);
}

export async function addProvider(provider: Provider): Promise<void> {
  const config = await loadConfig();
  const existing = config.providers.findIndex((p) => p.name === provider.name);
  if (existing >= 0) {
    config.providers[existing].url = provider.url;
  } else {
    config.providers.push(provider);
  }
  await saveConfig(config);
}

export async function removeProvider(name: string): Promise<void> {
  const config = await loadConfig();
  if (config.active_provider === name) {
    throw new Error(`Cannot remove active provider '${name}'; switch first.`);
  }
  config.providers = config.providers.filter((p) => p.name !== name);
  await saveConfig(config);
}

export async function setActiveProvider(name: string): Promise<void> {
  const config = await loadConfig();
  if (!config.providers.find((p) => p.name === name)) {
    throw new Error(`Provider '${name}' not found`);
  }
  config.active_provider = name;
  await saveConfig(config);
}

/** Return all manifests from all saved providers (for recommendation-set computation). */
export async function getAllManifests(): Promise<Array<{ provider: Provider; manifest: Manifest }>> {
  const providers = await getProviders();
  const results = await Promise.allSettled(
    providers.map(async (p) => ({ provider: p, manifest: await getManifest(p.url) }))
  );
  return results
    .filter((r): r is PromiseFulfilledResult<{ provider: Provider; manifest: Manifest }> => r.status === "fulfilled")
    .map((r) => r.value);
}
