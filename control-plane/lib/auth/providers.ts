/** How the client authenticates to the IdP's token endpoint. */
export type TokenAuthMethod = "basic" | "post";

const TOKEN_AUTH_METHODS: readonly TokenAuthMethod[] = ["basic", "post"];

/**
 * Which OIDC claim to read each user field from, overriding the standard claim
 * of the same name.
 *
 * Institutional IdPs routinely release an `email` that is not the identifier
 * the institution actually keys accounts on. Globus is a clear example: for an
 * ASU identity it sends `email: "Johnathan.Lee@asu.edu"` (a display alias)
 * while `preferred_username` carries the canonical `jlee379@asu.edu`. Without a
 * mapping, obleth keys the account on the alias, so the same human arrives as a
 * second, unrecognised user.
 *
 *     "claims": { "email": "preferred_username" }
 *
 * Only these three fields are mappable, because they are the only profile
 * fields better-auth writes onto a user. An unset field keeps the standard
 * claim; a claim that is missing or non-string in a given token falls back to
 * the standard claim rather than writing null.
 */
export interface ClaimMapping {
  email?: string;
  name?: string;
  image?: string;
}

const MAPPABLE_FIELDS = ["email", "name", "image"] as const;
type MappableField = (typeof MAPPABLE_FIELDS)[number];

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
  /** See {@link ClaimMapping}. */
  claims?: ClaimMapping;
  /**
   * Re-apply the profile (and therefore the claim mapping) to an EXISTING user
   * on every sign-in, instead of only at sign-up. Off by default, matching
   * better-auth.
   *
   * Turn this on when correcting a mapping after users already exist: without
   * it, a fixed `claims.email` only affects accounts created from then on, and
   * everyone who already signed in keeps the wrong address forever.
   */
  overrideUserInfo?: boolean;
}

export interface GenericOAuthConfig {
  providerId: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes: string[];
  authentication?: TokenAuthMethod;
  /** Built from `claims`; better-auth calls this with the raw OIDC profile. */
  mapProfileToUser?: (profile: Record<string, unknown>) => Record<string, unknown>;
  overrideUserInfo?: boolean;
}

/**
 * Turn a declarative {@link ClaimMapping} into the `mapProfileToUser` callback
 * better-auth expects. Returns undefined when nothing is mapped, so the
 * provider config stays exactly as it was before this feature existed.
 *
 * OIDC_PROVIDERS is JSON and cannot carry a function, which is why the mapping
 * is declarative and the callback is synthesised here.
 */
function buildProfileMapper(
  claims: ClaimMapping | undefined,
): ((profile: Record<string, unknown>) => Record<string, unknown>) | undefined {
  const entries = Object.entries(claims ?? {}).filter(([, claim]) => Boolean(claim));
  if (entries.length === 0) return undefined;
  return (profile) => {
    const mapped: Record<string, unknown> = {};
    for (const [field, claim] of entries) {
      const value = profile[claim as string];
      // Only override with a usable string. A missing or non-string claim must
      // fall through to better-auth's standard handling -- writing undefined
      // here would blank out a field the IdP did supply.
      if (typeof value === "string" && value.trim() !== "") mapped[field] = value;
    }
    return mapped;
  };
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
    // Same reasoning as `authentication`: an unrecognised key in `claims` would
    // otherwise be accepted and do nothing, which is indistinguishable from a
    // mapping that silently failed. Name the bad key.
    for (const field of Object.keys(p.claims ?? {})) {
      if (!MAPPABLE_FIELDS.includes(field as MappableField)) {
        throw new Error(
          `OIDC_PROVIDERS: provider "${p.providerId}" maps unknown claim field "${field}"; ` +
            `expected one of ${MAPPABLE_FIELDS.map((f) => `"${f}"`).join(", ")}.`,
        );
      }
      const claim = (p.claims as Record<string, unknown>)[field];
      if (typeof claim !== "string" || claim.trim() === "") {
        throw new Error(
          `OIDC_PROVIDERS: provider "${p.providerId}" maps "${field}" to a non-string claim name.`,
        );
      }
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
      mapProfileToUser: buildProfileMapper(p.claims),
      overrideUserInfo: p.overrideUserInfo,
    };
  });
}
