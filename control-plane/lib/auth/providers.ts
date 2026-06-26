interface ProviderEnv {
  providerId: string;
  displayName: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes?: string[];
}

export interface GenericOAuthConfig {
  providerId: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes: string[];
}

export interface OidcProviderLabel {
  providerId: string;
  displayName: string;
}

/**
 * Parse OIDC_PROVIDERS and return ONLY the safe-to-expose fields
 * (providerId + displayName). No secrets, clientIds, or discovery URLs.
 * Safe to call from a server component and pass to the client.
 */
export function oidcProviderLabels(): OidcProviderLabel[] {
  const raw = process.env.OIDC_PROVIDERS;
  if (!raw) return [];
  let parsed: ProviderEnv[];
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  return parsed.map((p) => ({ providerId: p.providerId, displayName: p.displayName }));
}

/** Parse OIDC_PROVIDERS (JSON array) into better-auth genericOAuth configs. */
export function oidcProviders(): GenericOAuthConfig[] {
  const raw = process.env.OIDC_PROVIDERS;
  if (!raw) return [];
  let parsed: ProviderEnv[];
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error("OIDC_PROVIDERS is not valid JSON (expected an array of provider configs).");
  }
  return parsed.map((p) => ({
    providerId: p.providerId,
    discoveryUrl: p.discoveryUrl,
    clientId: p.clientId,
    clientSecret: p.clientSecret,
    scopes: p.scopes ?? ["openid", "email", "profile"],
  }));
}
