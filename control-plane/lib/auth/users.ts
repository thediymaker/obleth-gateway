import type { PoolClient } from "pg";
import { getDb } from "@/lib/db";

export interface AdminUser {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "pending" | "active";
  tenantId: string | null;
}

/** Outcome of a role/status change guarded against removing the last active admin. */
export type UserChangeResult = "ok" | "not-found" | "last-admin";

// Transaction-scoped advisory lock serializing every change that can strip
// admin access, so two concurrent demotions cannot each see the other as the
// remaining admin. Distinct from the gateway's migration lock (…_0001).
const ADMIN_CHANGE_LOCK_KEY = 0x0b1e_7480_0101;

export async function listUsers(): Promise<AdminUser[]> {
  const { rows } = await getDb().query(
    `select id, email, role, status, "tenantId" as "tenantId" from "user" order by "createdAt" desc`,
  );
  return rows as AdminUser[];
}

/**
 * Run `write` for user `id` under the admin-change lock. When `removesAdmin`
 * and the target is currently an active admin, refuse (without writing) if no
 * other active admin exists.
 */
async function guardedUserWrite(
  id: string,
  removesAdmin: boolean,
  write: (client: PoolClient) => Promise<unknown>,
): Promise<UserChangeResult> {
  const client = await getDb().connect();
  try {
    await client.query("begin");
    await client.query("select pg_advisory_xact_lock($1)", [ADMIN_CHANGE_LOCK_KEY]);
    const { rows } = await client.query<{ role: string; status: string }>(
      `select role, status from "user" where id = $1`,
      [id],
    );
    const target = rows[0];
    let result: UserChangeResult = "ok";
    if (!target) {
      result = "not-found";
    } else if (removesAdmin && target.role === "admin" && target.status === "active") {
      const others = await client.query<{ count: number }>(
        `select count(*)::int as count from "user" where role = 'admin' and status = 'active' and id <> $1`,
        [id],
      );
      if (Number(others.rows[0]?.count ?? 0) === 0) result = "last-admin";
    }
    if (result === "ok") await write(client);
    await client.query(result === "ok" ? "commit" : "rollback");
    return result;
  } catch (e) {
    await client.query("rollback").catch(() => {});
    throw e;
  } finally {
    client.release();
  }
}

export async function assignUser(
  id: string,
  role: "admin" | "user",
  tenantId: string | null,
): Promise<UserChangeResult> {
  return guardedUserWrite(id, role !== "admin", (client) =>
    client.query(
      `update "user" set role = $1, "tenantId" = $2, status = 'active', "updatedAt" = now() where id = $3`,
      [role, tenantId, id],
    ),
  );
}

export async function setUserStatus(
  id: string,
  status: "active" | "pending",
): Promise<UserChangeResult> {
  return guardedUserWrite(id, status !== "active", (client) =>
    client.query(`update "user" set status = $1, "updatedAt" = now() where id = $2`, [status, id]),
  );
}
