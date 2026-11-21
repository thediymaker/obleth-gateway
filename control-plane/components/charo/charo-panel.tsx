"use client";

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
import type { useCharoStream } from "./use-charo-stream";

type Stream = ReturnType<typeof useCharoStream>;

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
 */
export function CharoPanel({ stream }: { stream: Stream }) {
  const { messages, busy, confirmRun, confirmCancel, startActivity, submitActivity, cancelActivity } = stream;

  return (
    <div className="w-full space-y-3">
      {messages.map((m) => {
        const content = m.content.replace(/^\s+/, "");

        if (m.workflowActivityId) {
          ensureActivitiesRegistered();
          const activity = getActivity(m.workflowActivityId);
          if (!activity) return null;
          return (
            <div key={m.id} className="w-full">
              <WorkflowCard
                activity={activity}
                onRun={(args) => submitActivity(m.id, activity.id, args)}
                onCancel={() => cancelActivity(m.id)}
              />
            </div>
          );
        }

        if (m.showLauncher) {
          return (
            <div key={m.id} className="w-full max-w-sm">
              <ActivityCards onPick={(a) => startActivity(a.id)} />
            </div>
          );
        }

        const hasLiveBench = (m.liveSteps?.length ?? 0) > 0 && (m.toolResults?.length ?? 0) === 0;
        const hasLiveCaps = (m.liveCapabilities?.length ?? 0) > 0 && (m.toolResults?.length ?? 0) === 0;
        const hasToolResults = (m.toolResults?.length ?? 0) > 0;
        const showAnswer = !!content || !!m.image || (m.images?.length ?? 0) > 0 || !!m.error;
        const hasReasoning = !!m.reasoning?.trim();
        if (!showAnswer && !hasReasoning && !hasLiveBench && !hasLiveCaps && !hasToolResults && !m.pendingConfirm) {
          // Streaming but nothing to show yet (waiting on the first token or a
          // tool's first progress event): show the typing indicator, not dead air.
          if (!m.streaming) return null;
          return (
            <div key={m.id} className="flex flex-col items-start">
              <div className={ANSWER_CLASS}>
                <TypingDots />
              </div>
            </div>
          );
        }
        // Children follow arrival order: tool and live cards land before the
        // answer tokens, so they render above the streaming text.
        return (
          <div key={m.id} className="flex flex-col items-start space-y-2">
            {hasLiveBench && (
              <div className="w-full">
                <BenchResultCard data={{ steps: m.liveSteps }} />
              </div>
            )}
            {hasLiveCaps && (
              <div className="w-full">
                <CapabilityResultCard data={{ modelName: undefined, tests: m.liveCapabilities }} />
              </div>
            )}
            {coalesceDocsResults(m.toolResults ?? []).map((tr, i) => {
              const Renderer = resultRenderer(tr.type);
              return <div key={i} className="w-full"><Renderer data={tr.data} /></div>;
            })}
            {hasReasoning && <ThinkingBlock text={m.reasoning!} active={!!m.streaming && !content} ms={m.reasoningMs} />}
            {showAnswer && (
              <div className={ANSWER_CLASS}>
                {m.image && (
                  /* eslint-disable-next-line @next/next/no-img-element */
                  <img src={m.image} alt="attachment" className="mb-1.5 max-h-40 rounded-md" />
                )}
                {m.images?.map((src, i) => (
                  /* eslint-disable-next-line @next/next/no-img-element */
                  <img
                    key={i}
                    src={src}
                    alt={`generated image ${i + 1}`}
                    className="my-1 max-h-80 rounded-md"
                  />
                ))}
                {content && <CharoMarkdown text={content} />}
                {m.error && <p className="mt-1 text-[13px] text-destructive">{m.error}</p>}
              </div>
            )}
            {!content && !m.error && m.streaming && !hasReasoning && (
              <div className={ANSWER_CLASS}>
                <TypingDots />
              </div>
            )}
            {m.pendingConfirm && (
              <div className="w-full">
                <ConfirmCard
                  pending={m.pendingConfirm}
                  disabled={busy}
                  onRun={() => confirmRun(m.id)}
                  onCancel={() => confirmCancel(m.id)}
                />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
