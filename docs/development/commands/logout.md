# `libra logout`

**Compatibility:** `intentionally-different` — Libra host-scoped HTTP auth extension, not a Git command.

## Summary

`libra logout` manages Libra host-scoped HTTP session tokens (the
`internal::account` surface authenticated via `/api/cli/*`). It is a
Libra-only extension with no Git equivalent, and is independent of the
D1/R2/Cloudflare credentials used by `cloud`/`publish`.

Clears stored session tokens. Flags: `--host <host>`, `--all` (every host), `--local-only` (drop the local token without notifying the host).

## Examples

```bash
libra logout --host libra.tools
```
