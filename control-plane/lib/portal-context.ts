import { cookies } from "next/headers";
import { getSession, type SessionUser } from "@/lib/auth/session";
import { userIdentity } from "@/lib/auth/personal";
import { accounts, type MemberRole } from "@/lib/obleth-accounts";

export const PORTAL_TENANT_COOKIE = "portal_tenant";

export interface PortalTenant {
  id: string;
  name: string;
  kind: "standard" | "personal";
  role: MemberRole;
}

export interface PortalContext {
  identity: { issuer: string; subject: string };
  tenants: PortalTenant[];
  current: PortalTenant;
}

export async function loadPortalContext(session: SessionUser, requested?: string): Promise<PortalContext> {
  const identity = await userIdentity(session.id);
  if (!identity) throw new Error("No identity");
  const memberships = await accounts.memberships(identity.issuer, identity.subject);
  const tenants: PortalTenant[] = memberships.map((m) => ({
    id: m.tenant_id,
    name: m.tenant_name,
    kind: m.kind,
    role: m.role,
  }));
  if (tenants.length === 0) throw new Error("No tenant assigned to this user");
  const current =
    tenants.find((t) => t.id === requested) ?? tenants.find((t) => t.kind === "personal") ?? tenants[0];
  return { identity, tenants, current };
}

export async function currentPortalContext(): Promise<PortalContext> {
  const session = await getSession();
  if (!session) throw new Error("Unauthorized");
  const requested = (await cookies()).get(PORTAL_TENANT_COOKIE)?.value;
  return loadPortalContext(session, requested);
}
