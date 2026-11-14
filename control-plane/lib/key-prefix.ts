/** The prefix new API keys start with, before the `_`: the gateway's rule. */
export const KEY_PREFIX_MAX = 3;

/** True when the gateway accepts `prefix`: 1 to 3 lowercase letters or digits, the first a letter. */
export function isValidKeyPrefix(prefix: string): boolean {
  return /^[a-z][a-z0-9]{0,2}$/.test(prefix);
}

/** Why `prefix` can't be used, or null when it can. */
export function keyPrefixProblem(prefix: string): string | null {
  if (prefix === "") return "Enter a prefix.";
  if (prefix.length > KEY_PREFIX_MAX) return `Use at most ${KEY_PREFIX_MAX} characters.`;
  if (!/^[a-z]/.test(prefix)) return "Start with a lowercase letter.";
  if (!isValidKeyPrefix(prefix)) return "Use only lowercase letters and digits.";
  return null;
}

/** How a key made with `prefix` begins, for the setting's preview. */
export function keyPrefixExample(prefix: string): string {
  return `${prefix}_3f9a07c2e1…`;
}
