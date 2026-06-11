"use client";

import { useRef, useTransition } from "react";
import { Info } from "lucide-react";
import { createTenantAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function CreateTenant() {
  const formRef = useRef<HTMLFormElement>(null);
  const [pending, start] = useTransition();

  return (
    <form
      ref={formRef}
      action={(fd) =>
        start(async () => {
          await createTenantAction(fd);
          formRef.current?.reset();
        })
      }
      className="flex flex-wrap items-end gap-3"
    >
      <div className="space-y-1.5">
        <Label htmlFor="tenant-name">Name</Label>
        <Input id="tenant-name" name="name" required placeholder="chatbot" className="w-40" />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="tenant-weight">Weight</Label>
        <Input id="tenant-weight" name="weight" type="number" min={1} defaultValue={100} className="w-24" />
      </div>
      <div className="space-y-1.5">
        <div className="flex items-center gap-1.5">
          <Label htmlFor="tenant-tokens-per-minute">Tokens / min</Label>
          <InfoTip>Blank or 0 means no token-rate cap.</InfoTip>
        </div>
        <Input
          id="tenant-tokens-per-minute"
          name="tokens_per_minute"
          type="number"
          min={0}
          placeholder="No cap"
          className="w-32"
        />
      </div>
      <div className="space-y-1.5">
        <div className="flex items-center gap-1.5">
          <Label htmlFor="tenant-max-in-flight">Max in-flight</Label>
          <InfoTip>Optional per-tenant concurrent request cap. Blank means no tenant-specific cap.</InfoTip>
        </div>
        <Input
          id="tenant-max-in-flight"
          name="max_in_flight"
          type="number"
          min={1}
          placeholder="No cap"
          className="w-28"
        />
      </div>
      <Button type="submit" disabled={pending}>
        {pending ? "Creating..." : "Create tenant"}
      </Button>
    </form>
  );
}

function InfoTip({ children }: { children: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={children}
          className="inline-flex h-4 w-4 items-center justify-center rounded-sm text-muted-foreground hover:bg-muted/30 hover:text-foreground"
        >
          <Info className="h-3.5 w-3.5" />
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" align="start" className="max-w-xs leading-relaxed">
        {children}
      </TooltipContent>
    </Tooltip>
  );
}
