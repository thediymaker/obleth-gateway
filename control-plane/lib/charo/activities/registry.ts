import type { Activity } from "./types";

const registry = new Map<string, Activity>();

export function registerActivity(a: Activity): void {
  registry.set(a.id, a);
}

/** Test-only: reset between cases. */
export function __clearActivities(): void {
  registry.clear();
}

export function getActivity(id: string): Activity | undefined {
  return registry.get(id);
}

export function listActivities(): Activity[] {
  return [...registry.values()];
}
