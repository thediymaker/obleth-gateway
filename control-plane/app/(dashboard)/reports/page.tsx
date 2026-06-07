import { ReportsDashboard } from "@/components/reports-dashboard";

export const dynamic = "force-dynamic";

export default function ReportsPage() {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Reports</h1>
        <p className="text-sm text-muted-foreground">
          Historical usage from the permanent daily rollup. Pick a date range, explore the charts,
          and export a CSV with exactly the columns you need.
        </p>
      </div>
      <ReportsDashboard />
    </div>
  );
}
