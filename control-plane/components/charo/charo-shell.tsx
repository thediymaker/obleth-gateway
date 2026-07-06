"use client";
import { CharoProvider } from "./charo-context";
import { CharoRoot } from "./charo-root";
import { useCharoStream } from "./use-charo-stream";

export function CharoShell({ children }: { children: React.ReactNode }) {
  const stream = useCharoStream();
  return (
    <CharoProvider stream={stream}>
      {children}
      <CharoRoot />
    </CharoProvider>
  );
}
