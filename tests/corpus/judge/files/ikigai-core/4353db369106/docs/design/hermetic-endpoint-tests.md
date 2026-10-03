# Endpoints that read the config home — design

**Status:** settled 2026-08-23. An endpoint takes its config home **at construction**,
never at the point of use. Core supplies the path algebra and will not supply a handle
to hold it. Companion to `ikigai_core::config` (PRs #86, #87, #90) and to the
`Invocation` clock seam that landed alongside this note. This document is the
rationale; the code is the spec where they differ.

Sibling to `ambient-app-name.md`, and the same shape of question one level along: that
one asked who supplies the app NAME, this one asks who holds the config HOME and when.

---

## The problem

`config::config_home()` reads `$XDG_CONFIG_HOME` and `$HOME`. Both are process-global.
So an endpoint whose body calls it:

- reads the **developer's real** `~/.config/ikigai` under `cargo test` — on the machine
  this was written on, a real `a11y.toml` with a real contrast floor in it;
- cannot be given a different one by a test without `set_var`, which races the
  harness's own threads;
- gives two tests in one binary no way to disagree, because there is one environment
  and `cargo test` shares a process across a crate's tests.

None of that is a core defect. `config` already ships the injected form of every rule
it states — `config_home_from`, `layered_paths_in`, `data_home_in`, `data_path_in` —
and its own doc comments call those "the only testable form". The seam exists. The
question is who holds the value it produces, and the answer has to be made at a level
core cannot see.

## What it costs when the read stays ambient

`ikigai-a11y` **as it stood when this note was written** is the case to look at, because
it got the loader right and the endpoints wrong, which is what makes it evidence rather
than an oversight. `load::complete_in(home, app)` and `load::threads_in(home, app)` took
the home; `endpoints.rs` called the ambient `complete(app)` / `threads(app)` instead.
(It has since been fixed — `A11yHandle`, a11y PR #4, 2026-08-23 — so read this section
as the diagnosis it came from, not as a description of that crate today.)

Read the test module it had then and the consequence is visible without running
anything. Five
endpoint tests, and every one of them either bails —

```rust
let Ok(rep) = invoke(&presentation(), request, &nothing) else {
    return; // no config home on this machine; the loader said so
};
```

— or asserts only that a field has the right **type**: `is_string()`, `is_number()`,
`is_boolean()`, `is_null()`. Not one pins a value, because not one owns the file the
value comes from. On a machine with no `HOME` the tests silently assert nothing at all;
on this machine they assert against a file that is not theirs.

That is the shape to recognize: tests written *around* a read rather than *of* it. They
pass, they stay passing, and they are not evidence about the endpoint.

## The shape: the home is taken at construction and exposed

**The rule is *taken at construction and exposed*. A handle is one way to hold it;
a builder is another.** `ikigai-log` is the reference implementation of the handle
form, and the rules below are written in its dialect — but a module that already
has a construction seam uses the one it has. `ikigai-browse` (PR #21, merged
2026-08-23) meets all five rules with **no handle at all**: the home rides
`Mount::config_home(Option<PathBuf>)`, a builder method beside the `Mount::app` it
already had, and travels down to the endpoint bodies as `Option<&Path>`. Do not
grow a handle beside an existing builder just to match the shape of the example.

`ikigai-log` as the handle form:

```rust
// The honest form: the caller states the home. A test hands it a tempdir.
pub fn new(home: Option<PathBuf>, app: Option<String>, config: LogConfig) -> LogHandle;

// The sugar, for a host configuring itself from the machine it is running on.
pub fn ambient(app: Option<String>, base: LogConfig) -> Result<LogHandle, ConfigError>;
```

The endpoints close over the handle, so the ambient read happens **once, in a host, at
startup**, where it is a fact about the process rather than a hidden input to a
resolution. The rules:

1. **The injected constructor is the real one; the ambient one is sugar over it.**
   `ambient()` resolves the home and calls `new()`. One code path, so the production
   path is the tested path with a different argument.
2. **`Option<PathBuf>`, not a fallible constructor.** No config home is a legal state —
   the process is under-configured, not broken — and it is `config_home()`'s own
   contract (`None` rather than a guess) carried up one level intact.
3. **Taken at construction, never per request.** A caller does not get to choose which
   config applies; the host does. This is also what keeps the endpoint cacheable on a
   stable key, and it is the same rule `ambient-app-name.md` §2 sets for `app` — the
   home and the name travel together and are held by the same object.
4. **Expose what was taken.** `LogHandle::home()` lets the endpoint body — and a test —
   see the home it is working against instead of re-deriving it.
5. **Never guessed.** No `current_dir()` fallback, no `env!("CARGO_MANIFEST_DIR")`. A
   guessed home reads as success and is the failure `config_home()` refuses by design.

A test then builds the handle over a `tempdir`, writes the exact layers it wants to
assert about, and asserts values — with no environment variable in sight and no race
with the test beside it.

**One refinement the builder form needs and the constructor form does not.** In a
constructor the ambient read is a *different function* (`ambient()` vs `new()`), so
`Option<PathBuf>` carries everything rule 2 asks of it. A builder has a default —
not calling it at all — and that is a third statement: *read this machine's home*.
`None` is already spoken for as a legal stated value (*this process has no config
home*), so the two cannot share it. `ikigai-browse` holds a private
`enum ConfigHome { Ambient, Stated(Option<PathBuf>) }` and resolves the `Ambient`
arm inside `Mount::space`, which keeps rule 1 intact: the ambient read still happens
once, at the point the host builds its mount.

## Why this is documentation and not a core helper

Core cannot own the handle. What a module holds at construction is its own — its
config home, its app name, and in some modules its *parsed config*, in the module's
own type: `LogHandle` carries a `LogConfig`. A core type generic enough to hold all
of them would hold nothing, and would still leave every module to write the
constructor pair that is the actual pattern.

### Whether the handle holds a PARSED config is not a style choice

`ikigai-a11y`'s `A11yHandle` deliberately does **not** carry an `A11y`. It holds the
home and the app name, and re-reads the layers per resolution. The reason is golden
threads: `urn:a11y:config` is `.cacheable()` with a thread on every candidate file,
and the contract of that thread is that **cutting it recomputes**. A handle that
parsed its config at construction would defeat exactly that — the watcher cuts, the
kernel dutifully re-resolves, and the endpoint hands back the struct the process
started with, forever. The layering is cheap and the cache is what makes repeating
it cheap, so the read stays per-resolution and the *home* is what gets held.

`LogHandle` holding a parsed `LogConfig` is right **there** for the opposite reason:
log's config is process state that a Sink mutates in place, so the handle is the
authority on it rather than a stale copy of a file.

The test to apply is therefore: **is the config a file the kernel owns freshness
for, or process state this module owns?** File → hold the home. Process state →
hold the parsed value. The two reference implementations differ on this on purpose;
it is not an inconsistency between them.

The consequence to expect from that choice is the constructor's fallibility.
`A11yHandle::ambient` is **infallible** because nothing is parsed at construction —
an unreadable layer surfaces at the resolution that reads it, which is where a
reader can act on it. `LogHandle::ambient` returns `Result<_, ConfigError>` because
it does parse, and a config it cannot parse is a host that should not start.

What core *can* own, it already owns. The `_in` / `_from` functions are the seam, they
are public, and they are documented as the testable form. Nothing is missing from the
code. What was missing is that the next module author reads `config_home()`, sees a
function that works, and calls it from an endpoint body — because nothing at the point
of the decision says not to. So the fix goes where the question occurs: a pointer from
`config`'s module doc to here.

This is the same conclusion `ambient-app-name.md` reached about `app`, and for the same
reason: the value is a property of the **mount**, not of the process, and one process
legitimately has two answers. A test binary that mounts a fixture home beside a mount
with none is that case, and it is the ordinary case, not a corner.

## What core did change, next door

The `Invocation` clock seam that landed with this note is the same problem in the other
ambient input, resolved the other way — and the difference is instructive. "Now" is a
value core already models (`Clock`, injected into the `Kernel`), so core could close it
in code: `Invocation::with_clock`, plus a shipped `FixedClock`. The config home is a
value core models only as *path algebra*; the holding belongs above. Same disease, two
different right answers, because one of them has a core-shaped seam and the other does
not.

## What would reopen this

Not a preference for less boilerplate. The bar is `ambient-app-name.md`'s — a count
**plus** a demonstrated failure:

- **three or more modules** having independently written the `new(home, …)` /
  `ambient(…)` constructor pair, **and** an observed divergence between two of them
  that a shared helper would have prevented — one treating an absent home as an error
  where another treats it as a legal state, say, or two disagreeing about whether the
  ambient sugar re-reads the environment per call.

Two modules writing the same small constructor pair correctly is the pattern working.

**The count reached three on the day this note landed** — `ikigai-log`,
`ikigai-a11y`, `ikigai-browse` — and the bar is still not met, which is the point of
having two halves to it. The three do not even share a spelling: two constructor
pairs and one builder. They disagree about nothing that matters — all three take the
home at construction, all three treat an absent home as a legal state, none re-reads
the environment per call — and the one place they differ (whether a parsed config is
held) is the difference the section above says is *required* by the two configs'
different owners. Count without drift again.

A related count is already accumulating next door and deliberately not acted on, as the
worked example of this bar: **three** crates (`ikigai-core`'s own tests, `ikigai-http`,
`ikigai-cms-web`) carry an `AtomicU64`-backed settable test clock, which is one more
copy than the fixed clock that just got pushed down. It stays out of core because all
three are correct, none has drifted, and no behavior goes untested because of them —
count without drift, which is exactly the case this section says is not enough.
