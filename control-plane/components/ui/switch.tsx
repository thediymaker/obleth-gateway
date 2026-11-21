"use client";

import type { ChangeEvent, ReactNode } from "react";
import { cn } from "@/lib/utils";

/**
 * An on/off switch that is a real checkbox underneath, so it submits `on`
 * with the form. Controlled when `checked` is given. With `children`, the
 * whole card is the click target.
 */
export function Switch({
  name,
  label,
  checked,
  defaultChecked,
  onChange,
  disabled,
  children,
  className,
}: {
  name?: string;
  label: string;
  checked?: boolean;
  defaultChecked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  children?: ReactNode;
  className?: string;
}) {
  const controlled = checked !== undefined;
  const track = (
    <span className="relative inline-flex h-5 w-9 shrink-0 items-center">
      <input
        type="checkbox"
        role="switch"
        name={name}
        aria-label={children ? undefined : label}
        disabled={disabled}
        className="peer sr-only"
        {...(controlled
          ? { checked, onChange: (e: ChangeEvent<HTMLInputElement>) => onChange?.(e.target.checked) }
          : { defaultChecked, onChange: onChange ? (e: ChangeEvent<HTMLInputElement>) => onChange(e.target.checked) : undefined })}
      />
      <span className="absolute inset-0 rounded-full bg-muted transition-colors peer-checked:bg-foreground peer-focus-visible:ring-2 peer-focus-visible:ring-ring peer-disabled:opacity-40" />
      <span className="absolute left-[3px] h-3.5 w-3.5 rounded-full bg-muted-foreground transition-transform peer-checked:translate-x-4 peer-checked:bg-background" />
    </span>
  );
  if (!children) {
    return <label className={cn("inline-flex", disabled ? "cursor-not-allowed" : "cursor-pointer", className)}>{track}</label>;
  }
  return (
    <label
      className={cn(
        "flex min-h-10 items-center justify-between gap-3 rounded-lg border border-border px-3 py-2 text-[13px] transition-colors hover:border-muted-foreground/60",
        disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer",
        className,
      )}
    >
      <span className="min-w-0">{children}</span>
      {track}
    </label>
  );
}
