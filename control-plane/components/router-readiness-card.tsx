import { AlertTriangle, CheckCircle2, Info } from "lucide-react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import type { RouterReadinessView } from "@/lib/obleth";

/**
 * Routing readiness: the gateway's own lints for the known ways `auto`
 * misroutes, surfaced before a user hits one. Server-computed — this
 * component only renders the findings, ordered warnings-first.
 */
export function RouterReadinessCard({ readiness }: { readiness: RouterReadinessView | null }) {
  if (!readiness) return null;
  const findings = [...readiness.findings].sort((a, b) =>
    a.severity === b.severity ? 0 : a.severity === "warn" ? -1 : 1,
  );
  const warnCount = findings.filter((f) => f.severity === "warn").length;

  return (
    <Card>
      <CardHeader>
        <CardTitle>Routing readiness</CardTitle>
        <CardDescription>
          {readiness.pool_size} model{readiness.pool_size === 1 ? "" : "s"} in the auto pool.
          These checks catch configurations that route badly before a user hits one.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {findings.length === 0 ? (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <CheckCircle2 className="h-4 w-4 text-emerald-500" aria-hidden="true" />
            Nothing to flag — the auto pool looks routable.
          </p>
        ) : (
          <ul className="space-y-3">
            {findings.map((f, i) => (
              <li key={`${f.code}-${i}`} className="flex gap-2.5">
                {f.severity === "warn" ? (
                  <AlertTriangle
                    className="mt-0.5 h-4 w-4 shrink-0 text-amber-500"
                    aria-label="warning"
                  />
                ) : (
                  <Info
                    className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
                    aria-label="info"
                  />
                )}
                <div className="min-w-0">
                  <p className="text-sm font-medium leading-snug">{f.title}</p>
                  <p className="mt-0.5 max-w-prose text-xs leading-snug text-muted-foreground">
                    {f.detail}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )}
        {warnCount > 0 && (
          <p className="mt-3 text-[11px] text-muted-foreground">
            Warnings will visibly misroute or waste; info rows are defaults worth knowing about.
          </p>
        )}
      </CardContent>
    </Card>
  );
}
