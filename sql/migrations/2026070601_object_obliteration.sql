-- lore.md 2.5: index-flagged obliteration ("保留 ADDRESS 删 PAYLOAD", §19.6).
-- One OID-keyed side-table owned SOLELY by internal::obliteration::
-- ObliterationStore. A row IS the intentional-absence TOMBSTONE (permanently
-- retained, excluded from all cleanup): its ABSENCE means Live, so the state
-- machine is (no row)=Live -> insert 'obliterating' (tombstone, written BEFORE
-- any payload delete) -> physical payload delete -> UPDATE 'obliterated'.
CREATE TABLE IF NOT EXISTS `object_obliteration` (
    `id`                   INTEGER PRIMARY KEY AUTOINCREMENT,
    `oid`                  TEXT NOT NULL,
    `hash_kind`            TEXT NOT NULL,
    `state`                TEXT NOT NULL CHECK (`state` IN ('obliterating', 'obliterated')),
    `reason`               TEXT,
    `actor`                TEXT,
    `approval_source`      TEXT,
    `requested_at`         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `tombstone_written_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `payload_deleted_at`   TEXT,
    `updated_at`           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (`oid`, `hash_kind`)
);

CREATE INDEX IF NOT EXISTS `idx_object_obliteration_oid` ON `object_obliteration` (`oid`);
