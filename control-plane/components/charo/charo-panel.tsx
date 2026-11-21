"use client";

import { memo, useMemo } from "react";
import { resultRenderer } from "./results/registry";
import { BenchResultCard } from "./results/bench-result-card";
import { CapabilityResultCard } from "./results/capability-result-card";
import { ConfirmCard } from "./results/confirm-card";
import { WorkflowCard } from "./workflow-card";
import { ActivityCards } from "./activity-launcher";
import { ensureActivitiesRegistered, getActivity } from "@/lib/charo/activities";
import { coalesceDocsResults } from "@/lib/charo/docs/coalesce-docs-results";
import { CharoMarkdown } from "./markdown";
import { ThinkingBlock } from "./thinking-block";
import { useLatestCallback } from "@/lib/use-latest-callback";
import type { ChatTurn, useCharoStream } from "./use-charo-stream";

/** What a turn's cards can do: run or cancel a confirmation, start or answer an activity. */
type TurnActions = Pick<ReturnType<typeof useCharoStream>, "confirmRun" | "confirmCancel" | "startActivity" | "submitActivity" | "cancelActivity">;

const ANSWER_CLASS = "min-w-0 w-full max-w-full text-[14.5px] leading-relaxed";

// Three pulsing dots shown in the answer's place between hitting send and the
// first token, so a pending response never reads as dead air.
function TypingDots() {
  return (
    <span className="flex h-5 items-center gap-1" role="status" aria-label="The assistant is thinking">
      {[0, 160, 320].map((d) => (
        <span
          key={d}
          className="h-1.5 w-1.5 animate-pulse rounded-full bg-muted-foreground/70"
          style={{ animationDelay: `${d}ms` }}
        />
      ))}
    </span>
  );
}

/**
 * One lane's answer turns in the Playground: streamed text and thinking,
 * generated images, tool and activity cards, and confirmation prompts. The
 * host frames the answer and shows its own metrics.
 *
 * Each turn renders on its own and only when it changes: the stream replaces
 * just the turn a token lands in, so earlier turns keep their parsed markdown.
 */
export function CharoPanel({ turns, busy, actions }: { turns: ChatTurn[]; busy: boolean; actions: TurnActions }) {
  const confirmRun = useLatestCallback(actions.confirmRun);
  const confirmCancel = useLatestCallback(actions.confirmCancel);
  const startActivity = useLatestCallback(actions.startActivity);
  const submitActivity = useLatestCallback(actions.submitActivity);
  const cancelActivity = useLatestCallback(actions.cancelActivity);
  const stable = useMemo<TurnActions>(
    () => ({ confirmRun, confirmCancel, startActivity, submitActivity, cancelActivity }),
    [confirmRun, confirmCancel, startActivity, submitActivity, cancelActivity],
  );
  return (
    <div className="w-full space-y-3">
      {/* Only a turn waiting on a confirmation reads busy, so the stream starting or ending re-renders no other turn. */}
      {turns.map((turn) => <Turn key={turn.id} turn={turn} busy={!!turn.pendingConfirm && busy} actions={stable} />)}
    </div>
  );
}

const Turn = memo(function Turn({ turn, busy, actions }: { turn: ChatTurn; busy: boolean; actions: TurnActions }) {
  const content = turn.content.replace(/^\s+/, "");

  if (turn.workflowActivityId) {
    ensureActivitiesRegistered();
    const activity = getActivity(turn.workflowActivityId);
    if (!activity) return null;
    return (
      <div className="w-full">
        <WorkflowCard
          activity={activity}
          onRun={(args) => actions.submitActivity(turn.id, activity.id, args)}
          onCancel={() => actions.cancelActivity(turn.id)}
        />
      </div>
    );
  }

  if (turn.showLauncher) {
    return (
      <div className="w-full max-w-sm">
        <ActivityCards onPick={(a) => actions.startActivity(a.id)} />
      </div>
    );
  }

  const hasLiveBench = (turn.liveSteps?.length ?? 0) > 0 && (turn.toolResults?.length ?? 0) === 0;
  const hasLiveCaps = (turn.liveCapabilities?.length ?? 0) > 0 && (turn.toolResults?.length ?? 0) === 0;
  const hasToolResults = (turn.toolResults?.length ?? 0) > 0;
  const showAnswer = !!content || !!turn.image || (turn.images?.length ?? 0) > 0 || !!turn.error;
  const hasReasoning = !!turn.reasoning?.trim();
  if (!showAnswer && !hasReasoning && !hasLiveBench && !hasLiveCaps && !hasToolResults && !turn.pendingConfirm) {
    // Streaming but nothing to show yet (waiting on the first token or a
    // tool's first progress event): show the typing indicator, not dead air.
    if (!turn.streaming) return null;
    return (
      <div className="flex flex-col items-start">
        <div className={ANSWER_CLASS}>
          <TypingDots />
        </div>
      </div>
    );
  }
  // Children follow arrival order: tool and live cards land before the
  // answer tokens, so they render above the streaming text.
  return (
    <div className="flex flex-col items-start space-y-2">
      {hasLiveBench && (
        <div className="w-full">
          <BenchResultCard data={{ steps: turn.liveSteps }} />
        </div>
      )}
      {hasLiveCaps && (
        <div className="w-full">
          <CapabilityResultCard data={{ modelName: undefined, tests: turn.liveCapabilities }} />
        </div>
      )}
      {coalesceDocsResults(turn.toolResults ?? []).map((tr, i) => {
        const Renderer = resultRenderer(tr.type);
        return <div key={i} className="w-full"><Renderer data={tr.data} /></div>;
      })}
      {hasReasoning && <ThinkingBlock text={turn.reasoning!} active={!!turn.streaming && !content} ms={turn.reasoningMs} />}
      {showAnswer && (
        <div className={ANSWER_CLASS}>
          {turn.image && (
            /* eslint-disable-next-line @next/next/no-img-element */
            <img src={turn.image} alt="attachment" className="mb-1.5 max-h-40 rounded-md" />
          )}
          {turn.images?.map((src, i) => (
            /* eslint-disable-next-line @next/next/no-img-element */
            <img
              key={i}
              src={src}
              alt={`generated image ${i + 1}`}
              className="my-1 max-h-80 rounded-md"
            />
          ))}
          {content && <CharoMarkdown text={content} />}
          {turn.error && <p className="mt-1 text-[13px] text-destructive">{turn.error}</p>}
        </div>
      )}
      {!content && !turn.error && turn.streaming && !hasReasoning && (
        <div className={ANSWER_CLASS}>
          <TypingDots />
        </div>
      )}
      {turn.pendingConfirm && (
        <div className="w-full">
          <ConfirmCard
            pending={turn.pendingConfirm}
            disabled={busy}
            onRun={() => actions.confirmRun(turn.id)}
            onCancel={() => actions.confirmCancel(turn.id)}
          />
        </div>
      )}
    </div>
  );
});
