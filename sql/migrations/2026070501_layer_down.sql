-- Reverse 2026070501: drop the layer overlay side-tables. Local-only overlay
-- registrations are lost on rollback (no commit ever depended on them).
DROP TABLE IF EXISTS `layer_path`;
DROP TABLE IF EXISTS `layer`;
