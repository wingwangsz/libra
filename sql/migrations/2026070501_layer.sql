-- lore.md 2.4: Lore's `layer` local-overlay primitive. Two side-tables owned
-- SOLELY by `internal::layer::LayerStore` — NEVER serialized into any commit /
-- revision (a layer is a purely-local materialized overlay). `layer` holds the
-- registered overlays; `layer_path` records exactly which working-tree paths
-- each apply materialized (so unapply/remove/re-apply and the un-negatable
-- ignore-consult are precise).
CREATE TABLE IF NOT EXISTS `layer` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `name`       TEXT NOT NULL UNIQUE,
    `source`     TEXT NOT NULL,
    `priority`   INTEGER NOT NULL DEFAULT 0,
    `enabled`    INTEGER NOT NULL DEFAULT 1,
    `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS `layer_path` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `layer_name`      TEXT NOT NULL,
    `path`            TEXT NOT NULL UNIQUE,
    `content_hash`    TEXT NOT NULL,
    `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
