-- 2026070802_agent_checkpoint_paging: AG-20 idempotent-catalog probe index +
-- keyset pagination indexes for `agent session list` / `agent checkpoint list`.
--
-- (1) `traces_commit` lookup index. DELIBERATELY NON-UNIQUE: writer-side
--     idempotency is enforced in code (probe-by-traces_commit before INSERT
--     plus an `ON CONFLICT(checkpoint_id) DO NOTHING` backstop in
--     `src/internal/ai/hooks/runtime.rs::write_committed_checkpoint`), so
--     the index only needs to make the probe O(log n). A UNIQUE index would
--     hard-fail this migration on any legacy database that already carries
--     duplicate `traces_commit` rows (theoretically possible from pre-AG-20
--     crash windows), and because migrations run automatically on connect,
--     that failure would brick the repository. Availability wins.
--
-- (2) Keyset pagination for `agent session list`: newest-first ordering with
--     `session_id` as the unique tiebreaker (`docs/development/tracing/
--     agent.md` §8 performance row).
--
-- (3) Keyset pagination for `agent checkpoint list`: same shape on
--     `agent_checkpoint`.
--
-- Idempotency: `IF NOT EXISTS` on every statement, safe to re-apply.

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_traces_commit`
    ON `agent_checkpoint`(`traces_commit`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_started_paging`
    ON `agent_session`(`started_at` DESC, `session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_created_paging`
    ON `agent_checkpoint`(`created_at` DESC, `checkpoint_id`);
