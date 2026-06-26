import { getDb } from "@/lib/db";

export interface AdminUser {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "pending" | "active";
  tenantId: string | null;
}

export async function listUsers(): Promise<AdminUser[]> {
  const { rows } = await getDb().query(
    `select id, email, role, status, "tenantId" as "tenantId" from "user" order by "createdAt" desc`,
  );
  return rows as AdminUser[];
}

export async function assignUser(
  id: string,
  role: "admin" | "user",
  tenantId: string | null,
): Promise<void> {
  await getDb().query(
    `update "user" set role = $1, "tenantId" = $2, status = 'active', "updatedAt" = now() where id = $3`,
    [role, tenantId, id],
  );
}

export async function setUserStatus(
  id: string,
  status: "active" | "pending",
): Promise<void> {
  await getDb().query(
    `update "user" set status = $1, "updatedAt" = now() where id = $2`,
    [status, id],
  );
}
