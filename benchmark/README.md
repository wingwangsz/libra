# Libra CLI benchmark

This directory provides a reproducible, end-to-end performance benchmark for
the Libra CLI. It adopts the useful experimental boundaries from
[version-control-bench](https://github.com/gitbutlerapp/version-control-bench):
fixtures are built before timing begins, each scenario begins from a known
state, runs are repeated, and the result is machine-readable. It is **not** an
agent benchmark and it does not reuse or report the upstream project's claims.

## Run

From the repository root:

```bash
benchmark/run.sh
```

The default run resolves the current `HEAD` with `libra rev-parse`, exports that
exact source tree through `libra archive --format zip`, builds it in a temporary directory
with `LIBRA_SKIP_WEB_BUILD=1 cargo build --release --locked`, creates fixtures using that binary, and
writes `benchmark/results/<revision>-<timestamp>.json`. The build and fixture
setup times are recorded or excluded; only a fresh CLI process for the requested
operation is timed.

The runner requires `bash`, `cargo`, `libra`, `perl` (for a monotonic-enough
wall-clock sample), `unzip`, and `/usr/bin/time`. It never switches the current
worktree or writes to the repository's Cargo target directory.

The `libra` used to resolve and archive a commit must understand the current
repository schema. The runner first tries `libra` from `PATH`; if it cannot
resolve the revision, it builds a temporary control binary from the current
worktree and records that cost separately as `control_build_elapsed_ms`. Set
`--control-binary /path/to/libra` (or `LIBRA_BENCHMARK_CONTROL_BINARY`) when a
compatible binary is already available.

Benchmark an older local commit or tag without switching the current worktree:

```bash
benchmark/run.sh --revision a0567ce --runs 20 --warmup 5
benchmark/run.sh --revision v0.18.84 --scenario log_history
```

`--revision` must resolve in the local Libra object store. The selected revision
must support the fixture commands and the scenarios it is asked to run; this is
intentional, because a missing command is a compatibility failure rather than a
performance result.

For a quick smoke run or script development, lower the fixture scale and sample
count. `--binary` intentionally bypasses the isolated source build and is only
for this use case.

```bash
benchmark/run.sh --binary "$(command -v libra)" --scenario status_clean \
  --file-count 100 --runs 1 --warmup 0
```

## Scenarios and measurement contract

| Scenario | Fixture | Timed command |
| --- | --- | --- |
| `status_clean` | committed tree with `--file-count` files | `libra status --short` |
| `status_dirty` | same shape, with ten tracked files changed | `libra status --short` |
| `log_history` | linear history with `--history-count` commits | `libra log --oneline` |
| `rev_list_refs` | one commit with `--ref-count` branches | `libra rev-list --all --count` |
| `fsck_history` | the linear history fixture | `libra fsck` |

The defaults deliberately line up with the existing repository performance
budgets: 1,000 commits for history operations and 10,000 refs for `rev-list`.
Every recorded sample starts a new CLI process and runs in the same prepared
fixture. The runner reports min/median/mean/max wall time plus maximum RSS for
each scenario. It uses `/usr/bin/time -l` on macOS and `/usr/bin/time -v` on
Linux; memory is normalized to bytes.

Results contain the exact resolved revision, build mode, fixture scale, platform,
sample counts, and per-scenario aggregates. Generated JSON files are ignored so
that comparisons can be attached to a PR or stored by CI without accidentally
committing local measurements.

## Verification

```bash
benchmark/test/test-runner.sh
```

This is an end-to-end contract test: it verifies CLI options and confirms that a
pair of real scenarios writes the documented JSON shape. By default it follows
the revision-build path, so it is intentionally as expensive as a release build.
After building a compatible local binary, use
`LIBRA_BENCHMARK_TEST_BINARY=/path/to/libra benchmark/test/test-runner.sh` for a
fast fixture-and-result check.
