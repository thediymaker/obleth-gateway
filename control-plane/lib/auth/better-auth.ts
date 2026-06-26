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
    emailAndPassword: { enabled: true },
    user: {
      additionalFields: {
        role: { type: "string", defaultValue: "user", input: false },
        status: { type: "string", defaultValue: "pending", input: false },
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
