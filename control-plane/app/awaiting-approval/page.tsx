import { getSession } from "@/lib/auth/session";
import { redirect } from "next/navigation";
import type { SessionUser } from "@/lib/auth/session";

export const dynamic = "force-dynamic";

/**
 * Determines where an awaiting-approval visitor should be redirected, or null
 * if they should stay on this page.
 *
 * Cases:
 *   no session          → "/login"
 *   active + admin      → "/"
 *   active + user + tenant → "/portal/models"
 *   active + user + no tenant → null  (terminal state — stay and show message)
 *   pending             → null  (stay and show "awaiting approval" message)
 */
export function awaitingApprovalTarget(
  s: SessionUser | null,
): "/" | "/portal/models" | "/login" | null {
  if (!s) return "/login";
  if (s.status === "active" && s.role === "admin") return "/";
  if (s.status === "active" && s.role !== "admin" && s.tenantId) return "/portal/models";
  return null;
}

export default async function AwaitingApprovalPage() {
  const s = await getSession();
  const target = awaitingApprovalTarget(s);
  if (target) redirect(target);

  // s is non-null here (null would have targeted "/login")
  const session = s!;

  const isActiveMissingTenant =
    session.status === "active" && session.role !== "admin" && !session.tenantId;

  return (
    <div className="flex min-h-screen items-center justify-center bg-background px-4 text-center">
      <div className="max-w-sm space-y-3">
        {isActiveMissingTenant ? (
          <>
            <h1 className="text-xl font-semibold">No tenant assigned</h1>
            <p className="text-sm text-muted-foreground">
              Signed in as {session.email}. Your account is active but hasn&apos;t been assigned to
              a tenant yet — an administrator needs to assign you one.
            </p>
          </>
        ) : (
          <>
            <h1 className="text-xl font-semibold">Account awaiting approval</h1>
            <p className="text-sm text-muted-foreground">
              Signed in as {session.email}. An administrator must assign your access before you can
              continue.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
