import { Pool } from "pg";

let pool: Pool | null = null;

/**
 * Lazily-created Postgres pool used ONLY for better-auth's tables. Everything
 * else in the control plane goes through the Management API (lib/obleth.ts).
 * Lazy so `next build` (which evaluates server modules without a database)
 * does not fail.
 */
export function getDb(): Pool {
  if (pool) return pool;
  const url = process.env.DATABASE_URL;
  if (!url) {
    throw new Error(
      "DATABASE_URL is not set. The control plane needs a Postgres connection for the auth (user/session) tables. " +
        "Point it at the same database the gateway uses, e.g. postgres://obleth:<pw>@postgres:5432/obleth.",
    );
  }
  pool = new Pool({ connectionString: url, max: 5 });
  return pool;
}
