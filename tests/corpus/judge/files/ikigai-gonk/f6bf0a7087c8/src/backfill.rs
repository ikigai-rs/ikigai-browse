//! The judge **backfill** — verdicts for the queue that predates the judge (ledger
//! [#696](http://localhost:1060/l/default/item/696), Brian 2026-10-02: yes to backfilling).
//!
//! ```text
//! urn:iki:gonk:judge:backfill   Source  where the run stands: judged, already judged, could
//!                                       not judge, failed, remaining, the rate (text or JSON)
//!                               Sink    content=start | content=stop
//! ```
//!
//! `ikigai-browse` 0.16.0 judges only the serious findings a review pass MINTS, so every
//! finding queued before it carries no verdict and the Queue can only order it as unjudged.
//! `urn:repo:{repo}:judge-finding:{id}` judges one queued finding on demand and archives the
//! verdict exactly as a pass does. This is the job that walks the queue through it.
//!
//! # ★ The rules, each one the brief's
//!
//! - **Started by an operator, never on start.** A Sink (`content=start`) begins one run; a
//!   restart of this server begins nothing. The Sink requires what a derivation requires — a
//!   browse read and a net grant — so a caller who could not make the judge spend cannot make
//!   this spend either. The socket door's root can; the anonymous HTTP caller cannot.
//! - **Bounded.** A run is the serious pending findings that exist WHEN IT STARTS (the set
//!   `gonk.queue.serious` names, read through each root's findings resource), walked once.
//!   What a pass mints meanwhile is judged by that pass.
//! - **One call at a time, Exists first.** Each finding is asked with Exists — "is there a
//!   verdict under the configured judge's tag?" — which never calls the model, so a finding
//!   already judged costs nothing; only then the Source, which judges and archives. A re-run
//!   therefore repays nothing it already paid for, and that IS the resume: stop it, start it,
//!   and it walks past everything judged.
//! - **It yields to review passes.** The peer is one model with two users, and the review
//!   queue's passes run one at a time. Before each finding the run waits while a pass is in
//!   flight, or while the ARMED queue still has requests waiting, so a pass is delayed by at
//!   most the one judge call already in flight. ★ "A pass" is ANY review Source through the
//!   browse family, not only the queue's: the page's Review button calls browse through the
//!   `/k/` adapter, past [`crate::trigger::Activity::begin`], so the family's overlay counts
//!   every review in flight ([`crate::browse::CachedReads::observing_reviews`], ledger #702
//!   item 4). ⚠ It does NOT yield to an explain or a PR review, which spend the same model
//!   and are not review passes.
//! - **`status: cannot` is counted, never retried in a loop.** browse answers a finding whose
//!   reviewed version cannot be recovered as a value; the run records the reason, moves on, and
//!   asks again only on the next run. The Queue reads those reasons through this resource and
//!   orders those findings after the unsure, saying why ([`crate::verdict::Standing`]).
//! - **Under a grant the host scopes to exactly this**: the browse read and `urn:cap:net:` the
//!   mounted peer's host — the same two the reviewer's grant carries for the same reason, and
//!   nothing else: no store write (browse archives the verdict itself), no annotate, no `gh`.
//! - **No `provider=`.** browse's `selectable()` leaves the judge out, so naming it would be
//!   Denied; omitted, judge-finding asks the configured judge (`gonk.review.judge`). With the
//!   judge OFF the run refuses to start: judge-finding would fall back to the REVIEW tier, and
//!   an operator who switched the judge off did not ask for that.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact,
    Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Space, Verb,
};
use serde_json::{json, Value};

use crate::config::QueuePolicy;
use crate::trigger::{Activity, Trigger};

/// `urn:iki:gonk:judge:backfill` — the run, as a resource.
pub const BACKFILL: &str = "urn:iki:gonk:judge:backfill";

/// The Sink's two words.
pub const START: &str = "start";
/// See [`START`].
pub const STOP: &str = "stop";

const JSON: &str = "application/json";
const TEXT: &str = "text/plain";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// How long a yielding run sleeps before it looks again.
const YIELD_EVERY: Duration = Duration::from_secs(2);

/// The backfill: what it may walk, under what, and where the run stands.
pub struct Backfill {
    roots: Vec<String>,
    policy: QueuePolicy,
    judge: Option<String>,
    scopes: Vec<String>,
    activity: Arc<Activity>,
    review: Option<Trigger>,
    pause: Duration,
    hub: OnceLock<Weak<Kernel>>,
    armed: AtomicBool,
    stop: AtomicBool,
    run: Mutex<Run>,
}

/// Where one run stands — what the Source answers.
#[derive(Debug, Clone, Default)]
pub struct Run {
    /// `idle` (never run in this process), `listing`, `running`, `yielding`, `stopping`,
    /// `stopped`, `done` or `failed`.
    pub phase: &'static str,
    /// When this run began, in milliseconds since the epoch.
    pub started_ms: Option<u64>,
    /// When it ended.
    pub ended_ms: Option<u64>,
    /// The serious pending findings the run set out to walk.
    pub total: usize,
    /// How many it has walked.
    pub done: usize,
    /// Judged by this run (a model call archived a verdict).
    pub judged: usize,
    /// Already carried a verdict under this judge's tag: no call.
    pub already: usize,
    /// Answered `cannot`, this run.
    pub cannot_now: usize,
    /// Failed outright (a transport error, a refusal), this run.
    pub failed: usize,
    /// Model calls made.
    pub calls: usize,
    /// The time spent in them, in milliseconds.
    pub call_ms: u64,
    /// Time spent waiting for review passes.
    pub yielded_ms: u64,
    /// The judge's tag, from the first judged answer.
    pub tag: Option<String>,
    /// The last failure, said.
    pub last_error: Option<String>,
    /// Roots whose findings could not be listed, with why.
    pub unreadable: Vec<(String, String)>,
    /// Every finding a judge could not judge, by id, with the reason it gave — kept across
    /// runs, and dropped the moment one is judged.
    pub unjudgeable: BTreeMap<String, String>,
}

impl Backfill {
    /// A backfill over `roots`, judging the findings `policy` calls serious with `judge` (the
    /// configured `gonk.review.judge`, `None` when off), under exactly `scopes`.
    #[must_use]
    pub fn new(
        roots: Vec<String>,
        policy: QueuePolicy,
        judge: Option<String>,
        scopes: Vec<String>,
        activity: Arc<Activity>,
        review: Option<Trigger>,
    ) -> Backfill {
        Backfill {
            roots,
            policy,
            judge,
            scopes,
            activity,
            review,
            pause: YIELD_EVERY,
            hub: OnceLock::new(),
            armed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            run: Mutex::new(Run {
                phase: "idle",
                ..Run::default()
            }),
        }
    }

    /// The same, sleeping `pause` between looks while it yields — for a test.
    #[must_use]
    pub fn with_pause(mut self, pause: Duration) -> Backfill {
        self.pause = pause;
        self
    }

    /// Hand the backfill the kernel it issues through, once there is one, and whether the
    /// review queue was armed (a waiting request is then a pass about to run, and the run
    /// yields to it; unarmed, a waiting request waits for a person, and yielding to it would
    /// starve the run for nothing).
    pub fn attach(&self, hub: &Arc<Kernel>, armed: bool) {
        let _ = self.hub.set(Arc::downgrade(hub));
        self.armed.store(armed, Ordering::SeqCst);
    }

    /// The space that serves [`BACKFILL`].
    pub fn space(self: &Arc<Self>) -> Arc<dyn Space> {
        Arc::new(EndpointSpace::new().bind(
            Exact::new(BACKFILL),
            BackfillEndpoint {
                backfill: Arc::clone(self),
            },
        ))
    }

    /// Where the run stands.
    pub fn status(&self) -> Run {
        self.run.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn with_run(&self, change: impl FnOnce(&mut Run)) {
        change(&mut self.run.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Begin one run on a thread of its own, or say why not. Starting while a run is live is
    /// not an error: the answer is the live run.
    ///
    /// # Errors
    ///
    /// When the judge is off, or there is no kernel to issue through yet.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        if self.judge.is_none() {
            return Err(Error::Unavailable(
                "the judge is off (gonk.review.judge = \"off\"), so there is nothing to \
                 backfill with: judge-finding would fall back to the REVIEW tier, which is not \
                 what switching the judge off asked for"
                    .to_string(),
            ));
        }
        let hub = self
            .hub
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| Error::Unavailable("the backfill has no kernel yet".to_string()))?;
        {
            let mut run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(run.phase, "listing" | "running" | "yielding" | "stopping") {
                return Ok(());
            }
            let unjudgeable = std::mem::take(&mut run.unjudgeable);
            *run = Run {
                phase: "listing",
                started_ms: Some(now_ms()),
                unjudgeable,
                ..Run::default()
            };
        }
        self.stop.store(false, Ordering::SeqCst);
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("gonk-judge-backfill".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().build();
                match runtime {
                    Ok(runtime) => runtime.block_on(me.walk(hub)),
                    Err(e) => me.with_run(|run| {
                        run.phase = "failed";
                        run.last_error = Some(format!("no runtime for the run: {e}"));
                        run.ended_ms = Some(now_ms());
                    }),
                }
            })
            .map_err(|e| Error::Unavailable(format!("could not start the backfill: {e}")))?;
        Ok(())
    }

    /// Ask a live run to stop after the call in flight.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.with_run(|run| {
            if matches!(run.phase, "listing" | "running" | "yielding") {
                run.phase = "stopping";
            }
        });
    }

    /// Whether a review pass should go first: one is in flight — a queued pass, or a review
    /// any other door started (the page's Review button, a person on the socket), which the
    /// browse family's overlay counts (ledger #702 item 4) — or the armed queue has requests
    /// waiting for the reactor.
    fn review_first(&self) -> bool {
        let passes = self.activity.snapshot();
        if passes.in_flight_since_ms.is_some() || passes.reviews_in_flight > 0 {
            return true;
        }
        self.armed.load(Ordering::SeqCst)
            && self
                .review
                .as_ref()
                .is_some_and(|queue| crate::trigger::pending(queue) > 0)
    }

    /// The run itself.
    async fn walk(self: Arc<Self>, hub: Arc<Kernel>) {
        let cap = Capability::scoped(self.scopes.clone());
        // The work: every serious pending finding, per root, as it stands now.
        let mut work: Vec<(String, String)> = Vec::new();
        for root in &self.roots {
            match pending_serious(&hub, &cap, root, &self.policy).await {
                Ok(ids) => work.extend(ids.into_iter().map(|id| (root.clone(), id))),
                Err(why) => self.with_run(|run| run.unreadable.push((root.clone(), why))),
            }
        }
        self.with_run(|run| {
            run.total = work.len();
            if run.phase == "listing" {
                run.phase = "running";
            }
        });
        for (root, id) in work {
            // Yield to a review pass, and stop when asked — checked before every finding.
            loop {
                if self.stop.load(Ordering::SeqCst) {
                    self.with_run(|run| {
                        run.phase = "stopped";
                        run.ended_ms = Some(now_ms());
                    });
                    return;
                }
                if !self.review_first() {
                    break;
                }
                self.with_run(|run| run.phase = "yielding");
                let waited = Instant::now();
                std::thread::sleep(self.pause);
                let ms = waited.elapsed().as_millis() as u64;
                self.with_run(|run| run.yielded_ms += ms);
            }
            self.with_run(|run| {
                if run.phase == "yielding" {
                    run.phase = "running";
                }
            });
            self.one(&hub, &cap, &root, &id).await;
        }
        self.with_run(|run| {
            run.phase = if self.stop.load(Ordering::SeqCst) {
                "stopped"
            } else {
                "done"
            };
            run.ended_ms = Some(now_ms());
        });
    }

    /// One finding: Exists first (free), then the judge (one call), and the answer recorded.
    async fn one(&self, hub: &Kernel, cap: &Capability, root: &str, id: &str) {
        let iri = format!("urn:repo:{root}:judge-finding:{id}");
        let exists = issue(hub, cap, Verb::Exists, &iri, &[]).await;
        match exists {
            Ok(answer) if String::from_utf8_lossy(&answer).trim() == "true" => {
                self.with_run(|run| {
                    run.already += 1;
                    run.done += 1;
                    run.unjudgeable.remove(id);
                });
                return;
            }
            Ok(_) => {}
            Err(e) => {
                self.with_run(|run| {
                    run.failed += 1;
                    run.done += 1;
                    run.last_error = Some(format!("{iri} (Exists): {e}"));
                });
                return;
            }
        }
        let began = Instant::now();
        let answer = issue(hub, cap, Verb::Source, &iri, &[("as", JSON)]).await;
        let ms = began.elapsed().as_millis() as u64;
        self.with_run(|run| {
            run.done += 1;
            match answer.map_err(|e| e.to_string()).and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
            }) {
                Err(e) => {
                    run.failed += 1;
                    run.last_error = Some(format!("{iri}: {e}"));
                }
                Ok(answer) => {
                    let calls = answer.get("calls").and_then(Value::as_u64).unwrap_or(0) as usize;
                    run.calls += calls;
                    if calls > 0 {
                        run.call_ms += ms;
                    }
                    if let Some(tag) = answer.get("tag").and_then(Value::as_str) {
                        run.tag.get_or_insert_with(|| tag.to_string());
                    }
                    match answer.get("status").and_then(Value::as_str) {
                        Some("judged") => {
                            run.judged += 1;
                            run.unjudgeable.remove(id);
                        }
                        Some("archived") => {
                            run.already += 1;
                            run.unjudgeable.remove(id);
                        }
                        Some("cannot") => {
                            run.cannot_now += 1;
                            let why = answer
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("no reason given");
                            run.unjudgeable.insert(id.to_string(), why.to_string());
                        }
                        other => {
                            run.failed += 1;
                            run.last_error = Some(format!("{iri}: status {other:?}"));
                        }
                    }
                }
            }
        });
    }
}

impl Run {
    /// Findings the run has not reached yet.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.done)
    }

    /// Findings walked per minute of the run's wall clock so far, yielding included.
    #[must_use]
    pub fn per_minute(&self, now: u64) -> Option<f64> {
        let started = self.started_ms?;
        let elapsed = self.ended_ms.unwrap_or(now).saturating_sub(started);
        (elapsed > 0 && self.done > 0).then(|| self.done as f64 * 60_000.0 / elapsed as f64)
    }

    /// The mean time one model call took.
    #[must_use]
    pub fn mean_call_ms(&self) -> Option<u64> {
        (self.calls > 0).then(|| self.call_ms / self.calls as u64)
    }

    /// The run as JSON.
    #[must_use]
    pub fn json(&self, judge: Option<&str>, now: u64) -> Value {
        json!({
            "resource": BACKFILL,
            "phase": self.phase,
            "judge": judge,
            "tag": self.tag,
            "total": self.total,
            "done": self.done,
            "remaining": self.remaining(),
            "judged": self.judged,
            "already_judged": self.already,
            "cannot": self.cannot_now,
            "failed": self.failed,
            "calls": self.calls,
            "mean_call_ms": self.mean_call_ms(),
            "per_minute": self.per_minute(now),
            "yielded_ms": self.yielded_ms,
            "started_ms": self.started_ms,
            "ended_ms": self.ended_ms,
            "last_error": self.last_error,
            "unreadable": self.unreadable.iter().map(|(root, why)| json!({"repo": root, "why": why})).collect::<Vec<_>>(),
            "unjudgeable": self.unjudgeable,
        })
    }

    /// The run in one sentence.
    #[must_use]
    pub fn sentence(&self, now: u64) -> String {
        if self.phase == "idle" {
            return format!(
                "The judge backfill has not run in this process. Start it with `sink \
                 {BACKFILL} content={START}` (one call at a time, yielding to review passes)."
            );
        }
        let mut out = format!(
            "Judge backfill {}: {} judged, {} already judged, {} could not be judged, {} \
             failed, {} remaining of {}",
            self.phase,
            self.judged,
            self.already,
            self.cannot_now,
            self.failed,
            self.remaining(),
            self.total
        );
        if let Some(mean) = self.mean_call_ms() {
            out.push_str(&format!(
                "; {} model call{} at {:.1} s each",
                self.calls,
                if self.calls == 1 { "" } else { "s" },
                mean as f64 / 1000.0
            ));
        }
        if let Some(rate) = self.per_minute(now) {
            out.push_str(&format!("; {rate:.1} findings a minute"));
        }
        if self.yielded_ms > 0 {
            out.push_str(&format!(
                "; {} s yielded to review passes",
                self.yielded_ms / 1000
            ));
        }
        if let Some(error) = &self.last_error {
            out.push_str(&format!(". Last failure: {error}"));
        }
        for (root, why) in &self.unreadable {
            out.push_str(&format!(". {root} could not be listed: {why}"));
        }
        out.push('.');
        out
    }
}

/// The serious pending findings of one root, by id, in the findings resource's own order.
async fn pending_serious(
    hub: &Kernel,
    cap: &Capability,
    root: &str,
    policy: &QueuePolicy,
) -> std::result::Result<Vec<String>, String> {
    let iri = crate::queue::findings_iri(root);
    let bytes = issue(
        hub,
        cap,
        Verb::Source,
        &iri,
        &[("as", JSON), ("state", "pending")],
    )
    .await
    .map_err(|e| e.to_string())?;
    let rows: Vec<Value> = serde_json::from_slice(&bytes).map_err(|e| format!("{iri}: {e}"))?;
    Ok(rows
        .iter()
        .filter(|row| crate::queue::rated(row).is_some_and(|word| policy.is_serious(word)))
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

async fn issue(
    hub: &Kernel,
    cap: &Capability,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<Vec<u8>> {
    let target = Iri::parse(iri).map_err(|e| Error::Endpoint(format!("{iri}: {e}")))?;
    let mut request = Request::new(verb, target);
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    hub.issue(request, cap).await.map(|answer| answer.bytes)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What a page needs from the run: the findings a judge could not judge, with why — read
/// through the kernel under the CALLER's capability, like the review depth. Empty when the
/// backfill is not bound here (no judge to backfill with) or the caller may not read it.
pub async fn unjudgeable(inv: &Invocation<'_>) -> BTreeMap<String, String> {
    let Ok(target) = Iri::parse(BACKFILL) else {
        return BTreeMap::new();
    };
    let request =
        Request::new(Verb::Source, target).with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()));
    let Ok(answer) = inv.issue(request).await else {
        return BTreeMap::new();
    };
    serde_json::from_slice::<Value>(&answer.bytes)
        .ok()
        .and_then(|status| {
            serde_json::from_value::<BTreeMap<String, String>>(status.get("unjudgeable")?.clone())
                .ok()
        })
        .unwrap_or_default()
}

/// The page's one sentence about the run, when it has run in this process — `None` when it
/// has not, or is not bound, or the caller may not read it.
pub async fn page_sentence(inv: &Invocation<'_>) -> Option<String> {
    let request = Request::new(Verb::Source, Iri::parse(BACKFILL).ok()?)
        .with_arg("as", ArgRef::Inline(TEXT.as_bytes().to_vec()));
    let answer = inv.issue(request).await.ok()?;
    let sentence = String::from_utf8(answer.bytes).ok()?;
    (!sentence.starts_with("The judge backfill has not run")).then_some(sentence)
}

/// `urn:iki:gonk:judge:backfill`.
struct BackfillEndpoint {
    backfill: Arc<Backfill>,
}

#[async_trait]
impl Endpoint for BackfillEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => {}
            Verb::Sink => {
                let word = inv.inline_str("content")?.trim();
                match word {
                    START => self.backfill.start()?,
                    STOP => self.backfill.stop(),
                    other => {
                        return Err(Error::InvalidArgument {
                            name: "content".to_string(),
                            detail: format!(
                                "`{other}`: `{START}` begins one run over the serious pending \
                                 findings, `{STOP}` ends it after the call in flight"
                            ),
                        })
                    }
                }
            }
            other => {
                return Err(Error::Endpoint(format!(
                    "{BACKFILL} answers Source and Sink, not {other:?}"
                )))
            }
        }
        let run = self.backfill.status();
        let now = inv.now().map(|t| t.as_millis()).unwrap_or_else(now_ms);
        if matches!(inv.inline_str("as"), Ok(JSON)) {
            return Ok(Representation::new(
                ReprType::new(JSON).with_param("charset", "utf-8"),
                run.json(self.backfill.judge.as_deref(), now)
                    .to_string()
                    .into_bytes(),
            ));
        }
        Ok(Representation::new(
            ReprType::new(TEXT).with_param("charset", "utf-8"),
            run.sentence(now).into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-judge-backfill"
    }

    fn describe(&self) -> Description {
        let face = || {
            ArgSpec::new("as")
                .optional()
                .class(XSD_STRING)
                .one_of([TEXT, JSON])
                .default_value(TEXT)
                .summary("the face: a sentence, or the numbers")
        };
        Description::new("gonk-judge-backfill")
            .title("Judge the queue that predates the judge, one finding at a time")
            .summary(
                "Walk the serious pending findings that exist when a run starts and judge each \
                 through urn:repo:{repo}:judge-finding:{id} with the configured judge \
                 (gonk.review.judge): Exists first, so a finding already judged under that \
                 judge's tag costs no model call; then the judgment, archived on the finding. \
                 One call at a time, yielding while a review pass is in flight or the armed \
                 review queue has requests waiting. A finding the judge cannot judge is \
                 counted with its reason and asked again only on the next run. Started by an \
                 operator (Sink content=start), never on server start; content=stop ends it \
                 after the call in flight, and a later start resumes past everything judged. \
                 Source: where the run stands (judged, already judged, could not judge, \
                 failed, remaining, model calls and their mean time, findings a minute).",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("where the run stands")
                    .requires(ikigai_browse::CAP_WILDCARD)
                    .input(face())
                    .output(TEXT)
                    .output(JSON),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("start a run, or stop the one in progress")
                    .requires(ikigai_browse::CAP_WILDCARD)
                    .requires(crate::grants::CAP_NET_ANY)
                    .input(
                        ArgSpec::new("content")
                            .class(XSD_STRING)
                            .one_of([START, STOP])
                            .summary("start: begin one run; stop: end it after the call in flight"),
                    )
                    .input(face())
                    .output(TEXT)
                    .output(JSON),
            )
            .verb(Verb::Meta)
    }
}
