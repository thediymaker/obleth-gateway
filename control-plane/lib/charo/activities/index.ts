import { registerActivity, __clearActivities } from "./registry";
import { testCapabilitiesActivity } from "./test-capabilities";
import { chatWithModelActivity } from "./chat-with-model";
import { benchmarkActivity } from "./benchmark";

let done = false;
export function ensureActivitiesRegistered(): void {
  if (done) return;
  done = true;
  registerActivity(testCapabilitiesActivity);
  registerActivity(chatWithModelActivity);
  registerActivity(benchmarkActivity);
}

/** Test-only: allow re-running registration after __clearActivities(). */
export function __resetActivitiesBootstrap(): void {
  done = false;
}

// Re-exported for convenience so UI imports have one entry point.
export { listActivities, getActivity } from "./registry";
export { __clearActivities };
