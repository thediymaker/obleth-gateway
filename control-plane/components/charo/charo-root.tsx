"use client";

import dynamic from "next/dynamic";
import { useState } from "react";
import { useRouter } from "next/navigation";
import { CharoLauncher } from "./launcher";
import { useCharoContext } from "./charo-context";

// Defer the panel out of the initial dashboard bundle; it hydrates on the
// client only.
const CharoPanel = dynamic(() => import("./charo-panel").then((m) => m.CharoPanel), {
  ssr: false,
});

export function CharoRoot() {
  const [open, setOpen] = useState(false);
  const router = useRouter();
  const stream = useCharoContext();

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
        onExpand={() => {
          setOpen(false);
          router.push("/charo");
        }}
        stream={stream}
        mascotState={stream.state}
      />
    </>
  );
}
