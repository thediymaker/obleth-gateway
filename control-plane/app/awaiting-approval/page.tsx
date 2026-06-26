import { getSession } from "@/lib/auth/session";
import { redirect } from "next/navigation";

export const dynamic = "force-dynamic";

export default async function AwaitingApprovalPage() {
  const s = await getSession();
  if (!s) redirect("/login");
  if (s.status === "active") redirect(s.role === "admin" ? "/" : "/portal/models");
  return (
    <div className="flex min-h-screen items-center justify-center bg-background px-4 text-center">
      <div className="max-w-sm space-y-3">
        <h1 className="text-xl font-semibold">Account awaiting approval</h1>
        <p className="text-sm text-muted-foreground">
          Signed in as {s.email}. An administrator must assign your access before you can continue.
        </p>
      </div>
    </div>
  );
}
