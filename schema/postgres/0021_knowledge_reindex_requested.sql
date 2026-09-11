-- Idempotent: safe to re-run.
--
-- Signals "a reindex was requested while this document was already
-- `indexing`" without making the row claimable a second time. Before this
-- column existed, that request was expressed by resetting `status` straight
-- to `pending`, which is exactly what let a second `claim_pending_document`
-- pick up a document a first worker was still actively embedding: two
-- workers deriving and committing the same `active_generation + 1`. The flag
-- lets the three request sites (an upsert of identical content, a single
-- document reindex, and a collection-wide reindex) leave `status` alone
-- while a document is `indexing` and instead ask
-- `commit_document_generation` to requeue (`pending`) rather than promote
-- (`ready`) once the in-flight run finishes -- consuming the flag in that
-- same transaction so the requeue happens exactly once, not forever.
alter table knowledge_documents
    add column if not exists reindex_requested boolean not null default false;
