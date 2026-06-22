"use client";

import { useState } from "react";

// Charo's art. Preferred path: real artwork — one transparent PNG per state in
// `public/charo/`. If a frame is missing/broken we fall back to the inline SVG
// `SvgSprite` below, so the companion always renders. The PNG is cropped from
// the reference sheet (one pose per state), with a subtle cursor lean to keep
// it feeling alive.

export type CharoState = "idle" | "held" | "thinking" | "result" | "error";

// Per-state frame. Drop matching files in control-plane/public/charo/.
const FRAME: Record<CharoState, string> = {
  idle: "/charo/idle.png",
  held: "/charo/held.png",
  thinking: "/charo/thinking.png",
  result: "/charo/result.png",
  error: "/charo/error.png",
};

// SVG-fallback palette — kept here so the drawn character restyles from one place.
const CLOAK = "#2b2833";
const CLOAK_DK = "#17151f";
const BONE = "#f1ede2";
const BONE_LINE = "#d6cfc0";
const EYE = "#b794f6";
const EYE_HI = "#ede9fe";
const EYE_HAPPY = "#c4b5fd";
const BAD = "#ff6b6b";

export function Sprite({
  state,
  lookAngle,
  size = 72,
}: {
  state: CharoState;
  lookAngle: number;
  size?: number;
}) {
  const [broken, setBroken] = useState<Record<string, boolean>>({});

  const wanted = FRAME[state];
  // If a specific pose is missing, reuse idle; if even idle is missing, draw SVG.
  const src = broken[wanted] ? FRAME.idle : wanted;
  if (broken[FRAME.idle]) {
    return <SvgSprite state={state} lookAngle={lookAngle} size={size} />;
  }

  // Subtle parallax: lean toward the cursor + per-state flourish.
  const lean = Math.cos(lookAngle) * 2;
  const transform =
    state === "held"
      ? "translateY(3px) scale(1.03, 0.95)"
      : `translateX(${lean.toFixed(1)}px)`;

  // The thinking/result/error poses are drawn facing left; mirror them so Charo
  // turns toward the chat panel (bottom-right) while responding.
  const faceChat =
    state === "thinking" || state === "result" || state === "error";

  const cls =
    "charo-sprite select-none drop-shadow-md " +
    (state === "idle" ? "charo-bob " : "") +
    (state === "error" ? "charo-shake" : "");

  return (
    <span className={cls} style={{ display: "inline-block", transform }}>
      <style>{`
        @keyframes charo-bob { 0%,100%{ transform: translateY(0) } 50%{ transform: translateY(-3px) } }
        .charo-bob { animation: charo-bob 2.8s ease-in-out infinite; }
        @keyframes charo-shake { 0%,100%{ transform: translateX(0) } 25%{ transform: translateX(-2px) } 75%{ transform: translateX(2px) } }
        .charo-shake { animation: charo-shake .35s ease-in-out infinite; }
      `}</style>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={src}
        alt=""
        width={size}
        height={size}
        draggable={false}
        onError={() => setBroken((b) => ({ ...b, [src]: true }))}
        style={{
          width: size,
          height: size,
          objectFit: "contain",
          pointerEvents: "none",
          imageRendering: "auto",
          transform: faceChat ? "scaleX(-1)" : undefined,
        }}
      />
    </span>
  );
}

// Dependency-free fallback character (hooded chibi reaper) drawn as inline SVG.
function SvgSprite({
  state,
  lookAngle,
  size = 72,
}: {
  state: CharoState;
  lookAngle: number;
  size?: number;
}) {
  const happy = state === "result";
  const bad = state === "error";
  const held = state === "held";
  const thinking = state === "thinking";

  // Highlight travel inside each glowing eye (px in the 100x100 viewBox).
  const r = 2.1;
  const hx = Math.cos(lookAngle) * r;
  const hy = thinking ? -2.2 : Math.sin(lookAngle) * r;

  const rootClass =
    "charo-sprite select-none drop-shadow-md " +
    (state === "idle" ? "charo-bob " : "") +
    (bad ? "charo-shake" : "");

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 100 100"
      className={rootClass}
      style={{ overflow: "visible" }}
      aria-hidden
    >
      <style>{`
        @keyframes charo-bob { 0%,100%{ transform: translateY(0) } 50%{ transform: translateY(-3px) } }
        .charo-bob { animation: charo-bob 2.8s ease-in-out infinite; transform-origin: 50% 100%; }
        @keyframes charo-shake { 0%,100%{ transform: translateX(0) } 25%{ transform: translateX(-2px) } 75%{ transform: translateX(2px) } }
        .charo-shake { animation: charo-shake .35s ease-in-out infinite; }
        @keyframes charo-think { 0%{ opacity:.2 } 50%{ opacity:1 } 100%{ opacity:.2 } }
        .charo-dot { animation: charo-think 1s ease-in-out infinite; }
        .charo-dot2 { animation-delay: .15s; }
        .charo-dot3 { animation-delay: .3s; }
        @keyframes charo-tw { 0%,100%{ transform: scale(.5); opacity:.3 } 50%{ transform: scale(1); opacity:1 } }
        .charo-tw { animation: charo-tw 1.1s ease-in-out infinite; transform-box: fill-box; transform-origin: center; }
        .charo-tw2 { animation-delay: .4s; }
      `}</style>

      <defs>
        <filter id="charo-glow" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="1.5" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>

      {/* squash slightly while held */}
      <g transform={held ? "translate(0,3) scale(1.02,0.95)" : undefined}>
        {/* little bone feet poking out from under the cloak */}
        <ellipse cx="42" cy="90" rx="5" ry="3.4" fill={BONE} stroke={BONE_LINE} strokeWidth="1.2" />
        <ellipse cx="58" cy="90" rx="5" ry="3.4" fill={BONE} stroke={BONE_LINE} strokeWidth="1.2" />

        {/* hooded cloak silhouette with a soft, slightly curled point */}
        <path
          d="M50 8 C 30 9 23 26 26 45 C 16 57 18 82 31 88 C 43 94 57 94 69 88 C 82 82 84 57 74 45 C 77 26 70 9 50 8 Z"
          fill={CLOAK}
          stroke={CLOAK_DK}
          strokeWidth="2.2"
        />
        {/* curled hood tip */}
        <path
          d="M50 9 C 55 3 64 4 64 11 C 64 16 58 17 54 14 Z"
          fill={CLOAK}
          stroke={CLOAK_DK}
          strokeWidth="2"
          strokeLinejoin="round"
        />

        {/* skull tucked inside the hood opening */}
        <path
          d="M50 26 C 36 26 31 36 33 47 C 34 55 41 61 50 61 C 59 61 66 55 67 47 C 69 36 64 26 50 26 Z"
          fill={BONE}
          stroke={BONE_LINE}
          strokeWidth="1.4"
        />

        {/* hood brow overhanging the skull (recessed-face shadow) */}
        <path
          d="M30 46 C 32 29 41 23 50 23 C 59 23 68 29 70 46 C 60 37 40 37 30 46 Z"
          fill={CLOAK}
        />

        {/* faint nasal cavity */}
        <path d="M50 50 l-2.2 3.6 h4.4 Z" fill={CLOAK_DK} opacity="0.45" />

        {/* eyes */}
        {bad ? (
          <>
            {/* worried brows + dim eyes */}
            <g stroke={CLOAK_DK} strokeWidth="2" strokeLinecap="round">
              <line x1="36" y1="40" x2="44" y2="43" />
              <line x1="64" y1="40" x2="56" y2="43" />
            </g>
            <ellipse cx="42" cy="48" rx="4.2" ry="5" fill={EYE} opacity="0.7" />
            <ellipse cx="58" cy="48" rx="4.2" ry="5" fill={EYE} opacity="0.7" />
          </>
        ) : happy ? (
          <>
            {/* happy eye-smiles */}
            <g
              fill="none"
              stroke={EYE_HAPPY}
              strokeWidth="3"
              strokeLinecap="round"
              filter="url(#charo-glow)"
            >
              <path d="M37 49 Q42 43 47 49" />
              <path d="M53 49 Q58 43 63 49" />
            </g>
          </>
        ) : (
          <>
            {/* glowing purple eyes with a tracking highlight */}
            <g filter="url(#charo-glow)">
              <ellipse cx="42" cy="47" rx="5.2" ry="6.4" fill={EYE} />
              <ellipse cx="58" cy="47" rx="5.2" ry="6.4" fill={EYE} />
              <circle cx={42 + hx} cy={47 + hy} r="1.7" fill={EYE_HI} />
              <circle cx={58 + hx} cy={47 + hy} r="1.7" fill={EYE_HI} />
            </g>
          </>
        )}

        {/* tiny content smile only when happy */}
        {happy && (
          <path
            d="M45 57 Q50 61 55 57"
            fill="none"
            stroke={BONE_LINE}
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        )}
      </g>

      {/* thinking: rising dots */}
      {thinking && (
        <g>
          <circle className="charo-dot" cx="76" cy="22" r="3" fill={EYE} />
          <circle className="charo-dot charo-dot2" cx="85" cy="17" r="3.4" fill={EYE} />
          <circle className="charo-dot charo-dot3" cx="95" cy="11" r="3.8" fill={EYE} />
        </g>
      )}

      {/* result: purple sparkles */}
      {happy && (
        <g fill={EYE_HAPPY}>
          <path className="charo-tw" d="M82 18 l1.6 4 4 1.6 -4 1.6 -1.6 4 -1.6 -4 -4 -1.6 4 -1.6 Z" />
          <path
            className="charo-tw charo-tw2"
            d="M20 26 l1.1 2.8 2.8 1.1 -2.8 1.1 -1.1 2.8 -1.1 -2.8 -2.8 -1.1 2.8 -1.1 Z"
          />
        </g>
      )}

      {/* error: a red alert badge */}
      {bad && (
        <g>
          <circle cx="80" cy="20" r="9" fill={BAD} />
          <rect x="78.6" y="14.5" width="2.8" height="7" rx="1.4" fill="#fff" />
          <circle cx="80" cy="24.5" r="1.6" fill="#fff" />
        </g>
      )}
    </svg>
  );
}
