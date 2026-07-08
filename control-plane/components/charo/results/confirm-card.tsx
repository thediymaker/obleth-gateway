"use client";

import { Button } from "@/components/ui/button";
import { Rail } from "@/components/charo/rail";

// Inline approval card shown when Charo's brain proposes a confirmation-gated
// tool (e.g. a benchmark, which sends real billed load). Running is a deliberate
// operator action — the brain never starts one silently.
export function ConfirmCard({
  pending,
  onRun,
  onCancel,
  disabled,
}: {
  pending: { name: string; args: unknown };
  onRun: () => void;
  onCancel: () => void;
  disabled?: boolean;
}) {
  const args = (pending.args ?? {}) as Record<string, unknown>;
  const model = typeof args.model === "string" ? args.model : undefined;
  return (
    <Rail className="w-full py-0.5">
      <div className="text-[13px] font-medium text-foreground">
        Run {pending.name}{model ? ` on ${model}` : ""}?
      </div>
      <p className="mt-0.5 text-[12px] text-muted-foreground">
        This sends real load through the gateway, billed to the internal tenant.
      </p>
      <div className="mt-2 flex gap-2">
        <Button size="sm" onClick={onRun} disabled={disabled}>Run</Button>
        <Button size="sm" variant="ghost" onClick={onCancel} disabled={disabled}>Cancel</Button>
      </div>
    </Rail>
  );
}
