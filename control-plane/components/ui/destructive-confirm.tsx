"use client";

import * as React from "react";
import { AlertTriangle } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogFooter,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export interface DestructiveConfirmProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: React.ReactNode;
  /** Word the user must type verbatim to unlock the action. */
  confirmWord?: string;
  /** Label on the acknowledgement checkbox. */
  checkboxLabel: string;
  /** Label on the destructive confirm button. */
  confirmLabel: string;
  /** Disables controls and shows progress while the action runs. */
  pending?: boolean;
  onConfirm: () => void;
}

/// A two-gate destructive confirmation: the user must both tick an
/// acknowledgement box and type a confirmation word before the action button
/// unlocks. Resets its internal gate state whenever it reopens.
export function DestructiveConfirm({
  open,
  onOpenChange,
  title,
  description,
  confirmWord = "I AGREE",
  checkboxLabel,
  confirmLabel,
  pending = false,
  onConfirm,
}: DestructiveConfirmProps) {
  const [acked, setAcked] = React.useState(false);
  const [typed, setTyped] = React.useState("");

  React.useEffect(() => {
    if (open) {
      setAcked(false);
      setTyped("");
    }
  }, [open]);

  const unlocked = acked && typed.trim() === confirmWord;

  return (
    <Dialog open={open} onOpenChange={(o) => (pending ? null : onOpenChange(o))}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 text-destructive">
            <AlertTriangle className="h-4 w-4" />
            {title}
          </DialogTitle>
          <DialogDescription asChild>
            <div className="space-y-2 pt-1 text-sm text-muted-foreground">{description}</div>
          </DialogDescription>
        </DialogHeader>

        <label className="flex items-start gap-2 text-sm">
          <input
            type="checkbox"
            className="mt-0.5 h-4 w-4 rounded border-border accent-[hsl(var(--destructive))]"
            checked={acked}
            disabled={pending}
            onChange={(e) => setAcked(e.target.checked)}
          />
          <span>{checkboxLabel}</span>
        </label>

        <div className="space-y-1.5">
          <label className="text-xs text-muted-foreground">
            Type <span className="font-mono font-semibold text-foreground">{confirmWord}</span> to
            confirm
          </label>
          <Input
            value={typed}
            disabled={pending}
            onChange={(e) => setTyped(e.target.value)}
            placeholder={confirmWord}
            autoComplete="off"
            spellCheck={false}
          />
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            disabled={pending}
            onClick={() => onOpenChange(false)}
            type="button"
          >
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={!unlocked || pending}
            onClick={onConfirm}
            type="button"
          >
            {pending ? "Working…" : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
