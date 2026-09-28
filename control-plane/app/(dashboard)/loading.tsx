import { DashboardSkeleton } from "@/components/page-skeletons";

// Route-level pending state for every dashboard page without its own: the
// pages are force-dynamic server components that block on admin-API
// fan-outs, so navigation shows this skeleton instead of freezing on the
// previous page. The redesigned pages carry skeletons shaped like themselves.
export default function DashboardLoading() {
  return <DashboardSkeleton />;
}
