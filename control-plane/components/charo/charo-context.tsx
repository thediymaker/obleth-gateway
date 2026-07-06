"use client";
import { createContext, useContext } from "react";
import { useCharoStream } from "./use-charo-stream";

type Stream = ReturnType<typeof useCharoStream>;
const Ctx = createContext<Stream | null>(null);

export function CharoProvider({ children, stream }: { children: React.ReactNode; stream: Stream }) {
  return <Ctx.Provider value={stream}>{children}</Ctx.Provider>;
}

export function useCharoContext(): Stream {
  const s = useContext(Ctx);
  if (!s) throw new Error("useCharoContext must be used within CharoProvider");
  return s;
}
