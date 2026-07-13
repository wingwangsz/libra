-- lore.md 2.1: per-worktree HEAD / index / HEAD-reflog isolation.
--
-- HEAD and the HEAD-reflog live in SQLite (not files), so a linked worktree
-- cannot get its own HEAD via a per-worktree file the way git does. Instead we
-- scope the relevant rows by a nullable `worktree_id`:
--   * main worktree  -> worktree_id IS NULL (existing rows unchanged; a
--     single-worktree repo is byte-identical to before this migration)
--   * linked worktree -> worktree_id = its stable instance id
-- Only kind='Head' (remote IS NULL) reference rows and ref_name='HEAD' reflog
-- rows are scoped; shared refs (Branch/Tag/remote) keep worktree_id NULL.
--
-- Forward DDL is idempotent-safe: the ALTERs run exactly once via the
-- schema_versions ledger (a second apply is prevented by the runner, not by
-- IF NOT EXISTS which ALTER ADD COLUMN does not support).

ALTER TABLE `reference` ADD COLUMN `worktree_id` TEXT;
ALTER TABLE `reflog` ADD COLUMN `worktree_id` TEXT;

-- Query index for the per-worktree HEAD lookup (kind='Head' AND remote IS NULL
-- AND worktree_id = ?). Not unique: the single-HEAD-per-worktree invariant is
-- maintained by the update path (find-then-update), and the existing
-- idx_name_kind already keeps HEAD rows distinct by branch name.
CREATE INDEX IF NOT EXISTS idx_reference_head_worktree
    ON `reference`(`kind`, `worktree_id`) WHERE `remote` IS NULL;

-- HEAD-reflog lookups are scoped by (ref_name, worktree_id).
CREATE INDEX IF NOT EXISTS idx_reflog_worktree
    ON `reflog`(`ref_name`, `worktree_id`, `timestamp`);
