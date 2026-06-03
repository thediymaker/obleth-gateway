"use client";

import { useState, useTransition } from "react";
import { setWeightAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function WeightControl({ id, initial }: { id: string; initial: number }) {
  const [value, setValue] = useState(initial);
  const [pending, start] = useTransition();
  const changed = value !== initial;

  return (
    <div className="flex items-center gap-2">
      <input
        type="range"
        min={1}
        max={1000}
        value={value}
        onChange={(e) => setValue(Number(e.target.value))}
        aria-label="Fairshare weight"
        className="h-1.5 w-28 cursor-pointer appearance-none rounded-full bg-muted accent-foreground"
      />
      <Input
        type="number"
        min={1}
        aria-label="Fairshare weight"
        className="w-20"
        value={value}
        onChange={(e) => setValue(Number(e.target.value))}
      />
      <Button size="sm" variant="secondary" disabled={pending || !changed} onClick={() => start(() => setWeightAction(id, value))}>
        {pending ? "..." : "Apply"}
      </Button>
    </div>
  );
}
