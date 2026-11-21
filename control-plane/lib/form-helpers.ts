import { z } from "zod";

/**
 * Reading numbers and text out of form and query values: one place for what
 * "blank" and "a number" mean, shared by the server actions and the live
 * routes. Not a server-action module, so it can export constants.
 */

/** A form value as trimmed text ("" when absent). */
export const trimmed = (v: unknown) => (v == null ? "" : String(v)).trim();

/** Blank (absent, empty or whitespace) as `undefined`, so zod `.default()` and `.optional()` apply. */
export const blankToUndef = (v: unknown) => {
  const s = trimmed(v);
  return s === "" ? undefined : s;
};

/**
 * A form value as a number, `undefined` when blank. Text that is not a
 * number reads as `NaN`, so a caller that checks the range can say so
 * instead of quietly using its default.
 */
export function formNumber(v: unknown): number | undefined {
  const s = trimmed(v);
  return s === "" ? undefined : Number(s);
}

/** A form or query value as a finite number; blank or not a finite number is `undefined`. */
export function finiteNumber(v: unknown): number | undefined {
  const n = formNumber(v);
  return n !== undefined && Number.isFinite(n) ? n : undefined;
}

/** {@link finiteNumber}, with `fallback` for blank or not a number. */
export function numOr(v: unknown, fallback: number): number {
  return finiteNumber(v) ?? fallback;
}

/** {@link finiteNumber}, with `null` for blank or not a number (a nullable field cleared). */
export function numOrNull(v: unknown): number | null {
  return finiteNumber(v) ?? null;
}

export const requiredText = (message: string) => z.preprocess(trimmed, z.string().min(1, message));
export const optionalText = z.preprocess(trimmed, z.string());
export const checkbox = z.preprocess((v) => v === "on", z.boolean());

/** Optional positive integer (absent when the field is blank). */
export const optionalPositiveInt = z.preprocess(blankToUndef, z.coerce.number().int().positive().optional());
/** Optional non-negative integer. */
export const optionalNonNegInt = z.preprocess(blankToUndef, z.coerce.number().int().nonnegative().optional());
/** Optional non-negative number. */
export const optionalNonNegNumber = z.preprocess(blankToUndef, z.coerce.number().nonnegative().optional());
/** Non-negative number with a default applied when the field is blank. */
export const nonNegNumber = (def: number) => z.preprocess(blankToUndef, z.coerce.number().nonnegative().default(def));
/** Positive integer with a default applied when blank. */
export const positiveIntWithDefault = (def: number) => z.preprocess(blankToUndef, z.coerce.number().int().positive().default(def));
