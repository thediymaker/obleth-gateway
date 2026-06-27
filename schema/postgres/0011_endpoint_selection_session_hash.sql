-- Idempotent: safe to re-run.

-- Allow 'session_hash' as an endpoint-selection mode. The application layer
-- (obleth-config ENDPOINT_SELECTION_MODES) and the data plane (proxy
-- build_targets) have always supported it, and the dashboard offers it in the
-- Delivery panel, but the original CHECK constraint (0001) only permitted
-- 'failover' and 'load_balance'. Saving 'session_hash' therefore failed with a
-- constraint violation, crashing the reliability save (and taking the other
-- delivery fields in the same UPDATE down with it). Widen the constraint to
-- match the supported vocabulary.
alter table models drop constraint if exists models_endpoint_selection_mode_check;
alter table models add constraint models_endpoint_selection_mode_check
    check (endpoint_selection_mode in ('failover', 'load_balance', 'session_hash'));
