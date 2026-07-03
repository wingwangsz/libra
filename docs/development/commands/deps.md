# libra deps

`libra deps` manages the **file dependency graph** (lore.md 3.1): typed,
versioned per-file dependency edges. A Libra extension — Git has no
file-dependency concept.

## Compatibility

- Level: `intentionally-different`.

## Design

An edge `(from -> to, kind)` declares that one file depends on another. Edges
are VERSIONED per-commit: the authoritative store is one adjacency document
per commit under the reserved notes ref `refs/notes/deps`, owned solely by
`internal::deps::DependencyStore` (mirroring the `refs/notes/metadata`
pattern — no new SQLite table, honoring the §3.6 "no per-kind table" rule).
Every query loads the revision's (size-bounded) document and computes in
memory, so there is no projection cache to fall out of sync.

Queries are cycle-safe (iterative BFS with a visited set — deep/wide graphs
never overflow the stack) and absence-tolerant (a missing note → an empty
graph). Paths are repo-relative and normalized (`./` stripped, `\`→`/`,
trailing `/` collapsed); absolute paths, `..` escapes, and empty strings are
rejected.

The `transitive_closure` API is the reusable seam that 3.2 (dependency-filtered
clone/sync) and 3.3 (hydrating VFS) call to expand a root file set.

## Wire travel (lore.md 3.2 — local side-channel)

A Libra deps note is a loose blob (the JSON adjacency doc) plus a row in the
SQLite `notes` table; `refs/notes/deps` is not a real reference-table ref, so it
cannot ride the pack/ref want set. lore.md 3.2 travels edges over a dedicated
local-protocol side-channel: `libra fetch --notes` / `libra pull --notes` imports
`refs/notes/deps` from a **local Libra source** (union-merging into any local
edges and re-validating every endpoint), default OFF (Git parity). Persist the
opt-in with `remote.<name>.fetchNotesDeps=true`; `libra clone --deps-of` implies
it. Network / foreign-Git / push-side travel is deferred (D17), so a fresh clone
still reads an empty graph until the notes are fetched with `--notes`.

## Examples

```bash
libra deps add scene.usd tex/wood.png     # declare a dependency
libra deps list scene.usd                 # direct deps
libra deps list tex/wood.png --reverse    # dependents
libra deps tree scene.usd                 # transitive closure
libra deps tree scene.usd --depth-limit 2 # bounded closure
libra deps why scene.usd tex/wood.png     # shortest dependency path
libra deps rm scene.usd tex/wood.png      # remove an edge
libra deps add a b --revision <commit>    # target a specific commit
```

## Deferred (not v1)

Network / foreign-Git / push-side edge travel (D17), carry-forward of edges onto new commits,
rename-following (path-keyed edges do not auto-migrate), and automatic
dependency inference (v1 edges are author-declared).
