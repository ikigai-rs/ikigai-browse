# Reading git in-process through gitoxide: a measured spike

Ledger #2496. Status: research note, with a measurement example behind the opt-in `gix-spike`
feature. Default behavior is unchanged: no library code calls `gix`, the feature is not in any
default or CI feature set, and every browse endpoint still reads git through the hardened CLI in
`src/checkout.rs`.

## Question

Would browse serve its git **reads** better through the `gix` library (gitoxide) than through
`git` subprocesses? Writes, `gh` and GitHub stay on the CLI whatever the answer is. The item
listed five claims. Each one is graded below as **held**, **partly held**, **did not hold** or
**not tested**.

## What browse actually runs today

Before timing anything, I counted the git spawns per endpoint from the source (every spawn goes
through `Jailed::git` / `Jailed::git_batch` in `src/checkout.rs`):

| Endpoint | git spawns per request | Commands |
|---|---|---|
| `tree`, `file` | **0** | none: both read the working tree through the jail (`openat`, `O_NOFOLLOW`), not git objects |
| `git-state` | 3 | `status --porcelain -z -- .`, `rev-parse --show-prefix`, `rev-parse HEAD` (`src/lib.rs`, `git_state`) |
| `hash:` | 1 | `ls-files -z --others --ignored --exclude-standard --directory` (`src/hash.rs`, `git_ignored`) |
| review judgment (find the reviewed version) | 3 | `rev-list --all -- <path>`, `cat-file --batch-check`, `cat-file --batch` (`src/judge.rs`) |

There is no "log for a path" endpoint today. The nearest thing is the judgment's
`rev-list --all -- <path>`. This changes the scope of claim 1: the most common pages (tree and
file) spawn no git at all, so a git library cannot speed them up. Only `git-state`, `hash:` and
review judgment can gain.

## Method

`examples/gix-read-spike.rs` (`cargo run --release --example gix-read-spike --features gix-spike --
<repo> [path] [n]`) runs each read three ways and compares the answers:

- **cli**: `git -C <repo> …`, one process per read. This is the same as browse's spawns, minus
  the `fchdir` pre-exec and the environment scrubbing, which cost nothing measurable.
- **gix warm**: one `gix::Repository` opened once and reused. A long-lived module would hold it
  this way.
- **gix cold**: `gix::open` on every read. This is the like-for-like comparison with a spawn,
  which rediscovers the repository each time.

Each cell is the median of `n` runs after one warm-up run. The `parity` column compares the
CLI's answer with gix's (a hash, an entry count, a size or a dirty-path count).

- Machine: a macOS laptop, Rust 1.99.1, git 2.54.0, release build, `gix` 0.90.0.
- Load: quiet. These are not the loaded upper-bound conditions of the item's original numbers,
  which were 7.5–16 ms per call.
- Repositories: this crate's own checkout, which is a linked worktree (about 270 tracked files,
  one modified and one untracked), and seven small fixtures built by script under
  `target/spike-fixtures`.

## Results

### This checkout (worktree, `n=50`, path `Cargo.toml`)

| Read | cli | gix warm | gix cold | parity |
|---|---:|---:|---:|---|
| `rev-parse HEAD` | 5.06 ms | 0.036 ms | 0.214 ms | same |
| `ls-tree -r HEAD` (entry count) | 5.82 ms | 0.241 ms | 0.467 ms | same |
| `cat-file -s HEAD:<path>` (size only) | 5.40 ms | 0.087 ms | 0.268 ms | same |
| `cat-file -p HEAD:<path>` (bytes) | 5.53 ms | 0.163 ms | 0.348 ms | same |
| `log -1 -- <path>` | 5.51 ms | 0.184 ms | 0.368 ms | same |
| `status --porcelain` (dirty count) | 11.3 ms | 5.03 ms | 5.31 ms | same |
| **`git-state` page (3 reads)** | **21.4 ms** | **5.06 ms** | **5.35 ms** | same |

With `src/lib.rs` (a 4,000-line file) as the path, the numbers were within 0.15 ms of these.

### Awkward cases (`n=10`, path `a.txt`)

| Fixture | `git-state` page, cli → gix warm | Object/ref reads, cli → gix warm | parity |
|---|---|---|---|
| loose refs | 15.3 → 0.25 ms | ≈5.0–5.3 → 0.03–0.19 ms | all same |
| packed refs (`pack-refs --all`, `gc`; no loose heads) | 15.4 → 0.13 ms | ≈5 → 0.01–0.03 ms | all same |
| empty repository (unborn `main`) | 15.0 → 0.11 ms | ≈4.9 → 0.01 ms | see below |
| SHA-256 object format | 15.4 → 0.25 ms | ≈5 → 0.04–0.11 ms | all same |
| sparse checkout (cone `d`) | 15.7 → 0.34 ms | ≈5 → 0.04–0.31 ms | all same |
| superproject with a dirty submodule | 19.7 → 0.84 ms | ≈5 → 0.04–0.20 ms | all same (both report `sub` modified) |
| linked worktree on another branch | 15.6 → 0.37 ms | ≈5 → 0.04–0.19 ms | all same |

**Empty repository.** The rows reported as different are an artifact of how the example
compares output. `git rev-parse HEAD` on an unborn branch fails but still echoes `HEAD` on
stdout, and the example compares stdout without checking the exit status. browse's `git_state`
filters on success and gets `head: None`. gix's `head_id()` is an error, which also maps to
`None`. The two agree on what browse would serve.

### Dependency, build and MSRV cost

- **Features:** `gix` with `default-features = false` needs `sha1` and `sha256` (it refuses to
  compile with neither), plus `status` and `revision`. That covers every read above.
  - `hash:`'s ignored-path listing would also need gix's dir-walk / excludes support. It is not
    measured here.
- **Added crates:** 79 (238 against 159 unique normal and build dependencies, from `cargo tree`).
  Of those, 50 are `gix` or `gix-*` crates. None of them is in the default build.
- **Build time:** about 31 s of wall clock (≈375 CPU-seconds at 16 threads) to add `gix` to an
  otherwise built release tree, across two runs. This is approximate, not a clean measurement.
- **MSRV:** `gix` 0.90 declares `rust-version = 1.88`. This crate declares none.

## The claims

1. **Speed: an order of magnitude or more. Partly held.**
   - **Object and ref reads: held, by a wide margin.** They drop from about 5 ms to 0.01–0.3 ms
     warm, 25–150×. They are still ≥10× faster when the repository is opened again on every
     read.
   - **Working-tree status: did not hold on a real checkout.** It goes from 11.3 to 5.0 ms (2.2×)
     on this checkout, because stat-ing the work tree dominates and a library does not avoid
     that. On the small fixtures it was 20–60×.
   - **Per page:**
     - `git-state` drops from 21 to 5 ms (4×). It is bounded by status.
     - Tree and file pages spawn no git, so they gain nothing.
     - Review judgment's three spawns are all object/ref reads, so it is where the order of
       magnitude is real: about 15 ms of fixed cost per judgment becomes under 1 ms.
2. **Caching: object reads are immutable and only refs and the work tree are volatile. Not
   tested.**
   - The design is sound on its face: an object id names its content.
   - The kernel-level proof the item asks for was not built in this spike. That proof is: read
     through the kernel, commit to an unrelated branch, show the read stays cached, then commit
     on the branch and show exactly its dependents recompute.
   - It is a follow-up below. Nothing here should be read as evidence for it.
3. **Structure: typed objects, ids, size known before reading. Held.**
   - `find_header(id).size()` gives a blob's size without reading its body. It matched
     `cat-file -s` everywhere and costs about half of a full read. That is enough to refuse an
     over-budget body up front.
   - Ids are typed `ObjectId`s, SHA-1 or SHA-256, and parity held on a SHA-256 repository.
4. **Authority: `ambient: fs` only, never `process`. Held for the spike.**
   - Every gix read in the example is in-process file I/O and spawns no `git`.
   - A production endpoint would still have to decide how gix's own repository discovery and
     reads relate to the `Jailed` name confinement. Like the CLI today, gix reads `.git` outside
     the jail, which refuses `.git` paths by design.
5. **Safety: gix does not run code that a repository's config names. Held for `core.fsmonitor`.**
   - I wrote a fixture whose `.git/config` sets `core.fsmonitor` to a script that touches a
     marker file.
   - `git status` ran it and the marker appeared.
   - gix's status alone (`SPIKE_GIX_STATUS_ONLY=1`) did not run it, and the marker was absent.
   - This also shows that browse's current CLI hardening (`harden_git`: environment scrubbing and
     `safe.bareRepository=explicit`) does not stop a non-bare repository's **own** config from
     starting a hook on `status`. That config is not tracked content, so this is a host-trust
     question rather than a remote-content one, but it is a real difference between the two
     paths.
   - The `safe.directory` / ownership class was not exercised here.

## Recommendation

- **Use gix for object and ref reads.** Move review judgment's history lookup (`rev-list --all
  -- <path>` plus the two `cat-file` batches) and the two `rev-parse` calls in `git-state` to
  gix, behind a feature first. That is where the order-of-magnitude gain holds, and it removes
  `process` authority from the review path.
- **Status is a closer call.** The 2× speedup and claim 5 (no repo-configured hooks) argue for
  moving it, but it is the read with the most edge-case semantics: renames, submodules, sparse
  checkouts and ignore rules. It needs a parity test over porcelain output, not just counts,
  before it moves.
- **Tree and file pages:** leave them alone. They read the working tree, not git.
- **Other consumers:** do not move ikigai-repo's read endpoints, or other tools' history reads,
  on these numbers alone. The per-call gain is real, but it pays off only where a caller makes
  many reads per request or holds a repository open. Measure each caller's spawns per request
  first, the way the table above does for browse.
- **Dependency cost:** budget for it. It is 79 crates, about 30 s of build and an MSRV floor of
  1.88. Keep it behind a feature until the caching claim is proved.

## Proposed follow-up items

1. **Prove ref-scoped invalidation for gix reads through the kernel.** Write a test that reads a
   blob by id and a ref through the kernel, commits to an unrelated branch (the read stays
   cached), then commits on the read branch (exactly the ref's dependents recompute). The watcher
   is on `.git/refs` and `packed-refs`, plus `HEAD` and the worktree's own `HEAD`.
2. **Move review judgment's version lookup to gix behind a feature.** Replace `rev-list --all --
   <path>` and the two `cat-file --batch` spawns in `src/judge.rs` with gix reads. Keep the CLI
   path as the default, and add a parity test over the existing judge corpus.
3. **Read `git-state`'s HEAD and prefix through gix.** Replace the two `rev-parse` spawns, and
   keep `status` on the CLI until item 4 lands. Cover the unborn-branch and not-a-repository
   answers.
4. **Write a status parity test between gix and `git status --porcelain -z`.** Compare full
   porcelain records, not counts, on renames, submodules (dirty and new commits), sparse
   checkouts, ignored files and a root below its work tree's top. Run it before status moves.
5. **Decide whether the CLI path should neutralize repo-configured hooks.** For browse's read
   spawns, consider `core.fsmonitor=false` (and similar) in the protected command-scope config
   next to `safe.bareRepository`. Mirror whatever is decided in ikigai-repo's copy of the
   hardening.
6. **Confine gix's repository reads like the jail confines names.** Specify how a gix-backed
   endpoint opens its repository: discovery bounds, the ceiling directories, refusing a tracked
   bare repository, and environment overrides. That way it is no weaker than `harden_git`.
7. **Count git spawns per request in ikigai-repo and other history readers.** Repeat this note's
   spawn census for each candidate caller before deciding whether its reads should follow.
