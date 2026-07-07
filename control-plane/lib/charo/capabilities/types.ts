import type { TraceSummary } from "@/lib/charo/trace";

export type TestId = "ping" | "tools" | "json" | "vision";
export type TestStatus = "pass" | "warn" | "fail";

export interface TestOutcome {
  id: TestId;
  label: string;
  status: TestStatus;
  detail: string;
  /** The model's reply (trimmed), for the expandable row. */
  output: string;
  trace: TraceSummary | null;
}

export interface CapabilityResult {
  modelName: string;
  tests: TestOutcome[];
}
