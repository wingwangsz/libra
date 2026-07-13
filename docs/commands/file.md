# libra file

`libra file` groups object-level operations (a Libra extension, no Git
equivalent). Its one v1 subcommand is `obliterate` (lore.md 2.5).

## `file obliterate` — index-flagged payload obliteration

Implements the "保留 ADDRESS 删 PAYLOAD" compliance-deletion model (§19.6):
physically removes an object's PAYLOAD bytes while PRESERVING its address so
referencing history stays traversable. It is destructive and IRREVERSIBLE.

- Compatibility: `intentionally-different`.
- Synopsis: `libra file obliterate <oid> [--reason <text>] [--dry-run] [--yes]`
  or `libra file obliterate --recover`.

### Safety model

- `--dry-run` prints the blast-radius preview and deletes nothing; a real run
  REQUIRES `--yes` (`LBR-OBLITERATE-003` otherwise).
- v1 refuses a packed-only object (`LBR-OBLITERATE-002`) — no pack surgery
  (that is declined history-rewrite territory); loosen/repack first.
- Every run appends a durable, append-only, 0600 audit record to
  `.libra/obliteration-audit.jsonl` (§7.8) — OID (address), actor, approval
  source, reason, outcome; NEVER the erased content.

### State machine (crash-safe)

A tombstone row's ABSENCE means Live: `(no row)` → INSERT `obliterating`
(fsynced BEFORE any payload touch) → physical payload delete → UPDATE
`obliterated`. A crash can only leave `obliterating` with the payload maybe
still present — never "deleted but Live". `file obliterate --recover` (and an
opportunistic sweep at the start of every obliterate) re-runs the tail
idempotently.

### fsck / heal / restore integration

fsck reports an obliterated object as **intentionally absent** — a diagnostic
DISTINCT from `missing` that never flips the exit code — across the object,
tree, commit, parent, tag, and index seams. `fsck --heal` never resurrects it,
and cloud restore refuses to re-download it (拒绝重建).

## Examples

```bash
libra file obliterate <oid> --dry-run
libra file obliterate <oid> --reason "gdpr erasure" --yes
libra file obliterate --recover
```

## Deferred (not v1)

Byte-level in-object erasure (§3.5, declined); pack surgery / history rewrite
(declined); the §6.8 media/LFS chunk obliterate (waits on the media layer).
