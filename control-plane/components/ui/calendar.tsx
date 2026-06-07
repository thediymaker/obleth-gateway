"use client";

import * as React from "react";
import { DayPicker } from "react-day-picker";
import "react-day-picker/style.css";
import { cn } from "@/lib/utils";

export type CalendarProps = React.ComponentProps<typeof DayPicker>;

/// Thin wrapper over react-day-picker, themed to the dashboard palette via CSS
/// variables exposed on the root. Defaults to the accent color for selection.
export function Calendar({ className, ...props }: CalendarProps) {
  return (
    <DayPicker
      className={cn("rdp-obleth p-2", className)}
      style={
        {
          "--rdp-accent-color": "hsl(var(--primary))",
          "--rdp-accent-background-color": "hsl(var(--accent))",
          "--rdp-day_button-border-radius": "0.375rem",
          "--rdp-range_middle-color": "hsl(var(--accent-foreground))",
        } as React.CSSProperties
      }
      {...props}
    />
  );
}
