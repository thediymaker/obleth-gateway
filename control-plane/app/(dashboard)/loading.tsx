// Route-level pending state for every dashboard page: the pages are
// force-dynamic server components that block on admin-API fan-outs, so
// navigation shows this skeleton instead of freezing on the previous page.
export default function DashboardLoading() {
  return (
    <div className="animate-pulse space-y-5" aria-busy aria-label="Loading page">
      <div className="space-y-2">
        <div className="h-6 w-48 rounded-md bg-muted/40" />
        <div className="h-4 w-80 max-w-full rounded-md bg-muted/25" />
      </div>
      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="h-24 rounded-md border border-border bg-card/40" />
        ))}
      </div>
      <div className="h-96 rounded-md border border-border bg-card/40" />
      <div className="h-64 rounded-md border border-border bg-card/40" />
    </div>
  );
}
