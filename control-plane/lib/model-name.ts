// Normalizes a free-text model name into obleth's API id rules: lowercase,
// spaces/underscores -> dashes, only [a-z0-9.-], collapsed dashes. The "draft"
// form is used while typing (keeps a trailing dash); the "final" form also trims
// leading/trailing separators. Single source of truth for both wizards.

export function normalizeModelApiNameDraft(value: string) {
  return value
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9.-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^\.+/g, "");
}

export function normalizeModelApiNameFinal(value: string) {
  return normalizeModelApiNameDraft(value).replace(/^[.-]+|[.-]+$/g, "");
}
