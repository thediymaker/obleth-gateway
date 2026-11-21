"use client";

import { useRef, type ReactNode } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";

const FIELD = "input:not([type=hidden]):not([disabled]), textarea:not([disabled]), select:not([disabled])";

/**
 * A panel that slides over the right of the page, on the Radix dialog: focus
 * stays inside while it is open, Escape and the backdrop close it, and focus
 * returns where it was. It opens on its first field when it has one.
 * `header` replaces the title block (and its close button) for panels with
 * their own, which use SheetClose; `modal={false}` leaves the page behind it
 * usable, with no backdrop, and a click outside does not close it.
 */
export function Sheet({ open, onClose, title, description, header, children, footer, width = "w-[min(680px,100vw)]", modal = true }: {
  open: boolean;
  onClose: () => void;
  /** The heading, and the panel's name for screen readers. */
  title: string;
  description?: ReactNode;
  header?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: string;
  modal?: boolean;
}) {
  // Sheets open from buttons, menus and table rows rather than a Radix trigger,
  // so remember what had focus and hand it back on close.
  const returnTo = useRef<HTMLElement | null>(null);
  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => { if (!next) onClose(); }} modal={modal}>
      <DialogPrimitive.Portal>
        {modal && <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />}
        <DialogPrimitive.Content
          aria-label={title}
          {...(description && !header ? {} : { "aria-describedby": undefined })}
          onOpenAutoFocus={(e) => {
            returnTo.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
            const field = (e.currentTarget as HTMLElement | null)?.querySelector<HTMLElement>(FIELD);
            if (field) { e.preventDefault(); field.focus(); }
          }}
          onCloseAutoFocus={(e) => {
            if (returnTo.current?.isConnected) { e.preventDefault(); returnTo.current.focus(); }
          }}
          onInteractOutside={modal ? undefined : (e) => e.preventDefault()}
          className={cn(
            "fixed inset-y-0 right-0 z-50 flex flex-col border-l border-border bg-card shadow-2xl focus:outline-none",
            "data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right",
            width,
          )}
        >
          {header ? (
            <>
              <DialogPrimitive.Title asChild><span className="sr-only">{title}</span></DialogPrimitive.Title>
              {header}
            </>
          ) : (
            <div className="flex items-start justify-between gap-4 px-6 pb-4 pt-5">
              <div className="min-w-0">
                <DialogPrimitive.Title className="text-lg font-semibold">{title}</DialogPrimitive.Title>
                {description && <DialogPrimitive.Description className="mt-0.5 text-[12.5px] text-muted-foreground">{description}</DialogPrimitive.Description>}
              </div>
              <SheetClose />
            </div>
          )}
          <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
          {footer && <div className="border-t border-border bg-background/40 px-6 py-3.5">{footer}</div>}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

/** The sheet's close button, for a custom `header`. */
export function SheetClose() {
  return (
    <DialogPrimitive.Close aria-label="Close" className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground">
      <X className="h-4 w-4" />
    </DialogPrimitive.Close>
  );
}
