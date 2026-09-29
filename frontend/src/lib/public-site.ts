export type PublicApiBaseUrlResolution =
  | { baseUrl: string; error: null }
  | { baseUrl: null; error: "public_api_base_url_required" };

// Public surfaces drop the "Console" product descriptor from the configured site name so the
// marketing copy reads as a brand rather than a dashboard title (PS-L4). An empty result
// falls back to the configured value.
export function resolvePublicBrandName(siteName: string): string {
  const trimmed = siteName.trim();
  const brand = trimmed.replace(/\s+console$/i, "").trim();
  return brand || trimmed;
}

export function resolvePublicApiBaseUrl(
  configuredBaseUrl: string,
  browserOrigin: string,
): PublicApiBaseUrlResolution {
  const configured = configuredBaseUrl.trim();
  if (configured) {
    return { baseUrl: configured.replace(/\/+$/, ""), error: null };
  }

  const origin = new URL(browserOrigin);
  if (origin.protocol !== "https:") {
    return { baseUrl: null, error: "public_api_base_url_required" };
  }
  return { baseUrl: `${origin.origin}/v1`, error: null };
}
