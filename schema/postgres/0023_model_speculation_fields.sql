-- Idempotent: safe to re-run.
-- Speculation config lives ON the target model (the model clients call):
--   draft_model           which small model writes its drafts ('' = the fleet
--                         default from boon settings)
--   verify_api_base       direct (non-gateway) URL of a deployment of this
--                         model whose backend supports prompt_logprobs; it
--                         scores every draft token. '' = this model cannot
--                         speculate.
--   verify_upstream_model the name that scoring backend serves, when it
--                         differs from this model's own upstream ('' = same).
-- This retires 0022's models.verifier_for (the arrow pointed the wrong way:
-- a scorer declared itself for a target). The column is left in place,
-- unread, so the migration chain stays append-only.
alter table models add column if not exists draft_model text not null default '';
alter table models add column if not exists verify_api_base text not null default '';
alter table models add column if not exists verify_upstream_model text not null default '';
