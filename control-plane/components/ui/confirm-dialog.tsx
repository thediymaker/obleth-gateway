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

export interface ConfirmOptions {
  title: string;
  description: React.ReactNode;
  /** Label on the confirm button: the action, in a word or two. */
  confirmLabel: string;
  /**
   * "destructive" for what cannot be taken back (delete, remove, retire,
   * cancel a running job, drop data): a red title and button. Everything
   * else (turn off or on, replace, restart, reindex) stays neutral.
   */
  tone?: "default" | "destructive";
}

/// Styled replacement for `window.confirm`: one Cancel/Confirm question with
/// no typing gate. Typed confirmation (`DestructiveConfirm`) is kept for the
/// few actions that take a whole tenant or the database with them.
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  tone = "default",
  onConfirm,
}: ConfirmOptions & {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const destructive = tone === "destructive";
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className={destructive ? "flex items-center gap-2 text-destructive" : undefined}>
            {destructive && <AlertTriangle className="h-4 w-4" aria-hidden />}
            {title}
          </DialogTitle>
          <DialogDescription asChild>
            <div className="space-y-2 pt-1 text-sm text-muted-foreground">{description}</div>
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} type="button">
            Cancel
          </Button>
          <Button variant={destructive ? "destructive" : "default"} onClick={onConfirm} type="button">
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// Promise-based confirm for handler code:
///   const { confirm, confirmElement } = useConfirm();
///   ...render {confirmElement}...
///   if (!(await confirm({ title, description, confirmLabel }))) return;
export function useConfirm() {
  const [state, setState] = React.useState<{
    opts: ConfirmOptions;
    resolve: (ok: boolean) => void;
  } | null>(null);

  // Opened one microtask later, outside whatever transition called this.
  // Callers ask from inside `startTransition(async () => ...)`, and React holds
  // a transition's updates until its action finishes -- which is waiting on
  // this dialog -- so an open set directly here never rendered and the action
  // hung with its buttons disabled.
  const confirm = React.useCallback(
    (opts: ConfirmOptions) =>
      new Promise<boolean>((resolve) => queueMicrotask(() => setState({ opts, resolve }))),
    [],
  );

  const confirmElement = state ? (
    <ConfirmDialog
      open
      onOpenChange={(open) => {
        if (!open) {
          state.resolve(false);
          setState(null);
        }
      }}
      onConfirm={() => {
        state.resolve(true);
        setState(null);
      }}
      {...state.opts}
    />
  ) : null;

  return { confirm, confirmElement };
}
