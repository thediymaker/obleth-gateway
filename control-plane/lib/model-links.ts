/** A model's own page, optionally at one of its sections (`#capacity`). */
export function modelHref(name: string, section?: string): string {
  return `/models/${encodeURIComponent(name)}${section ? `#${section}` : ""}`;
}
