import { AppShell } from "@/components/app-shell";
import { CharoRoot } from "@/components/charo/charo-root";
import { getSession } from "@/lib/auth/session";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { redirect } from "next/navigation";
import { CONTROL_PLANE_VERSION } from "@/lib/version";

export default async function DashboardLayout({ children }: { children: React.ReactNode }) {
  const session = await getSession();
  if (!session) redirect("/login");
  if (session.status !== "active") redirect("/awaiting-approval");
  if (session.role !== "admin") redirect("/portal/models");

  // Charo is enabled by default; only hide it when the operator has explicitly
  // turned it off in Settings. If the gateway is unreachable, fail open (show it).
  const charo = await safe(obleth.getCharoSettings(), {
    enabled: true,
    brain_model: null,
    tools_enabled: {},
    bench_max_concurrency: 40,
    bench_max_duration_s: 120,
    bench_max_requests: 500,
  });

  return (
    <AppShell username={session.email} version={CONTROL_PLANE_VERSION}>
      {children}
      {charo.enabled && <CharoRoot />}
    </AppShell>
  );
}
