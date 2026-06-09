export async function safe<T>(promise: Promise<T>, fallback: T): Promise<T> {
  try {
    return await promise;
  } catch (e) {
    // Swallow the error so the dashboard degrades gracefully, but log it so the
    // failure is still observable rather than silently hidden.
    console.error("safe(): falling back after error:", e);
    return fallback;
  }
}
