-- Reverse lore.md 2.1 worktree-isolation scoping.
DROP INDEX IF EXISTS idx_reflog_worktree;
DROP INDEX IF EXISTS idx_reference_head_worktree;
ALTER TABLE `reflog` DROP COLUMN `worktree_id`;
ALTER TABLE `reference` DROP COLUMN `worktree_id`;
