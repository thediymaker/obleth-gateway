"use client";

import dynamic from "next/dynamic";
import { useState } from "react";
import { useCharoStream } from "./use-charo-stream";

// Defer the companion (framer-motion) + panel out of the initial dashboard
// bundle; they hydrate on the client only.
const Companion = dynamic(() => import("./companion").then((m) => m.Companion), {
  ssr: false,
});
const CharoPanel = dynamic(() => import("./charo-panel").then((m) => m.CharoPanel), {
  ssr: false,
});

export function CharoRoot() {
  const [open, setOpen] = useState(false);
  const stream = useCharoStream();

  return (
    <>
      <Companion state={stream.state} onOpen={() => setOpen(true)} />
      <CharoPanel open={open} onClose={() => setOpen(false)} stream={stream} />
    </>
  );
}
