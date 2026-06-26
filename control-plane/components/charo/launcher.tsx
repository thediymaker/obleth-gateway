"use client";

import { OblethLogo } from "@/components/obleth-logo";
import { cn } from "@/lib/utils";
import type { CharoState } from "./sprite";

const STATUS_LABEL: Record<CharoState, string> = {
  idle: "Idle",
  held: "Ready",
  thinking: "Thinking",
  result: "Ready",
  error: "Needs attention",
};

function CharoLauncherMark({ state }: { state: CharoState }) {
  const active = state === "thinking";
  const bad = state === "error";
  const happy = state === "result";

  return (
    <span
      className="relative flex h-10 w-10 shrink-0 items-center justify-center rounded-full"
      aria-hidden
    >
      <span
        className={cn(
          "absolute inset-[-4px] rounded-full border opacity-0 transition-opacity duration-200 group-hover:opacity-80",
          active && "border-violet-300/70 opacity-70",
          happy && "border-emerald-300/70 opacity-70",
          bad && "border-destructive/70 opacity-75",
        )}
      />
      <OblethLogo size={30} className="relative drop-shadow-sm transition-transform duration-200 group-hover:scale-105" />
    </span>
  );
}

export function CharoLauncher({
  state,
  onOpen,
}: {
  state: CharoState;
  onOpen: () => void;
}) {
  const label = `Open Charo model tester. ${STATUS_LABEL[state]}.`;
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label={label}
      className={cn(
        "group fixed bottom-5 right-8 z-[55] flex h-14 w-14 items-center justify-center rounded-full border bg-card/95 p-0 shadow-2xl ring-1 ring-primary/5 backdrop-blur transition-[border-color,background-color,box-shadow,transform] duration-200 hover:-translate-y-0.5 hover:border-violet-300/55 hover:bg-accent hover:shadow-[0_0_34px_hsl(267_86%_70%/0.38)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-300/70",
        "border-border",
      )}
    >
      <CharoLauncherMark state={state} />
    </button>
  );
}
