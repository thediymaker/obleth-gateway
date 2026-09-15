-- Idempotent: safe to re-run.
-- Names the model this deployment can score drafts for (speculation boon).
-- A scoring deployment is registered with a direct (non-gateway) URL whose
-- backend supports prompt_logprobs; a model may name itself. Empty means this
-- deployment cannot score drafts, which is the right default for everything
-- reached through a gateway that does not carry /tokenize.
alter table models add column if not exists verifier_for text not null default '';
