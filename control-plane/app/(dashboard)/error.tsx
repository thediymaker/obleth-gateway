"use client";

import { AlertTriangle, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";

// Route-level error boundary for the dashboard pages. Most upstream reads
// degrade gracefully via safe(), so landing here means an unexpected render
// failure — show a styled recovery surface instead of Next's default screen.
export default function DashboardError({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  return (
    <div className="flex min-h-[50vh] items-center justify-center">
      <div className="max-w-md rounded-md border border-border bg-card px-6 py-8 text-center">
        <AlertTriangle className="mx-auto h-6 w-6 text-[hsl(38_65%_62%)]" />
        <h1 className="mt-3 text-base font-semibold tracking-tight">This page failed to load</h1>
        <p className="mt-1.5 text-sm text-muted-foreground">
          The dashboard hit an unexpected error while rendering. The gateway itself is unaffected.
        </p>
        {error.digest && (
          <p className="mt-2 font-mono text-[11px] text-muted-foreground/70">ref {error.digest}</p>
        )}
        <Button type="button" variant="secondary" size="sm" className="mt-4" onClick={reset}>
          <RefreshCw className="h-3.5 w-3.5" />
          Try again
        </Button>
      </div>
    </div>
  );
}
