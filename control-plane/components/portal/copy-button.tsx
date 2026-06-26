"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export function CopyButton({
  value,
  label = "Copy",
  copiedLabel = "Copied",
  size = "sm",
  variant = "outline",
  className,
}: {
  value: string;
  label?: string;
  copiedLabel?: string;
  size?: "sm" | "icon";
  variant?: "default" | "secondary" | "outline" | "ghost";
  className?: string;
}) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    await navigator.clipboard.writeText(value);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }

  return (
    <Button
      type="button"
      size={size}
      variant={variant}
      onClick={copy}
      className={cn(size === "icon" && "h-8 w-8", className)}
      aria-label={copied ? copiedLabel : label}
      title={copied ? copiedLabel : label}
    >
      {copied ? (
        <Check className="h-3.5 w-3.5" aria-hidden />
      ) : (
        <Copy className="h-3.5 w-3.5" aria-hidden />
      )}
      {size !== "icon" && (copied ? copiedLabel : label)}
    </Button>
  );
}

export function CodeBlock({
  code,
  label,
}: {
  code: string;
  label: string;
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card/40">
      <div className="flex items-center justify-between gap-3 border-b border-border/60 bg-background/35 px-3 py-2">
        <p className="text-xs font-medium text-muted-foreground">{label}</p>
        <CopyButton value={code} label="Copy" variant="ghost" />
      </div>
      <pre className="overflow-x-auto px-3 py-3 text-xs leading-relaxed text-foreground">
        <code>{code}</code>
      </pre>
    </div>
  );
}
