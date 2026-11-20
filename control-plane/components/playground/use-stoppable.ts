import { useEffect, useMemo, useRef } from "react";

/**
 * A workspace's run in flight, cancelled by Stop or by leaving the workspace
 * (switching sessions remounts it), so the gateway call behind a run ends
 * with the page that asked for it.
 */
export function useStoppable() {
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => active.current?.abort(), []);
  return useMemo(
    () => ({
      /** A signal for a new run; a run still going is cancelled first. */
      start(): AbortSignal {
        active.current?.abort();
        active.current = new AbortController();
        return active.current.signal;
      },
      stop() {
        active.current?.abort();
        active.current = null;
      },
    }),
    [],
  );
}
