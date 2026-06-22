"use client";

import { useEffect, useRef, useState } from "react";
import { motion, useAnimationControls, useMotionValue } from "framer-motion";
import { Sprite, type CharoState } from "./sprite";

const SIZE = 100;
const MARGIN = 16;
const STORAGE_KEY = "charo-pos-x";

// Charo: a draggable companion. framer-motion handles the gesture; on release we
// spring him back down to the "floor" (gravity). His eyes follow the cursor.
export function Companion({
  state,
  onOpen,
}: {
  state: CharoState;
  onOpen: () => void;
}) {
  const constraintsRef = useRef<HTMLDivElement>(null);
  const x = useMotionValue(MARGIN);
  const y = useMotionValue(0);
  const controls = useAnimationControls();
  const draggedRef = useRef(false);
  const lastLook = useRef(0);
  const [look, setLook] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [ready, setReady] = useState(false);

  const floorY = () => window.innerHeight - SIZE - MARGIN;

  // Initial placement (bottom-left, or last horizontal position) + resize clamp.
  useEffect(() => {
    const saved = Number(localStorage.getItem(STORAGE_KEY));
    const maxX = window.innerWidth - SIZE - MARGIN;
    x.set(Number.isFinite(saved) && saved > 0 ? Math.min(saved, maxX) : MARGIN);
    y.set(floorY());
    setReady(true);
    const onResize = () => {
      x.set(Math.min(x.get(), window.innerWidth - SIZE - MARGIN));
      y.set(Math.min(y.get(), floorY()));
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Cursor tracking for the eyes (rAF; throttled; paused when tab hidden).
  useEffect(() => {
    let raf = 0;
    let mx = 0;
    let my = 0;
    let seen = false;
    const onMove = (e: PointerEvent) => {
      mx = e.clientX;
      my = e.clientY;
      seen = true;
    };
    window.addEventListener("pointermove", onMove);
    const tick = () => {
      if (seen && !document.hidden) {
        const cx = x.get() + SIZE / 2;
        const cy = y.get() + SIZE / 2;
        const a = Math.atan2(my - cy, mx - cx);
        if (Math.abs(a - lastLook.current) > 0.04) {
          lastLook.current = a;
          setLook(a);
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("pointermove", onMove);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      ref={constraintsRef}
      style={{
        position: "fixed",
        inset: 0,
        pointerEvents: "none",
        zIndex: 50,
      }}
    >
      <motion.div
        drag
        dragConstraints={constraintsRef}
        dragElastic={0.04}
        dragMomentum={false}
        animate={controls}
        whileDrag={{ scale: 1.06 }}
        onDragStart={() => {
          draggedRef.current = true;
          setDragging(true);
        }}
        onDragEnd={() => {
          localStorage.setItem(STORAGE_KEY, String(Math.round(x.get())));
          setDragging(false);
          // Gravity: fall back to the floor.
          controls.start({
            y: floorY(),
            transition: { type: "spring", stiffness: 240, damping: 18 },
          });
          // Let the tap handler see the drag, then reset.
          window.setTimeout(() => {
            draggedRef.current = false;
          }, 0);
        }}
        onTap={() => {
          if (!draggedRef.current) onOpen();
        }}
        style={{
          x,
          y,
          position: "absolute",
          left: 0,
          top: 0,
          width: SIZE,
          height: SIZE,
          cursor: dragging ? "grabbing" : "grab",
          pointerEvents: "auto",
          touchAction: "none",
          opacity: ready ? 1 : 0,
        }}
        role="button"
        aria-label="Open Charo, the model tester"
        title="Charo — click to test a model"
      >
        <Sprite state={dragging ? "held" : state} lookAngle={look} size={SIZE} />
      </motion.div>
    </div>
  );
}
