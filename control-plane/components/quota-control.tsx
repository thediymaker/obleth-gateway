"use client";

import { useState, useTransition } from "react";
import { setQuotaAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function QuotaControl({
  id,
  tokensPerMinute,
  maxInFlight,
}: {
  id: string;
  tokensPerMinute: number;
  maxInFlight: number | null;
}) {
  const [tokens, setTokens] = useState(tokensPerMinute);
  const [max, setMax] = useState(maxInFlight?.toString() ?? "");
  const [pending, start] = useTransition();

  const changed = tokens !== tokensPerMinute || max !== (maxInFlight?.toString() ?? "");
  const canSubmit = Number.isFinite(tokens) && tokens > 0 && (max === "" || Number(max) > 0);

  return (
    <form
      action={(fd) => start(() => setQuotaAction(fd))}
      className="flex min-w-[18rem] flex-wrap items-center gap-2"
    >
      <input type="hidden" name="id" value={id} />
      <Input
        name="tokens_per_minute"
        type="number"
        min={1}
        value={tokens}
        onChange={(e) => setTokens(Number(e.target.value))}
        aria-label="Tokens per minute"
        className="w-28"
      />
      <Input
        name="max_in_flight"
        type="number"
        min={1}
        value={max}
        onChange={(e) => setMax(e.target.value)}
        placeholder="No cap"
        aria-label="Max in-flight requests"
        className="w-24"
      />
      <Button type="submit" size="sm" variant="secondary" disabled={pending || !changed || !canSubmit}>
        {pending ? "..." : "Apply"}
      </Button>
    </form>
  );
}
