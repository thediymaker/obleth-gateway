"use client";

import { useRef, useTransition } from "react";
import { createTenantAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

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
        <Label htmlFor="tenant-tokens-per-minute">Tokens / min</Label>
        <Input
          id="tenant-tokens-per-minute"
          name="tokens_per_minute"
          type="number"
          min={1}
          defaultValue={60000}
          className="w-32"
        />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="tenant-max-in-flight">Max in-flight</Label>
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
