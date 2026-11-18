"use client";

import { useState } from "react";
import { TextField } from "@/components/models/fields";
import { countryCodes, countryName } from "@/lib/key-networks";

/**
 * A key's allowed countries: two-letter codes separated by commas or
 * spaces. Each code is checked as it is typed, with the country's name
 * beside it, so a typo shows before the gateway refuses it.
 */
export function AllowedCountriesField({ defaultValue = "" }: { defaultValue?: string }) {
  const [text, setText] = useState(defaultValue);
  const codes = countryCodes(text);
  return (
    <>
      <TextField
        name="allowed_countries"
        label="Allowed countries"
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="US, CA"
        autoComplete="off"
        spellCheck={false}
        mono
      />
      {codes.length > 0 && (
        <ul className="flex flex-wrap items-center gap-1.5" aria-label="Countries">
          {codes.map((code) => {
            const name = countryName(code);
            return (
              <li
                key={code}
                className="inline-flex h-6 items-center gap-1.5 rounded-full border border-border px-2 text-[11.5px] text-secondary-foreground"
              >
                <span className="font-mono text-foreground">{code}</span>
                <span className={name ? undefined : "text-muted-foreground"}>{name ?? "Not a country code"}</span>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}
