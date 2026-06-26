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
