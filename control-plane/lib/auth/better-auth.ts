import { betterAuth, type BetterAuthOptions } from "better-auth";
import {
  APIError,
  createAuthMiddleware,
  getAuthoritativeSessionFromCtx,
} from "better-auth/api";
import { admin, genericOAuth } from "better-auth/plugins";
import { getDb } from "@/lib/db";
import { oidcProviders } from "@/lib/auth/providers";
import { requireActiveAccountForAdminApi } from "@/lib/auth/admin-access";

export const SELF_ADMIN_REMOVAL_ERROR = "You cannot remove admin access from your own account";
export const LAST_ADMIN_REMOVAL_ERROR = "Cannot remove the last active admin";

type UserRow = { id: string; role?: string | null; status?: string | null; banned?: unknown };

// Request bodies arrive as JSON or form data, so a ban flag may be a string.
function isBanned(value: unknown): boolean {
  return value === true || value === "true";
}

function hasAdminRole(role: unknown): boolean {
  const roles = Array.isArray(role) ? role : String(role ?? "").split(",");
  return roles.some((r) => String(r).trim() === "admin");
}

function isActiveAdmin(user: UserRow): boolean {
  return hasAdminRole(user.role) && user.status === "active" && !isBanned(user.banned);
}

/**
 * Whether a built-in admin-plugin call would leave `userId` without admin
 * access, or `null` when the path cannot. `update-user` passes its `data`
 * straight to the adapter, so a role, status, or ban field there counts too.
 */
function strippedUserId(path: string, body: unknown): string | null {
  const b = (body ?? {}) as { userId?: unknown; role?: unknown; data?: Record<string, unknown> };
  const userId = b.userId == null ? null : String(b.userId);
  switch (path) {
    case "/admin/set-role":
      return hasAdminRole(b.role) ? null : userId;
    case "/admin/ban-user":
    case "/admin/remove-user":
      return userId;
    case "/admin/update-user": {
      const data = b.data ?? {};
      const has = (k: string) => Object.prototype.hasOwnProperty.call(data, k);
      const strips =
        (has("role") && !hasAdminRole(data.role)) ||
        (has("status") && data.status !== "active") ||
        isBanned(data.banned);
      return strips ? userId : null;
    }
    default:
      return null;
  }
}

/**
 * The dashboard's user actions refuse to strip admin access from the caller or
 * from the last active admin (`lib/auth/users.ts`), but better-auth also serves
 * its own `/api/auth/admin/*` endpoints, which would otherwise bypass that rule
 * and could lock every administrator out. The same two refusals apply here.
 */
async function guardAdminAccessChanges(ctx: Parameters<typeof getAuthoritativeSessionFromCtx>[0]) {
  const target = strippedUserId(ctx.path, ctx.body);
  if (!target) return;
  const session = await getAuthoritativeSessionFromCtx(ctx);
  if (session?.user.id === target) {
    throw new APIError("FORBIDDEN", { message: SELF_ADMIN_REMOVAL_ERROR });
  }
  const adapter = ctx.context.adapter;
  const row = await adapter.findOne<UserRow>({ model: "user", where: [{ field: "id", value: target }] });
  if (!row || !isActiveAdmin(row)) return;
  // Filter to the other active admins in the query itself: `findMany` returns
  // one capped page, so scanning every active user could miss the admins.
  // `role` is exactly "admin" or "user" (see lib/auth/users.ts), so an exact
  // match is both correct and narrower than a substring/comma-list `contains`.
  const others = await adapter.findMany<UserRow>({
    model: "user",
    where: [
      { field: "role", value: "admin" },
      { field: "status", value: "active" },
      { field: "id", operator: "ne", value: target },
    ],
  });
  if (!others.some(isActiveAdmin)) {
    throw new APIError("FORBIDDEN", { message: LAST_ADMIN_REMOVAL_ERROR });
  }
}

const adminApiBeforeHook = createAuthMiddleware(async (ctx) => {
  await requireActiveAccountForAdminApi(ctx);
  await guardAdminAccessChanges(ctx);
});

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
 *
 * Exported with injectable storage so tests can run the real configuration
 * (hooks, plugins, sign-up policy) against an in-memory adapter.
 */
export function createAuth(
  database: BetterAuthOptions["database"] = getDb(),
  authSecret: string = secret(),
) {
  return betterAuth({
    database,
    secret: authSecret,
    baseURL: process.env.BETTER_AUTH_URL ?? "http://localhost:3000",
    trustedOrigins: trustedOrigins(),
    // Accounts come from OIDC or the seeded break-glass admin; an open
    // email sign-up endpoint would let anyone mint a (pending) account.
    emailAndPassword: { enabled: true, disableSignUp: true },
    hooks: { before: adminApiBeforeHook },
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
