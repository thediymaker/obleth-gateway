import { redirect } from "next/navigation";

/** Key access moved to a tab of Reports; old links keep their filters. */
export default async function KeyAccessPage({ searchParams }: { searchParams: Promise<{ range?: string; tenant?: string; q?: string }> }) {
  const q = await searchParams;
  const p = new URLSearchParams({ tab: "key-access" });
  for (const k of ["range", "tenant", "q"] as const) if (q[k]) p.set(k, q[k]!);
  redirect(`/reports?${p.toString()}`);
}
