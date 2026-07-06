import { betterAuth } from "better-auth";
import { admin, genericOAuth } from "better-auth/plugins";
import { getDb } from "@/lib/db";
import { oidcProviders } from "@/lib/auth/providers";

function secret(): string {
  const s = process.env.BETTER_AUTH_SECRET ?? process.env.DASHBOARD_SESSION_SECRET;
  if (!s || s.length < 32) {
    throw new Error(
      "BETTER_AUTH_SECRET (or DASHBOARD_SESSION_SECRET) must be set to a random value of at least 32 characters.",
    );
  }
  return s;
}

/**
 * Extra origins better-auth should accept for its CSRF/origin check, on top of
 * `baseURL` (which is always trusted). Without this, hitting the dashboard from
 * anything other than the exact `BETTER_AUTH_URL` host — e.g. a LAN IP or
 * hostname in a self-hosted Docker/K8s deploy — is rejected with an
 * invalid-origin error and login fails.
 *
 * `TRUSTED_ORIGINS` is a comma-separated list of origins (scheme + host + port),
 * e.g. "http://192.168.1.50:3002,https://dashboard.internal". A single "*"
 * trusts all origins — convenient on a trusted private network, but do not use
 * it on anything internet-reachable.
 */
function trustedOrigins(): string[] {
  const raw = process.env.TRUSTED_ORIGINS;
  if (!raw) return [];
  return raw
    .split(",")
    .map((o) => o.trim())
    .filter(Boolean);
}

/**
 * Construct the better-auth instance. Kept as a factory (rather than a
 * module-scope const) because it calls `getDb()` and `secret()`, both of which
 * throw when `DATABASE_URL` / the session secret are absent. Next.js evaluates
 * server modules during `next build` without those env vars, so an eager
 * instance would break the build (the same hazard `lib/obleth.ts` avoids for its
 * admin token).
 *
 * A `pg.Pool` does not connect on construction, so `getDb()` here only risks its
 * explicit "DATABASE_URL unset" guard, not actual DB connectivity.
 */
function createAuth() {
  return betterAuth({
    database: getDb(),
    secret: secret(),
    baseURL: process.env.BETTER_AUTH_URL ?? "http://localhost:3000",
    trustedOrigins: trustedOrigins(),
    emailAndPassword: { enabled: true },
    user: {
      additionalFields: {
        role: { type: "string", defaultValue: "user", input: false },
        status: { type: "string", defaultValue: "pending", input: false },
        // additionalFields only supports "string" | "number" | "boolean" | "date",
        // so tenantId is "string" here while db/auth-schema.sql enforces uuid + a FK
        // to tenants(id). better-auth does no coercion, so the app layer must always
        // supply a valid UUID string (or null); admin assignment writes this column
        // directly via getDb() (a later task), not through better-auth's adapter.
        tenantId: { type: "string", required: false, input: false },
      },
    },
    plugins: [
      admin({ defaultRole: "user", adminRoles: ["admin"] }),
      genericOAuth({ config: oidcProviders() }),
    ],
  });
}

// Preserve the full plugin-augmented instance type (so `auth.api.*` stays typed).
type Auth = ReturnType<typeof createAuth>;

let instance: Auth | null = null;

function getAuth(): Auth {
  if (!instance) instance = createAuth();
  return instance;
}

/**
 * The better-auth instance, exposed as a lazily-resolved proxy so importing this
 * module is build-safe: the underlying instance is only constructed on first
 * property access (at request time), not when the module is loaded.
 */
export const auth = new Proxy({} as Auth, {
  get(_target, prop, receiver) {
    return Reflect.get(getAuth(), prop, receiver);
  },
}) as Auth;
