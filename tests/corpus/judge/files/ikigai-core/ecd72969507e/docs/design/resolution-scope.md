# Resolution scope: the chain, its two faces, and what the cache owes it

**Status:** built, core 0.1.72 (ledger [#509](http://localhost:1060/l/default/item/509));
the chain reaches selection, the probe and the pipe, and carries a clock, core 0.1.75
([#516](http://localhost:1060/l/default/item/516),
[#517](http://localhost:1060/l/default/item/517)).
**Anchor types:** `ikigai_core::{Scope, Scope::with_named_at, Confine, Invocation::confine,
Kernel::issue_in, Kernel::issue_with_incoming_in, Kernel::is_cached_in,
Kernel::select_transreptor_in, Kernel::select_action_in, Kernel::select_actions_in,
Issuer::issue_in_scope, Issuer::select_action_in, CacheKey::scope, CacheRow, SCOPE_NOTE,
SCOPE_MISS_NOTE, SCOPE_CLOCK_NOTE}`.
**Companions:** `docs/design/resolution-context.md` (levels: the resolved scope an endpoint
found inside a `Level` runs in, and sealed names — ledger
[#563](http://localhost:1060/l/default/item/563)),
`docs/design/sub-request-authority.md` (the authority argument this reuses
verbatim), `docs/design/cache-ejection.md` §2 (a fingerprint is not an identity),
`docs/design/spaces-as-named-graphs.md` (where corridor identity goes next, ledger
[#515](http://localhost:1060/l/default/item/515)), `docs/formalism/README.md` (the formal
companion to the paper: this chain as the paper's Γ, every claim pinned to a test).
**Normative reference:** Peter Rodgers, *Peter's Hotel: A Set-Theoretic Formalism* v5.18 —
§3 (context chains, value corridors), §9.1 (the trapdoor, Def. 10), §9.3 (confinement,
Prop. 4), §9.4 and §5.1 (cache sharing across a boundary).

---

## The gap, as found

`Scope { injected: Vec<Arc<dyn Space>> }` existed and was never populated. Every resolve
passed `Scope::empty()`, `EndpointSpace::resolve` ignored it, and every sub-request
re-entered the same root through `issue_under → issue_scoped → issue_inner`. Resolution was
a pure function of one static host-built tree, so the only confinement the kernel could
offer was a capability refusal. The paper's claim, which this arc makes true here: *a
denial is a decision that can be misconfigured; an unresolvable identifier has nowhere to
go.* Inside a severed chain a resource outside it must be `Unresolved`, not `Denied`.

## The primitive: one chain, two faces

A request is resolved in a **chain** `⟨injected corridors (innermost first), root⟩`.
`Scope` is that chain minus the root the kernel holds: the corridors, their identities, and
whether the root is still on the end. `Scope::empty()` — nothing injected, root present —
is what every plain `Kernel::issue` resolves in, and it is the status quo by construction
(fingerprint `0`, no trace note, key unchanged).

The kernel walks it in `Scope::resolve_in`: each injected corridor innermost first, then
the root unless severed. Both faces are that one walk:

| | **Injection** | **Severing** |
|---|---|---|
| Chain | `⟨C…, root⟩` | `⟨host corridors…, S⟩`, no root |
| Effect | `C` **shadows** a root door, for the request and every sub-request | anything not in the chain is `Unresolved` |
| Paper | value corridor (§3) | trapdoor (§9.1) |
| API | `Kernel::issue_in(request, cap, Scope::empty().with_named(name, C))` | `Confine::new(name, S, inner)` / `inv.confine(name, S)` |
| Who | the host, holding the kernel | any endpoint, on itself |
| Worst outcome | a spoofed door for everything below | an `Unresolved` |

The chain is a property of the **request** and travels with it: the kernel stamps the
`Invocation` with the chain its request resolved in (`Invocation::with_scope`, crate-private),
and `issue_under` / `fan_out` hand it to `Issuer::issue_in_scope`, so a confinement holds
under everything below the endpoint that entered it — including across a `fan_out` spawn
(test: `fan_out_from_a_confined_endpoint_stays_confined`).

## The nine decisions

### 1. The cache key gains a scope dimension

`CacheKey` is now `(request id, capability fingerprint, scope fingerprint)`. Paper §9.4: a
cached representation may be shared across a boundary only if the corridors actually
consulted are the same on both sides. The same request id under the same capability can
resolve to a **different endpoint** in a different chain (`urn:shared` bound in the root and
in a corridor), so a key without the chain would serve one chain's answer to the other —
silently, types identical, tests green: the ~2000× cms-web shape with the sign flipped.

The fingerprint is over the **whole chain**, not the corridors consulted — sound and
over-partitioned. Two chains differing only in a corridor neither request touched do not
share. Consulted-corridors-only is a later refinement and needs the resolver to report
which corridor answered (it can: `Resolved` is the place, the way `canonical` is reported).

The empty chain fingerprints to `0`, `CacheKey::new(id, cap)` sets `scope: 0`, so every
key built before this change is byte-for-byte the empty chain's key. Tests:
`the_empty_scope_is_the_status_quo`, and the §9.4 pair
`a_result_computed_{outside,inside}_confinement_is_not_served_{inside,outside}_it` — both
directions, with invoke counters.

`urn:kernel:*` is keyed with scope `0` whatever chain the caller ran in: it is resolved
ahead of the chain (decision 4), so its answer does not depend on it.

### 2. What is fingerprinted — a name the injector supplies

Spaces have no identity (#515). `Arc` pointer identity would partition a process-local
cache soundly and would be useless for the case injection exists for: a temporal corridor
rebuilt per request is a new pointer every time, so nothing ever shares. (And an address
is reused after the `Arc` drops while the cache entry lives on — unsound as well as
useless.) So the identity is a **name the injector supplies**: `Scope::with_named(name: Iri,
space)`, the value corridor's identifier in the paper's terms
(`urn:ctx:time:2026-09-25T18:00Z`).

**The name is a claim**, exactly like `Resolved::canonical`: same name ⇒ same doors ⇒ same
resource. Name two different corridors alike and one request is served the other's cached
answer. Said on the type, with a doctest that shows two freshly built corridors under one
name producing one cache entry and a different name producing a second.

`Scope::with(space)` (unnamed) is kept — it was public — and mints a process-unique
identity from a counter, so an anonymous corridor shares with its own clones and nothing
else. Its doc says so and points at `with_named`.

**What #515 still needs**, stated for the hub rather than done here:

- A space's identity should be **on the space**, not supplied at injection: a `Space` that
  knows its own IRI could be injected without naming it, be listed in a topology, and be
  named by a trace as *the corridor that answered*. Today identity is a property of the
  injection, so the same space injected under two names is two cache partitions.
- Chain order is part of the fingerprint (`⟨a, b⟩ ≠ ⟨b, a⟩`), correctly — but that means a
  topology resource has to represent order, not just membership. `ik:layers` in
  `spaces-as-named-graphs.md` is an ordered list; keep it one.
- The trace names the chain (decision 9), not the corridor that answered. Naming the
  answering corridor needs the identity to come back on `Resolved`, which is the same
  seam consulted-corridors-only caching needs. One change serves both.

### 3. Injection is authority — only the host may inject with root

Whoever pushes a corridor innermost can stand in for `urn:personal:contacts` for every
sub-request of that resolution. So the widening face is `Kernel::issue_in`, reachable only
by whoever holds the `Kernel` — the trust line of `Capability::root()`. Holding a `&dyn
Issuer` for the kernel is the same trust line (`issue_scoped` already lets an issuer-holder
pass any capability), and `Invocation`'s issuer field is private, so an endpoint cannot get
one from its invocation.

From inside an endpoint the only chain-changing operation is `Invocation::confine(name,
space)`, and its shape is the argument: it takes the chain the invocation runs in, puts
`space` **behind** every corridor already there — where the root used to be — and cuts the
root off. Relative to the chain it started in, nothing resolves differently except what the
root would have answered. It cannot get ahead of a corridor the host injected, so it can
never shadow one; the root's doors are exactly what confinement exists to remove. The
worst outcome is an `Unresolved`. This is `issue_attenuated`'s shape applied to the chain,
and `sub-request-authority.md`'s load-bearing fact applies verbatim: *an
authority-carrying sub-request takes its authority from the kernel, never from its
caller.* `Invocation::with_scope` is `pub(crate)` for the same reason `issue_under` is
private.

The consequence that ordering carries, stated rather than left to be re-derived: **a
host-injected corridor can shadow S's own doors inside a confinement.** An endpoint confined
to `S` that sources a name both `S` and a corridor the host injected bind gets the corridor's
answer, never `S`'s — the host's per-request context outranks the endpoint's confinement
exactly as it outranks the root. The paper's Definition 10 places `S` innermost and would
answer from `S`; the swap is deliberate (an endpoint must not get ahead of a host corridor)
and is argued as a deviation in `docs/formalism/README.md` §1.3.

And `Confine` is bound at one door — it is **not transparent**. The paper's trapdoor is a
transparent overlay that admits from outside everything `S` serves, which is how its worked
example (§12.3) finds the model client reachable from the application corridor and prescribes
wrapping the trapdoor in a mapper that exposes one identifier (§9.3). `Confine` is that
already-wrapped form: bound under the inner endpoint's own name and description, with `S`
never exposed outward, so that leak cannot arise here — nothing outside the confinement can
resolve a door of `S` at all, only the one identifier the decorator is bound at.

Nested confinement follows: an endpoint confined to `S1` that confines again with `S2`
runs in `⟨host corridors…, S1, S2⟩`, severed — `S2` adds doors `S1` lacks and shadows
nothing. Test: `an_endpoint_can_only_narrow_its_chain`, which also pins that an endpoint
reached *through* the corridor is confined too (the chain is closed under the subtree).

An open question recorded rather than decided: whether `confine` should be allowed to
**drop** the host's corridors (strictly narrower still, but it would make a pinned
`urn:time:now` unavailable inside rather than pinned). Kept them, on the view that the
host's per-request context is a claim about the request, not a privilege of the endpoint.

### 4. `urn:kernel:*` stays outside the chain

The kernel intercepts its own namespace before the root and before the chain; `Alias`
refuses to alias it; an injected corridor cannot shadow it either, and a severed chain
still reaches it (capability-gated as ever). Consequence worth naming: a confined endpoint
can read `urn:kernel:catalog` and see a list of names it cannot resolve. Names, not
content — the same leak `sub-request-authority.md` accepts for golden threads. Test:
`an_injected_corridor_cannot_shadow_urn_kernel_and_a_severed_chain_still_reaches_it`.

### 5. The capability floor is unchanged and runs against the endpoint that answered

Whichever corridor answered, `unsatisfied_scope` is evaluated on *that* endpoint's
declaration, after resolve and before the cache lookup — so a corridor shadowing an open
root door with a gated one is gated, and its cached entry is fenced from a caller the
declaration refuses. Test:
`the_capability_floor_is_evaluated_on_the_corridor_endpoint_that_answered`.

### 6. Values are not corridors here

The paper carries a request's values as transient corridors so they can be resolved by
identifier, and notes that value corridors accumulate down the chain so a trapdoor exposes
all of them unless filtered. ikigai's values already travel **on the `Request`** as
`ArgRef`s, and a confined sub-request carries its own request's args and nothing of its
parent's — there is no accumulation to filter. Modeling value corridors would add a second
way to say what the request already says, and a second thing the trapdoor has to strip.
Not modeled; if a by-reference argument (`ArgRef::Reference`) names something outside the
chain, dereferencing it inside is `Unresolved`, which is the trapdoor working.

### 7. The wire drops the scope — a named hole, in two places

`Issuer` implementors outside core (`ikigai-module`'s `SessionHostIssuer`,
`SocketHostIssuer`, `HostBridge`) implement only `issue`, and a mounted remote cannot carry
a chain at all: the wire has no field for it. So a `Confine`d endpoint that reaches a mount
inside `S` escapes the confinement **at the wire** — the remote resolves in its own root.
In the paper's terms a mount is a *named exception endpoint* whose external access is part
of the boundary (Prop. 4): confining to a corridor that contains a mount confines to
whatever the mount reaches. That is the host's decision to make when it builds `S`, and it
is not closable from core without a protocol version bump (a chain on `Call::Issue*`),
which is a different arc.

The second place is closable and is closed: `Issuer::issue_in_scope`'s **default refuses a
non-empty chain** (`Error::Endpoint`, naming the chain and the limitation) instead of
dropping it. An empty chain delegates, so every existing issuer behaves exactly as before;
a module endpoint inside a confinement — or under a host-injected corridor — fails loudly
rather than resolving in the plain root on the branch that looks like success. Test:
`an_issuer_that_cannot_carry_the_chain_refuses_a_non_empty_scope_rather_than_escaping_it`.
The cost: a host that starts using `issue_in` with module endpoints in the path gets a
refusal until `ikigai-module` implements the seam (three sites, each a delegation to
`issue_in` on its kernel once the wire carries the chain; until then they cannot).

### 8. Public surface: additive

- `Scope`: `with_named`, `sever`, `confined`, `is_severed`, `is_empty`, `fingerprint`,
  `Display`. `with`, `spaces`, `empty` unchanged. Representation changed (a thin
  `Option<Arc<Chain>>` handle) — fields were private.
- `Issuer::issue_in_scope` — new **defaulted** method; `issue_scoped`'s signature untouched.
  Grepped: no implementor of `issue_scoped` or `issue_with_parent` exists outside core;
  the three module issuers implement `issue` only.
- `Invocation::confine`, `Invocation::scope` — new. The scope field is private, so
  additive.
- `Kernel::issue_in` — new.
- `Confine` — new type, new module.
- `CacheKey::scope` — new **public field**, `#[serde(default)]`, plus `in_scope`.
  Grepped: nothing outside core constructs or destructures `CacheKey` (the two-argument
  `new` is unchanged). The serialized form gains a field; nothing consumes it yet
  (`cache-ejection.md` is design only).
- `SCOPE_NOTE`, `SCOPE_MISS_NOTE` — new constants.

Nothing forced a breaking change. Bumped 0.1.71 → 0.1.72: additive API is a *minor* in
semver terms, but the ecosystem is lockstep on `^0.1.x` — a `0.2.0` would ceiling out
every one of the ~31 consumers (rule 5's second half) for a change none of them has to
adopt.

### 9. Traced without a struct change

Two reserved note keys beside `DENIED_NOTE` / `ALIAS_NOTE`, so `TraceEvent`'s postcard
layout is untouched:

- `SCOPE_NOTE` (`"scope"`) — on every event of a non-empty-chain resolution (computed,
  cache hit, denial, miss), paired with the chain as `Scope` renders it: innermost first,
  ending in `root` or `severed` (`"urn:ctx:doc:1 severed"`). The two terminal tokens carry
  no colon, so they cannot be confused with a corridor IRI; an anonymous corridor renders
  as `_:<n>`.
- `SCOPE_MISS_NOTE` (`"scope-unresolved"`) — a miss **inside** a non-empty chain is now
  recorded (a plain miss in the empty chain still records nothing), paired with the target.
  The error is `Unresolved`, indistinguishable from "no such resource" by design; the trace
  is the only place "this name IS bound, just not in here" can show.

Empty-chain events are byte-identical to before. `ikigai-log` maps note keys straight
through its vocabulary table, so these need one term each there. Test:
`the_chain_is_disclosed_on_every_traced_event_and_a_confined_miss_is_traced`.

## The four faces (#516) — three reached in 0.1.75, one left at the wire

After 0.1.72 the chain governed resolution, the key, the floor and the trace, and four faces
still answered for the empty chain. The rule that closes three of them: **the scope is the
resolution context everywhere the kernel answers a question about resolution**, and every
scoped form is the existing form when the chain is empty (tested byte-identical, not argued).

1. **Selection** — `Kernel::select_transreptor_in` / `select_action_in` / `select_actions_in`
   select over the chain: the kernel's own operations ahead (as on the issue path), each
   injected corridor innermost first, then the root unless severed. The walk is
   `Scope::consulted`, the same one `resolve_in` takes — factored, so the manifold cannot
   drift from resolution — presented to the `entries → Meta → describe` walks as one space
   (`ChainView`), first hit wins, a pattern bound twice listed once as the innermost binds it
   (a shadowed door is not reachable, so it is not offered). `Invocation::select_*` pass the
   invocation's own chain, so an endpoint gets an honest manifold without knowing it is
   confined; the `Meta` arm plans `transrept_meta` over the chain and runs the steps in it
   as sub-requests of the Meta resolution (they were plain root `issue`s before, outside the
   trace and the depth budget). `Issuer::select_*_in` are defaulted: the empty chain
   delegates, a non-empty one offers **nothing** — the fail-closed twin of
   `issue_in_scope`'s refusal, since an offer the chain cannot resolve is the over-offer.
2. **The probe** — `Kernel::is_cached_in(request, cap, scope)`; `is_cached` is its
   empty-chain case. `urn:kernel:cache` gained a scope column: `root` for the empty chain,
   else the chain as `Scope` renders it, from a small kernel-side map fingerprint → rendered
   chain written when a scoped result is stored (never on the empty-chain path) and pruned
   to resident fingerprints when the readout renders; a fingerprint whose name is gone
   prints as hex. (`Kernel::probe` in the brief does not exist; `ReprCache::probe` takes a
   `CacheKey`, which already carries the scope.)
3. **The pipe** — `Kernel::issue_with_incoming_in(request, cap, incoming, scope)`;
   `issue_with_incoming` is its empty-chain case. The engine change that runs a pipeline
   stage by stage in one chain is `ikigai-cli`'s.
4. **The wire** — unchanged, deliberately: the chain does not cross it and the default
   `Issuer::issue_in_scope` refuses a non-empty chain. Carrying it is a protocol bump and its
   own decision (decision 7 above).

## The clock seam — decided (#517), built 0.1.75

The question as it stood: `Invocation::now()` read the injected `Clock` through the issuer,
so a temporal corridor pinning `urn:time:now` pinned the resolution seam and left the clock
seam live — an endpoint stamping `derived_at` from `now()` inside the corridor reported the
wall clock beside data resolved as-of the pinned instant. Brian's decision (2026-09-25):
option (a) with (c)'s split — a scope-level clock **derived from the corridor at injection**,
and the kernel keeps its own clock for validity.

**Mechanically, "derived at injection" is one call.** `Scope::with_named_at(name, space,
clock)` injects the corridor and sets the chain's clock together; there is no other way to
put a clock on a chain, so the binding and the clock cannot be set independently. Core
cannot verify the pairing — it would have to resolve the corridor's time door to find out,
and a corridor is any `Space` — so the pairing is a **claim the injector makes**, of exactly
the shape a corridor's name already is: *the time this corridor binds is the time this clock
reads.* The doctest on `with_named_at` shows the intended pairing.

**Resolution order.** `Invocation::now()` answers from the first of: a clock attached with
`with_clock` (what a caller stated beats what it inherited; reachable only on a detached
invocation), the **chain's** clock, the issuer's. `Issuer::now` is unchanged — the chain clock
is read off the invocation's scope, so it costs a null check on a chain-less invocation and
nothing at all on the cache-hit path, which never calls `now()`. At most one clock per chain
and the **innermost wins**: a temporal corridor injected inside another replaces its clock as
its `urn:time:now` shadows the outer one's; a corridor injected without a clock leaves the
chain's as it was; `confined` and `sever` keep it (an endpoint cannot change the host's time
any more than it can drop the host's corridors — decision 3).

**Validity stays on the kernel's clock.** `Expiry::At` is judged against `started`, read from
`self.clock` — never from the chain — so a pinned past cannot un-expire a live entry and a
pinned future cannot expire a fresh one; `is_cached_in` judges the same way. Pinned in both
directions (`validity_is_judged_on_the_kernels_clock_never_the_chains`). The consequence
worth stating because it is the one thing that changes what an endpoint *observes*: an
endpoint that turns a freshness window into an absolute deadline from `inv.now()` gets a
deadline in the **corridor's** time judged in the **kernel's** — under a pinned past it is
already expired (never cached), under a pinned future it outlives its window. As-of data is
a pure function of its context and should declare `.cacheable()`, not `cacheable_until`;
said on `Invocation::now`.

**The fingerprint does not change.** Argued rather than assumed: the clock is a property of
the corridor's *name* — the injector pairs them, and the name already claims "same name ⇒
same doors", of which the time door is one — so hashing it would hash a fact the name
states. Two injections under one name with different clocks are one claim made twice with
different content: the injector's error, exactly as two different spaces under one name,
and the cache treats them as one context. The alternative has nothing to hash: an
`Arc<dyn Clock>` has no stable identity (address reuse is why anonymous corridors use a
counter), so it would have to be the instant, which exists only for a clock that does not
move — a derived clock that ticks from a pinned origin has none. Pinned:
`the_fingerprint_is_the_corridors_name_not_its_clock`.

**Trace.** One reserved key beside `SCOPE_NOTE`: `SCOPE_CLOCK_NOTE` (`"scope-clock"`), the
instant the chain's clock read as the event was recorded, only when the chain carries one.
Added because the name alone cannot tell a reader whether `inv.now()` was pinned — a chain
`with_named` under a temporal-looking name and one `with_named_at` render identically — and
the note is what the kernel observed rather than what the injector claimed. One clock read
per traced event in a clocked chain; nothing off the trace path. `ikigai-log` needs one
vocabulary term for it.

**The payoff, pinned exactly** (`a_temporal_corridor_pins_both_faces_of_time_and_the_pinned_read_is_cacheable`):
an endpoint that sources `urn:time:now`, calls `inv.now()` and returns `.cacheable()` has
effective expiry `Always` under the root (the live door is uncacheable) and `Never` under
the pinned corridor; a second `issue_in` under the same named corridor is a cache hit; the
same name under two corridors is two entries; both faces agree under each; and inside a
confinement the manifold offers only what the chain resolves. Pinning the context turns an
uncacheable "now" into an immutable "then" — the paper's §5.1 and the context-tourism
direction, as one test.

## Not built, on purpose

Geographic / personal corridors, a context resource, the limiter (#511),
consulted-corridors caching, the chain on the wire (the depth budget, #513, left this list
in 0.1.73: `Kernel::with_max_depth` bounds a confined chain that recurses through a
corridor endpoint calling itself, though — like the chain — the depth does not cross the
wire), space identity beyond decision 2, the topology resource. A ticking derived clock (a
pinned origin plus elapsed real time) is a `Clock` implementation a host may write; core
ships only the fixed one. The engine's `as-of` affordance, `urn:tz:now` reading through the
invocation, and the REPL showing the scope are `ikigai-cli`'s.

## The read measurement

Cache-hit read on the empty-scope path (`Kernel::issue`, one cacheable `FnEndpoint`,
warmed once, 200 000 re-issues per round, three rounds, release, `futures::block_on`, M-series
laptop). The first round of each run is cold and reported for honesty; the warm rounds are the
number.

| tree | run | round 0 (cold) | round 1 | round 2 |
|---|---|---|---|---|
| main (0.1.71), machine otherwise idle | 1 | 540 ns | 410 ns | 410 ns |
| main | 2 | 546 ns | 411 ns | 410 ns |
| main | 3 | 438 ns | 408 ns | 408 ns |
| first cut (`Scope` = two `Vec`s by value), idle | 1 | 553 ns | 434 ns | 433 ns |
| first cut | 2 | 472 ns | 426 ns | 426 ns |
| **interleaved, another arc building on the box (load ~2.5)** | | | | |
| main | i1 | 549 ns | 412 ns | 411 ns |
| shipped (`Scope` = `Option<Arc<Chain>>`) | i1 | 552 ns | 420 ns | 421 ns |
| main | i2 | 526 ns | 413 ns | 410 ns |
| shipped | i2 | 541 ns | 411 ns | 413 ns |

The first cut cost a real +16–24 ns (~5 %): a `Scope` of two `Vec`s moved through the
async state machine and cloned into every invocation. Making the empty chain a null handle
took most of it back: interleaved under identical load the shipped tree sits 0–10 ns
(≤ 2.5 %) above main, inside the run-to-run band. What remains is plausibly the key itself
— `CacheKey` grew from 40 to 48 bytes and a hit hashes it twice (`get`, then `get_mut`) —
and was not chased further. Not the kind of number the ~2000× scar is about — that one came
from an added *source*, which this arc adds none of — but it is the number the brief asked
for, and it was worth the second cut.
The bench is not committed; it is twenty lines against the
public API and the table above is what it printed.

**0.1.75, the four faces and the chain clock** — the same bench (one cacheable `FnEndpoint`,
warmed, 200 000 re-issues × 3 rounds, release, `futures::block_on`, M-series laptop, load
~2.4–3.4), main (`d99a180`, a detached worktree) and the branch interleaved three times each:

| tree | run | round 0 | round 1 | round 2 |
|---|---|---|---|---|
| main (0.1.74) | 1 | 423 ns | 306 ns | 308 ns |
| branch | 1 | 306 ns | 304 ns | 305 ns |
| main | 2 | 319 ns | 305 ns | 304 ns |
| branch | 2 | 300 ns | 298 ns | 298 ns |
| main | 3 | 316 ns | 311 ns | 311 ns |
| branch | 3 | 300 ns | 299 ns | 300 ns |

0–10 ns *under* main: nothing this arc added is on the cache-hit path. `resolve_in`'s
empty-chain fast path is untouched (the factored walk is taken only by a non-empty chain and
by selection); `now()` is never called on a hit; the scope-name map is written only when a
scoped result is stored.
