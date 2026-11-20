import { useCallback, useLayoutEffect, useRef } from "react";

/**
 * A callback whose identity never changes but which always calls the latest
 * `fn`, so a memoised row is not re-rendered by its parent's fresh closure.
 * Call it from event handlers, not during render.
 */
export function useLatestCallback<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  const latest = useRef(fn);
  useLayoutEffect(() => {
    latest.current = fn;
  });
  return useCallback((...args: A) => latest.current(...args), []);
}
