# ikigai-core

**ikigai** is a resource-resolution kernel: you address information by identity and
resolve it through composable address spaces, with content-addressed caching so a
result is computed once and reused. Most requests are satisfied by deterministic
resolution; a language model is one optional, last-resort resolver — not the default.

**New here? Start with [The ikigai Book](https://ikigai-rs.github.io/ikigai-tutorial/)** —
the tutorial: resolution, your first endpoint, self-description, modules, and a kernel
behind a socket. Every code block in it is compiled against the published crates.

This repository is the core, dependency-light layer — no network transports, and it
compiles to WebAssembly. The CLI and its transports live in
[`ikigai-cli`](https://github.com/ikigai-rs/ikigai-cli).

## Crates
| crate | role |
|-------|------|
| `ikigai-core`  | identity, representations, resolution, caching, capabilities |
| `ikigai-vocab` | self-description vocabulary |

That is the whole workspace: the kernel and the vocabulary it describes itself with.
Every module crate that once lived here has moved to its own repo — capability-gated
file behaviour to [`ikigai-fs`](https://github.com/ikigai-rs/ikigai-fs) (published;
native `std::fs` + browser `localStorage`), SHACL validation to
[`ikigai-shacl`](https://github.com/ikigai-rs/ikigai-shacl), and the persistent RDF
store to [`ikigai-store`](https://github.com/ikigai-rs/ikigai-store) (since 2026-09-13,
with its history carried across), which now has its durable RocksDB-backed backend and
is published on its own cadence. Each is a separate dependency you add when you want
it; none of them is pulled in by depending on the kernel.

⚠ This table is a claim about the workspace that nothing checks — `ikigai-shacl` sat in
it for roughly two months after that crate had already moved out. Diff it against
`crates/*` when you touch either.

## Status

Pre-1.0 and in active use. 0.1.70 is on crates.io, 51 published crates depend on it, and
the kernel's shape — resources, the five verbs, representations, transreptors,
golden threads, capabilities — has been stable for months.

What that does **not** promise is a frozen API. Under Cargo's 0.x rules the second number is
the breaking one, so this line moves when a type does, and it has moved 64 times. Recent
examples of the kind of change to expect: a public struct gaining a field (which broke a
published consumer once — the reason `Resolved` is now built rather than constructed), and
async endpoints gaining a flat authoring form. Pin an exact minimum and read the release notes
rather than tracking `0.1`.

Where the edges still are, so nobody discovers them by surprise: a module cannot issue a
sub-request under authority it holds rather than its caller's, capability scopes match by
prefix but not infix, and the cache is bounded by policy rather than by an accounting of
memory. Each is written up in the repository's design notes.

## License
Licensed under either of MIT or Apache-2.0 at your option. See `LICENSE-MIT`,
`LICENSE-APACHE`, and `NOTICE`. See also `ACKNOWLEDGEMENTS.md`.
