-- Rollback for 2026070802_agent_checkpoint_paging: drop the probe +
-- pagination indexes. Data is untouched — these are pure secondary indexes.

DROP INDEX IF EXISTS `idx_agent_checkpoint_traces_commit`;
DROP INDEX IF EXISTS `idx_agent_session_started_paging`;
DROP INDEX IF EXISTS `idx_agent_checkpoint_created_paging`;
