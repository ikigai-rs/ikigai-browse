# An ambient application name in core — design

**Status:** settled 2026-08-15; **rejected**. Core will not carry a process-global
application name. The name stays an explicit argument taken at mount time, and this
document fixes the shape of that argument so the consumers that take it agree.
Companion to `ikigai_core::config` (PRs #86, #87) and `ikigai-browse` 0.2.12 (its PR #17).
This document is the rationale; the code is the spec where they differ.

---

## The proposal

`ikigai-a11y`'s layering is keyed on an application name: `a11y.toml` merged with
`{app}.a11y.toml`, where `app` is `cms-web`, `dev-server`, `web` — the **binary's**
name. Core already spells the file rule (`config::layered_paths(stem, app)`), but the
`app` argument is the caller's to supply. Binaries know their own name. Libraries do
not, so a library that wants the layering must be *told*.

`ikigai-browse` 0.2.12 solved this for itself with an additive builder knob:

```rust
ikigai_browse::Mount::new(roots).app("dev-server").space()
```

The satellite that built it flagged the shape as a stopgap: every library that wants
a11y layering grows its own `.app()`, and every host passes its own name to each of
them separately. The proposal was to replace the telling with an ambient read — a
`OnceLock`-backed `ikigai_core::config::app_name()`, set once at host startup, that
libraries consult instead of receiving.

## Why it looked right

It is not a bad idea, and the case for it is real:

- The name genuinely **is** a property of the process. `dev-server.a11y.toml` configures
  the front end a person is looking at; a module linked into three servers should read
  three different effective configs. Threading a process fact through every library's
  constructor is the classic shape of a value that wants to be ambient.
- The precedent is fresh. Core absorbed the config-home rule (#86) and the data-home
  rule (#87) on exactly this argument — the rule was written four times, the four had
  drifted, and pushing it down gave the system one spelling. "Push the shared fact into
  core" is a move this codebase has just made twice and been right about.
- The alternative scales badly on paper: *N* a11y-aware libraries in one host means *N*
  places to pass the same string, and *N* places for a host to forget one.

## Why no

### 1. It is a different widening than #86, not a further step along the same axis

Core was, until 0.1.56, free of `std::env`, `std::fs` and `PathBuf`; the config module
was a deliberate widening from "resolution algebra" to "resolution algebra plus a path
rule". That widening added **reads of the process environment** — pure functions of
ambient state, evaluated fresh, with no ordering to get wrong. `config_home()` returns
the same answer at every point in the process's life, and nothing in the program can
make it return the wrong one.

An ambient identity adds **process-global mutable state**: write-once, ordering-
dependent, and answering differently depending on whether the host got there first.
There is no such state anywhere in this workspace today — not one `OnceLock`, `OnceCell`,
`lazy_static` or `static mut` across `crates/`. That is not an accident of a small
codebase; it is what "runtime-free kernel" means in practice. The config module's own
header names the constraint it lives under — config home plus flags is the rule, and an
environment variable is the banned third channel. Process-global state is a fourth
channel, and a less inspectable one than the variable that was refused.

### 2. Its failure mode is silent, timing-dependent, and exactly the one #86 exists to kill

`app_name()` returns `None` until someone sets it. A library that reads it before the
host sets it gets `None` and quietly loads the shared layer only — the operator's
`dev-server.a11y.toml` sits on disk doing nothing, and everything reports success.

That is the precise failure the config-home push-down was written against: a wrong
answer that reads like a right one. But it is *worse* here in the way that matters for
debugging. XDG drift is deterministic — one machine, one wrong directory, reproducible
forever. An initialization-order bug is intermittent: it depends on where in `main` the
set happens relative to space construction, on whether a mount is built lazily, and on
whether the host is itself a library (`ikigai-embedded` is). It reproduces on one
machine and not the next. Trading a deterministic wrong answer for a nondeterministic
one is not the direction of travel.

### 3. A process-global cannot express what the escape hatch has to express

This is the decisive one, and it is not hypothetical — it is in `ikigai-browse`'s test
suite today. `the_stylesheet_is_cacheable_and_declares_the_config_files` builds a mount
with `.app("browse-test")`, in the same test binary as mounts that pass no app at all
and assert they see the shared layer only. One process, two required answers.

A `OnceLock` gives that binary one value: either `browse-test` for every test or `None`
for every test, whichever ran first. `cargo test` shares a process across a crate's
tests, so this is the ordinary case, not a corner. The escape hatch is not an accessory
to the ambient reader; the ambient reader cannot be adopted without it.

And the same shape shows up outside tests. `ikigai-embedded` is a host that is *also* a
library — the CLI links it, and so could anything else. A binary that mounts two front
ends has one process name and two correct answers. The app name is a property of the
**mount**, not of the process; today they coincide in every shipping binary, which is
why the process framing reads as true. A global bakes the coincidence into an API.

### 4. It does not remove the knob, and the repetition it removes is currently zero lines

Per point 3, `.app()` survives on `Mount` regardless — for tests and for a host serving
another config home. So the ambient version does not delete a knob from any library. It
removes the *host's* repetition, and the honest count of that repetition is:

| a11y-aware libraries in the ecosystem | 1 (`ikigai-browse`) |
|---|---|
| hosts passing the name | 1 (`ikigai-dev-server`) |
| lines an ambient name would delete | 0 — one `.app("dev-server")` becomes one `set_app_name("dev-server")` |

The three other crates in the brief — `dev-server`, `web`, `cms-web` — are hosts. Hosts
do not grow knobs; they supply the value, and supplying it once is not repetition, it is
the definition of the string. The repetition only becomes real when a single host mounts
several a11y-aware libraries, and the fix for that is a `const` in the host crate
(see below), not a global in the kernel.

Compare the bar #86 cleared: four independent spellings that had **already drifted**,
with a fifth consumer arriving. Here there is one implementation, used correctly, and no
drift to point at.

---

## What we do instead: the shape of the knob

Four consumers should agree on one shape, so that when a second a11y-aware library
appears it is recognizably the same knob rather than a new dialect. `ikigai-browse`'s
`Mount::app` is the reference implementation; the rules below are what it already does,
written down.

1. **The name is `app`.** Core's `layered_paths(stem, app)` already fixes the
   vocabulary — a library that calls the same thing `application`, `front_end` or
   `profile` is inventing a synonym for a parameter it is about to pass through.
2. **Taken at mount time, never per request.** A caller does not get to choose whose
   accessibility settings apply; the host does. This is also what keeps the resulting
   endpoint cacheable on a stable key.
3. **Optional, and absence means the shared layer only.** `Option<String>` stored,
   `Option<&str>` internally, `impl Into<String>` at the builder boundary. An empty
   string counts as absent, per `layered_paths_in`.
4. **Never guessed.** No `std::env::current_exe()` fallback, no `CARGO_PKG_NAME`, no
   panic on absence. A mount that names no app is under-configured, never wrong — which
   is `config_home()`'s "`None` rather than a wrong guess" contract, one level up. A
   guessed name is worse than none: `CARGO_PKG_NAME` in a library yields the *library's*
   name, and would read a `browse.a11y.toml` that no operator was ever told to write.
5. **Added as an additive builder method**, never as a new positional constructor and
   never by changing an existing signature. Existing constructors survive their
   consumers' caret pins; this is why `Mount` exists beside `space*` rather than
   replacing it. A library with no builder yet grows one — that is the migration, not a
   second constructor taking one more argument.
6. **A host spells its name once**, as a crate-level constant, and passes it to each
   mount:

   ```rust
   /// This binary's name — the `{app}.a11y.toml` layer operators write against.
   pub const APP: &str = "dev-server";
   ```

   That is the answer to "the host repeats itself": a `const`, visible at the top of the
   crate that owns the fact, with a compiler error if it is misspelled at a use site.
   It gives the single-spelling property the global was wanted for, and gives up nothing.

## No change to core

The decision costs core nothing but a pointer: `config::layered_paths`' documentation
records that the `app` argument is the caller's and why core will not supply it, so the
next reader finds the reasoning where the question occurs to them rather than here.
There is no API change, so no publish and no consumer bumps.

`ikigai-browse` needs no migration. Its `.app()` knob is the settled shape, and
`ikigai-dev-server`'s call site is the settled usage.

## What would reopen this

Not an aesthetic objection and not a third `.app()` call site. The bar is the one #86
cleared — a count *plus* observed drift:

- **three or more independent libraries** (not hosts) taking an app name, **and**
- a host observed passing **inconsistent** names to two of them, or a host observed
  passing the name to one library and forgetting another.

At that point the ecosystem has demonstrated the failure the global prevents, and the
trade against the failure the global introduces (§2, §3) can be re-argued with evidence
on both sides. Until then it is one knob, used correctly, in one library.

If it is reopened, the escape hatch is not optional and the shape is already known:
`set_app_name` / `app_name`, with every library still accepting an explicit
`Option<&str>` that **wins over** the ambient value — ambient as the default for the
argument, never as a replacement for it. A design that removes the argument is the one
this document rejects; a design that defaults it is a much smaller claim.
