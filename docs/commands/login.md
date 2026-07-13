# `libra login`

Authenticate to a Libra host and store a host-scoped session token.

`libra login` is a Libra-only extension (no Git equivalent). It performs a
browser-redirect (PKCE-style) flow against the host's `/api/cli/*`
endpoints and stores the resulting session token under
`account.host.<host_sha256>` in the global config. It is independent of the
D1/R2/Cloudflare credentials used by `cloud` / `publish`.

It always binds a same-machine loopback listener and waits for the browser
redirect callback, so it requires a session on the local machine
(remote/SSH sessions are unsupported).

## Options

| Flag | Description |
|------|-------------|
| `--host <url>` | Target host (defaults to the built-in host) |
| `--no-browser` | Print the login URL to open manually instead of launching a browser; the loopback callback is still required |

The flow times out after 15 minutes.

## Examples

```bash
libra login
libra login --host https://libra.tools --no-browser
```
