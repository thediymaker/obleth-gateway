/** How the client authenticates to the IdP's token endpoint. */
export type TokenAuthMethod = "basic" | "post";

const TOKEN_AUTH_METHODS: readonly TokenAuthMethod[] = ["basic", "post"];

interface ProviderEnv {
  providerId: string;
  displayName: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes?: string[];
  /**
   * Token-endpoint client authentication method. Omit for better-auth's
   * default, which sends `client_id`/`client_secret` in the request body
   * (`client_secret_post`).
   *
   * Set to "basic" for an IdP that only accepts HTTP Basic
   * (`client_secret_basic`).
   *
   * A discovery document advertising only `client_secret_basic` is NOT
   * sufficient reason to set this: many such providers accept body credentials
   * anyway. Globus is a good example — it advertises
   * `"token_endpoint_auth_methods_supported": ["client_secret_basic"]`, yet a
   * token request with body credentials authenticates fine. (Probing with
   * invalid credentials is misleading here: Globus answers "Basic auth failed"
   * to anything unauthenticated, including requests that sent no Authorization
   * header at all. Only a probe with real credentials distinguishes a rejected
   * auth method, 401, from an authenticated client refused for another reason,
   * e.g. 403.)
   *
   * Symptom when this is wrong: the IdP login itself succeeds, the user is
   * redirected back, and only then does the callback fail — because the
   * failure is in the server-to-server code exchange, not in the browser flow.
   */
  authentication?: TokenAuthMethod;
}

export interface GenericOAuthConfig {
  providerId: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes: string[];
  authentication?: TokenAuthMethod;
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
  return parsed.map((p) => {
    // Reject an unrecognised value rather than passing it through. better-auth
    // treats anything that is not exactly "basic" as "put the credentials in
    // the body", so a typo like "Basic" or "client_secret_basic" would silently
    // select the opposite of what was asked for and surface much later as a
    // failed callback. Failing loudly here is the whole point of the field.
    if (p.authentication !== undefined && !TOKEN_AUTH_METHODS.includes(p.authentication)) {
      throw new Error(
        `OIDC_PROVIDERS: provider "${p.providerId}" has authentication "${p.authentication}"; ` +
          `expected one of ${TOKEN_AUTH_METHODS.map((m) => `"${m}"`).join(", ")}.`,
      );
    }
    return {
      providerId: p.providerId,
      discoveryUrl: p.discoveryUrl,
      clientId: p.clientId,
      clientSecret: p.clientSecret,
      scopes: p.scopes ?? ["openid", "email", "profile"],
      // Passed through verbatim (undefined included) so better-auth keeps its
      // own default when unset.
      authentication: p.authentication,
    };
  });
}
