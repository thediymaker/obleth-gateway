"use client";

import { Check } from "lucide-react";
import { cn } from "@/lib/utils";

// Theme-consistent replacement for a native checkbox, which renders in the
// browser's own accent color and clashes with the rest of the dashboard.
// A button is a labelable element, so wrapping <label> text still toggles it.
export function Checkbox({
  checked,
  onChange,
  disabled,
  id,
  "aria-label": ariaLabel,
  className,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  id?: string;
  "aria-label"?: string;
  className?: string;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={ariaLabel}
      id={id}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors",
        checked
          ? "border-primary/60 bg-primary text-primary-foreground"
          : "border-input bg-background hover:border-border",
        disabled && "cursor-not-allowed opacity-50",
        className,
      )}
    >
      {checked && <Check className="h-3 w-3" strokeWidth={3} />}
    </button>
  );
}
