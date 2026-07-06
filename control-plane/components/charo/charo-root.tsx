"use client";

import dynamic from "next/dynamic";
import { useCallback, useState } from "react";
import { CharoLauncher } from "./launcher";
import { useCharoContext } from "./charo-context";

// Defer the panel out of the initial dashboard bundle; it hydrates on the
// client only.
const CharoPanel = dynamic(() => import("./charo-panel").then((m) => m.CharoPanel), {
  ssr: false,
});

export function CharoRoot() {
  const [open, setOpen] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const stream = useCharoContext();

  // Closing always returns to the collapsed dock so the next open is the small
  // window, not the modal.
  const close = useCallback(() => {
    setOpen(false);
    setExpanded(false);
  }, []);

  return (
    <>
      {!open && <CharoLauncher state={stream.state} onOpen={() => setOpen(true)} />}
      <CharoPanel
        open={open}
        expanded={expanded}
        onClose={close}
        onExpand={() => setExpanded(true)}
        onCollapse={() => setExpanded(false)}
        stream={stream}
        mascotState={stream.state}
      />
    </>
  );
}
