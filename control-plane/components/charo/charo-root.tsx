"use client";

import dynamic from "next/dynamic";
import { useState } from "react";
import { CharoLauncher } from "./launcher";
import { useCharoStream } from "./use-charo-stream";

// Defer the panel out of the initial dashboard bundle; it hydrates on the
// client only.
const CharoPanel = dynamic(() => import("./charo-panel").then((m) => m.CharoPanel), {
  ssr: false,
});

export function CharoRoot() {
  const [open, setOpen] = useState(false);
  const stream = useCharoStream();

  return (
    <>
      {!open && (
        <CharoLauncher
          state={stream.state}
          onOpen={() => setOpen(true)}
        />
      )}
      <CharoPanel
        open={open}
        onClose={() => setOpen(false)}
        stream={stream}
        mascotState={stream.state}
      />
    </>
  );
}
