// Typed client for the obleth accounts admin routes (members, identities,
// owned keys, member counts). Same transport as lib/obleth.ts.
import type { ApiKey } from "./obleth";

const BASE = process.env.OBLETH_ADMIN_BASE_URL ?? "http://localhost:9180";
const AUDIT_ACTOR_HEADER = "X-Obleth-Audit-Actor";

export interface AuditOptions {
  auditActor?: string | null;
}

export type MemberRole = "owner" | "manager" | "member";

export interface ProvisionedIdentity {
  tenant_id: string;
  tenant_name: string;
  key_id: string;
  tenant_created: boolean;
}

export interface Membership {
  tenant_id: string;
  tenant_name: string;
  kind: "standard" | "personal";
  role: MemberRole;
}

export interface IdentityRef {
  issuer: string;
  subject: string;
  email: string;
  personal_tenant_id: string | null;
}

export interface Member {
  tenant_id: string;
  issuer: string;
  subject: string;
  email: string | null;
  role: MemberRole;
  created_at: string;
}

export type OwnedKey = ApiKey & { owner_issuer: string; owner_subject: string };

export interface MemberCount {
  tenant_id: string;
  name: string;
  members: number;
}

function adminToken(): string {
  const token = process.env.OBLETH_ADMIN_TOKEN;
  if (!token) {
    throw new Error("OBLETH_ADMIN_TOKEN is not set. The control plane requires the management API admin token to operate.");
  }
  return token;
}

function auditHeaders(options?: AuditOptions): Record<string, string> {
  const actor = options?.auditActor?.trim().replace(/[\r\n]+/g, " ");
  return actor ? { [AUDIT_ACTOR_HEADER]: actor } : {};
}

function qs(params: Record<string, string>): string {
  return `?${new URLSearchParams(params).toString()}`;
}

async function api<T>(path: string, init: RequestInit = {}, options?: AuditOptions): Promise<T> {
  const res = await fetch(`${BASE}/api/v1${path}`, {
    cache: "no-store",
    ...init,
    headers: {
      Authorization: `Bearer ${adminToken()}`,
      "Content-Type": "application/json",
      ...auditHeaders(options),
    },
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    let message = text;
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed.error === "string") message = parsed.error;
    } catch {
      // Non-JSON body; fall back to the raw text.
    }
    throw new Error(message || `request failed with status ${res.status}`);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

const tenantPath = (id: string) => `/tenants/${encodeURIComponent(id)}`;
const send = (method: string, body: unknown): RequestInit => ({ method, body: JSON.stringify(body) });

export const accounts = {
  provisionIdentity: (body: { issuer: string; subject: string; email: string; rehome?: boolean }) =>
    api<ProvisionedIdentity>("/identities", send("POST", body)),
  memberships: (issuer: string, subject: string) =>
    api<Membership[]>(`/identities/memberships${qs({ issuer, subject })}`),
  identitiesByEmail: (email: string) => api<IdentityRef[]>(`/identities${qs({ email })}`),
  listMembers: (tenantId: string) => api<Member[]>(`${tenantPath(tenantId)}/members`),
  upsertMember: (
    tenantId: string,
    body: { issuer: string; subject: string; email?: string; role: "member" | "manager" },
    opts?: AuditOptions,
  ) => api<Member>(`${tenantPath(tenantId)}/members`, send("PUT", body), opts),
  removeMember: (tenantId: string, issuer: string, subject: string, opts?: AuditOptions) =>
    api<void>(`${tenantPath(tenantId)}/members${qs({ issuer, subject })}`, { method: "DELETE" }, opts),
  listOwnedKeys: (tenantId: string, owner?: { issuer: string; subject: string }) =>
    api<OwnedKey[]>(`${tenantPath(tenantId)}/owned-keys${owner ? qs(owner) : ""}`),
  createOwnedKey: (
    tenantId: string,
    body: { issuer: string; subject: string; name: string },
    opts?: AuditOptions,
  ) => api<{ key: OwnedKey; secret: string }>(`${tenantPath(tenantId)}/owned-keys`, send("POST", body), opts),
  memberCounts: () => api<MemberCount[]>("/member-counts"),
};
