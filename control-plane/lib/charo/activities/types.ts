import type { LucideIcon } from "lucide-react";

/** A single step in an activity's guided workflow. */
export type StepSpec =
  | { type: "model"; label: string }
  | {
      type: "checklist";
      key: string;
      label: string;
      /** "boons" derives options from the picked model's boons; otherwise static. */
      optionsFrom: "boons" | { value: string; label: string; hint?: string }[];
    }
  | { type: "number"; key: string; label: string; default: number; min?: number; max?: number }
  | {
      type: "image";
      key: string;
      label: string;
      /** Shown (and required) only while this checklist key has this value selected. */
      onlyWhen?: { key: string; includes: string };
    };

/** Collected step values, keyed by step key (model step uses key "model"). */
export type StepValues = Record<string, unknown>;

export interface Activity {
  id: string;
  label: string;
  blurb: string;
  icon?: LucideIcon;
  /** "run" = one-shot tool execution + result card. "target" = sets an active chat target (Phase 3). */
  kind: "run" | "target";
  /** Backing CharoTool name for kind:"run". */
  toolName?: string;
  /** resultRenderer key for the result card. */
  resultType?: string;
  steps: StepSpec[];
}
