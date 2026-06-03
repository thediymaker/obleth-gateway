"use client";

import { useTransition } from "react";
import { setCapacityAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function CapacityControl({ initial }: { initial: number }) {
  const [pending, start] = useTransition();

  return (
    <form
      className="flex items-center gap-2"
      action={(fd) => start(() => setCapacityAction(Number(fd.get("max"))))}
    >
      <Input name="max" type="number" min={1} defaultValue={initial} aria-label="Max in-flight requests" className="w-24" />
      <Button type="submit" size="sm" variant="secondary" disabled={pending}>
        {pending ? "..." : "Apply"}
      </Button>
    </form>
  );
}
