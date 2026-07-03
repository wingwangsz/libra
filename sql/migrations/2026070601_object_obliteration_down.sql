-- Reverse 2026070601. NOTE: tombstones are compliance records; a down-migration
-- destroys them, so rollback is an emergency-only path (documented).
DROP INDEX IF EXISTS `idx_object_obliteration_oid`;
DROP TABLE IF EXISTS `object_obliteration`;
