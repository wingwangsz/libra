# `libra logout`

Clear stored Libra host session tokens.

`libra logout` is a Libra-only extension (no Git equivalent). It removes the
`account.host.<host_sha256>` session blob from the global config and, unless
`--local-only` is given, first calls the host's `/api/cli/logout` to revoke
the server-side token — if that call fails the command errors and the local
token is left in place; use `--local-only` to drop the local token without
contacting the host. It is independent of the credentials used by `cloud` /
`publish`.

## Options

| Flag | Description |
|------|-------------|
| `--host <url>` | Target host (defaults to the built-in host) |
| `--all` | Remove stored tokens for every host |
| `--local-only` | Delete the local token without calling the host to revoke it |

## Examples

```bash
libra logout
libra logout --host https://libra.tools --local-only
libra logout --all
```
