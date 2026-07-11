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
  /** Label on the destructive confirm button. Defaults to "Delete". */
  confirmLabel?: string;
}

/// Styled replacement for `window.confirm` on routine destructive actions:
/// one Cancel/Confirm dialog, no typing gate. Catastrophic operations
/// (restore, secret rotation, bulk deletes) should keep `DestructiveConfirm`.
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel = "Delete",
  onConfirm,
}: ConfirmOptions & {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
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
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} type="button">
            Cancel
          </Button>
          <Button variant="destructive" onClick={onConfirm} type="button">
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
///   if (!(await confirm({ title, description }))) return;
export function useConfirm() {
  const [state, setState] = React.useState<{
    opts: ConfirmOptions;
    resolve: (ok: boolean) => void;
  } | null>(null);

  const confirm = React.useCallback(
    (opts: ConfirmOptions) =>
      new Promise<boolean>((resolve) => setState({ opts, resolve })),
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
