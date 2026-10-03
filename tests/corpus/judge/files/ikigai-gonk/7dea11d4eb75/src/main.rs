//! `ikigai-gonk` — the process: flags, the store open, three doors, the banner.
//!
//! The composition lives in the library ([`ikigai_gonk::compose`]) so the conformance walk
//! holds the kernel this `main` serves; the doors and what each grants are
//! [`ikigai_gonk::doors`], and the HTML face is [`ikigai_gonk::web`].

use std::sync::Arc;

use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::config::{self, Command, Homes};
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::watch::Watched;
use ikigai_gonk::{browse, compose_with, doors, mount, queue, quic, trigger, watch, web};
// ★ No `SharerWrites` here any more, and that absence is the shape of ledger #282's fix: the
// promise is not a type this file names, it is `browse::Graph` read as a declaration.
use ikigai_store::{DurableStore, StoreConfig};
use ikigai_time::{JobRegistry, Schedule, ThreadTimer};

fn main() {
    let command = config::parse_args(std::env::args().skip(1))
        .unwrap_or_else(|e| fail(&format!("{e}\n\n{}", config::USAGE)));
    match command {
        Command::Help => print!("{}", config::USAGE),
        Command::Grants { ledger, authority } => {
            let tokens = match &ledger {
                Some(ledger) => grants::grants_for(ledger, authority),
                None => grants::browse_graph_grants(authority),
            }
            .unwrap_or_else(|e| fail(&e));
            println!(
                "{}",
                serde_json::to_string_pretty(&tokens).expect("strings serialize")
            );
        }
        Command::ReviewRequest { repo, path, flags } => review_request(&repo, &path, &flags),
        Command::ClientAdd {
            name,
            cert,
            ledgers,
            browse_graph,
            force,
        } => client_add(&name, cert.as_deref(), &ledgers, browse_graph, force),
        Command::PasskeyInvite {
            name,
            ledgers,
            browse_graph,
            force,
            minutes,
            flags,
        } => passkey_invite(&name, &ledgers, browse_graph, force, minutes, &flags),
        Command::Serve(flags) => serve(&flags),
    }
}

fn serve(flags: &config::Flags) -> ! {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let (config_path, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    // Not prefixed with the config path: a setting may have come from a flag, and a message
    // that names the file sends the operator to edit a line that is not there.
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| {
        fail(&format!(
            "{e}\n  (config read from {})",
            config_path.display()
        ))
    });
    let http_grants = grants::grants_for_all(&settings.http_ledgers, Authority::Write)
        .unwrap_or_else(|e| fail(&e));

    // Authority is decided before the store opens: a broken authority file stops the server
    // instead of degrading it. `grants.json` feeds BOTH identity doors (certificates and
    // passkeys), so its broad-token check runs whether or not QUIC opens.
    let layout = quic::Layout::in_config_home(&homes.config);
    quic::read_grants(&layout.grants_json())
        .and_then(|grants| quic::check_grants(&grants))
        .unwrap_or_else(|e| fail(&e));
    let quic_door = quic::open_door(&layout, settings.quic).unwrap_or_else(|e| fail(&e));
    // The passkey file is read here too, so an unparsable `passkeys` block stops the server
    // rather than failing every sign-in.
    identity::read_passkeys(&layout).unwrap_or_else(|e| fail(&e));
    // And the render rules, for the same reason: a rule table this server cannot obey is a
    // setting that would look exactly like one in effect. Read once, checked once, then
    // SERVED — `urn:iki:gonk:render-rules` is what the renderer resolves, so what a page
    // acted on is what a person can read back.
    let render_rules = read_render_rules(&layout).unwrap_or_else(|e| fail(&e));

    // The browse roots' watchers start BEFORE the space is built, and the set they actually
    // got decides which roots' reads may be cached. Fail closed, in that order: a root whose
    // platform watcher refuses is served live, exactly as `ikigai-browse` declares it.
    let (root_watch, unwatched) = watch::RootWatch::start(&settings.browse_roots);
    for root in &unwatched {
        eprintln!(
            "ikigai-gonk: browse root `{}` will not be watched ({}) — its reads are served \
             live and uncached",
            root.name, root.reason
        );
    }

    // Hold the store. A second holder is refused by RocksDB — and the fix is topology.
    let store_path = StoreConfig::load(Some("gonk"))
        .unwrap_or_else(|e| fail(&e.to_string()))
        .path;
    // ★ The handle leaves this crate ONLY when something else in this process needs it. The
    // browse annotation family takes an `Arc<Store>`, which is how its quads land in the
    // dataset the ledger lives in — the whole point of composing them here.
    //
    // ★★ And when it leaves, gonk SAYS WHERE THAT HOLDER WRITES. `SharerWrites` is a promise
    // to `ikigai-store`, which answers it per read: a scoped read of a graph outside the
    // promise is cacheable again, and every read that can see the whole dataset stays
    // `Expiry::Always`. Without the promise the store must assume the worst and make EVERY
    // read `Expiry::Always` — and expiry propagates, so that would de-cache every ledger read
    // as a side effect of adding a browse face (`tests/browse.rs` prints both numbers).
    //
    // ★ The promise is DERIVED, not transcribed. `browse::Graph` is gonk's choice of where
    // the family's quads land, made once, on the line below; `sharer_writes()` is that choice
    // read as a declaration and `browse::wire` is the same choice read as a mount. Until
    // `ikigai-browse` 0.4.0's `Mount::graph` this line said `only_the_default_graph()` because
    // a person had gone and read browse's three writers — a fact about a dependency's
    // internals, re-typed here, which is what ledger #282 filed. ⚠ A FALSE promise is silent,
    // unbounded staleness (reads of a graph the sharer does write, cached against threads its
    // write never cuts), and `tests/browse.rs` fingerprints the reserved graphs around a real
    // browse WRITE and a real browse READ to keep the derivation honest at both ends.
    let browse_graph = browse::Graph::chosen();
    let opened = if settings.browse_roots.is_empty() {
        DurableStore::open(&store_path).map(|store| (store, None))
    } else {
        DurableStore::open_shared_declaring(&store_path, browse_graph.sharer_writes())
            .map(|(store, handle)| (store, Some(handle)))
    };
    let (store, handle) = opened.unwrap_or_else(|e| {
        fail(&format!(
            "cannot hold the store at {}: {e}\n  gonk is the one process on a machine that opens the dataset. If \
             the holder is another gonk, this one has nothing to add — reach that one through its socket. If it \
             is anything else (an `ikigai` with `store = true`), stop it and give it these two lines, naming the \
             socket this gonk serves on:\n    \
             mount = \"prefer urn:iki:store:={sock}\"\n    mount = \"prefer urn:iki:ledger:={sock}\"",
            store_path.display(),
            sock = settings.socket.display()
        ))
    });
    let explains = settings.explains();
    // ★★ Obligation 1, checked before a single door opens. The choice above confines browse's
    // reads to one graph; quads it wrote into another are still on disk and no longer visible
    // to any of them, and NOTHING anywhere reports that — an empty annotation list and an
    // empty archive are legitimate answers. So this server counts them and says so, in
    // browse's own numbers.
    if let Some(handle) = handle.as_deref() {
        warn_unmigrated(handle, &browse_graph, &store_path, &settings.socket);
    }
    let browse = handle.map(|handle| {
        browse::wire(
            settings.browse_roots.clone(),
            handle,
            root_watch.watched(),
            explains.then_some(&settings.explain),
            // The SAME value the declaration above was derived from — not a second
            // `Graph::chosen()`, which would be a second decision.
            &browse_graph,
        )
    });
    let browse_line = browse_line(&settings.browse_roots, root_watch.watched(), &browse_graph);
    let (browse_space, style) = match browse {
        Some(wired) => (
            Some(Arc::new(wired.space) as Arc<dyn ikigai_core::Space>),
            Some(wired.style),
        ),
        None => (None, None),
    };
    // The mounts are built before the kernel and dialled by neither: `crate::mount` dials on
    // first use, so a peer that is down costs this server nothing at startup and the ledger
    // is served whether or not a model can be reached.
    let mounted: Vec<Arc<dyn ikigai_core::Space>> =
        settings.mounts.iter().map(mount::space).collect();
    let mount_line = mount_line(&settings, explains);

    // The backup rotation, and the timer that fires it. Both are built before the kernel
    // because the kernel binds the family; the timer is handed the kernel afterwards.
    let backup_settings = Arc::new(backup::Settings {
        dir: settings.backup.dir.clone(),
        keep: settings.backup.keep,
        every: settings.backup.every,
        store_path: store_path.clone(),
    });
    // ★ The backup fires under exactly `backup::JOB_SCOPES` and nothing else, so a scheduled
    // backup has the authority to read every graph and write the rotation — and no authority
    // to write a single quad. Since ikigai-time 0.4.0 (ledger #79) the registry's capability
    // is a CEILING and each job records its own; a tick fires under `ceiling.clamp(job)`.
    // Both are `JOB_SCOPES` here, so the clamp is exactly `JOB_SCOPES`. `Capability::root()`
    // is still the registry's default ceiling and would be silently broader than anything this
    // server hands any caller, which is why the ceiling is set as well as the job's own.
    let jobs = settings.backup.every.map(|_| {
        JobRegistry::new(Arc::new(ThreadTimer), Arc::new(ikigai_core::SystemClock))
            .with_capability(ikigai_core::Capability::scoped(backup::JOB_SCOPES))
    });
    // The review queue, when one is configured. `prepare` creates the tree 0700 up front for
    // the reason `ikigai-intray`'s own watcher does: a directory that appears later races
    // whatever writes into it, and `review request` writes with no server running.
    //
    // ★ The reviewer's SCOPES are resolved here, before the kernel exists, because a grant
    // that `grants.json` cannot honour must stop this server rather than arm a reactor that
    // dead-letters everything. The reactor itself needs the kernel, so it is installed after
    // the hub below — the two halves of arming, in the only order they can happen.
    let reviewer = match &settings.review {
        Some(t) => resolve_reviewer(t, &layout),
        None => None,
    };
    let activity = Arc::new(trigger::Activity::default());
    let trigger_spaces = match &settings.review {
        Some(t) => {
            trigger::prepare(t).unwrap_or_else(|e| fail(&e));
            trigger::space(
                t,
                Arc::clone(&activity),
                reviewer.is_some(),
                settings.queue.clone(),
            )
        }
        None => Vec::new(),
    };
    let hub = Arc::new(compose_with(
        store,
        browse_space,
        mounted,
        trigger_spaces,
        Some(Backups {
            settings: Arc::clone(&backup_settings),
            jobs: jobs.clone(),
        }),
    ));

    // ★ The Queue's serious set, checked against the finding contract THIS kernel binds
    // (ledger #496): a word the contract does not declare stops this server naming both
    // lists, rather than serving a queue that asks about nothing. The same shape as
    // `gonk.review.arm` without a space — a line for a queue this server does not serve is
    // a refusal, not a no-op.
    let queue_line = if settings.browse_roots.is_empty() {
        if settings.queue.configured {
            fail(
                "gonk.queue.serious is set and no gonk.browse.root is configured: there is no \
                 finding contract to check it against and no queue for it to narrow",
            );
        }
        "no gonk.browse.root, so no findings and no Queue page".to_string()
    } else {
        let declared = queue::check_serious(&hub, &settings.queue).unwrap_or_else(|e| fail(&e));
        queue_line(&settings, &declared)
    };

    // ★★ ARMING, and it is deliberately the last thing before the doors: the reviewer's
    // grant is checked against the CONTRACT of the review this kernel actually binds, and a
    // grant that is short of it — or that carries the publish token — stops this server.
    let review_line = match (&settings.review, &reviewer) {
        (Some(queue), Some(scopes)) => {
            let host = mount_host(&settings);
            let probe = settings
                .browse_roots
                .first()
                .map(|(name, _)| trigger::review_probe_iri(name))
                .unwrap_or_else(|| {
                    fail(
                        "gonk.review.arm is set and no gonk.browse.root is configured: there \
                         is no repository to review, so no pass could ever resolve",
                    )
                });
            trigger::check_reviewer(&hub, &probe, &host, scopes).unwrap_or_else(|e| fail(&e));
            trigger::arm(queue, Arc::clone(&hub), scopes).unwrap_or_else(|e| fail(&e));
            armed_line(queue, scopes.len(), &host)
        }
        _ => {
            // Not armed: make sure nothing is left behind that a reactor would fire. The
            // handler file lives in the tree a dropper writes into, and an unarmed gonk that
            // left one there would be a loaded gun for the next process that is armed.
            if let Some(queue) = &settings.review {
                trigger::set_handler(queue, false).unwrap_or_else(|e| fail(&e));
            }
            review_line(&settings, &layout)
        }
    };

    let backup_line = start_backups(&hub, &jobs, &backup_settings, &settings.socket);

    // Both watchers, now that there is a kernel to cut threads on. `urn:repo:style` declares
    // a thread per `a11y.toml` candidate and browse ships the watch for them; the roots'
    // threads are this server's own (`crate::watch`), declared only for what is watched.
    if let Some(style) = style {
        if let Err(e) = style.spawn(Arc::clone(&hub)) {
            // Not fatal — the sheet still serves — but the symptom (an edit that never
            // lands) is silent, so say it once.
            eprintln!("ikigai-gonk: urn:repo:style will not follow a11y.toml edits: {e}");
        }
    }
    root_watch.spawn(Arc::clone(&hub));

    // The socket door. Bound only after the store is held, so it can never replace the
    // socket of a gonk that is still running.
    if let Some(parent) = settings.socket.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| fail(&format!("creating {}: {e}", parent.display())));
    }
    let (socket, door) = (
        settings.socket.clone(),
        doors::door_kernel(Arc::clone(&hub)),
    );
    std::thread::spawn(move || {
        let error = ikigai_ipc::serve(door, &socket).err();
        fail(&format!(
            "the socket door at {} stopped: {error:?}",
            socket.display()
        ))
    });

    let quic_line = match quic_door {
        quic::QuicDoor::Off(why) => why,
        quic::QuicDoor::Open {
            addr,
            identity,
            trusted,
            enrolled,
        } => {
            let (door, minter) = (
                doors::door_kernel(Arc::clone(&hub)),
                quic::minter(layout.clone()),
            );
            let line = format!(
                "udp {addr} — {} trusted certificate(s), {enrolled} enrolled",
                trusted.len()
            );
            std::thread::spawn(move || {
                let error = ikigai_quic::serve(door, addr, &identity, &trusted, minter).err();
                fail(&format!("the QUIC door on {addr} stopped: {error:?}"))
            });
            line
        }
    };

    let runtime =
        tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(&format!("tokio runtime: {e}")));
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(settings.http)
            .await
            .unwrap_or_else(|e| fail(&format!("cannot bind http {}: {e}", settings.http)));
        let port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(settings.http.port());
        let passkeys = Arc::new(Passkeys::new(layout.clone(), port));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: settings.http_ledgers.clone(),
            // The NAMES of the configured roots, for the header's entry point and the page
            // behind it. The paths went to the mount above; a page never needs one.
            browse_roots: settings
                .browse_roots
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            passkeys: Arc::clone(&passkeys),
            rules: Arc::clone(&render_rules),
            queue: settings.queue.clone(),
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        eprintln!(
            "ikigai-gonk {} — holding the store at {}",
            env!("CARGO_PKG_VERSION"),
            store_path.display()
        );
        // ★ The `localhost` form, never the bound IP: WebAuthn refuses an IP address as a
        // relying party, so a page opened at 127.0.0.1 cannot use a passkey at all.
        eprintln!(
            "  http    http://localhost:{port}/ — loopback ({}); anonymous read+write: {}; {} passkey(s)",
            settings.http,
            settings.http_ledgers.join(", "),
            passkeys.enrolled_count()
        );
        eprintln!("  browse  {browse_line}");
        eprintln!("  backup  {backup_line}");
        eprintln!("  llm     {mount_line}");
        eprintln!("  review  {review_line}");
        eprintln!("  queue   {queue_line}");
        eprintln!("  socket  {} — owner only", settings.socket.display());
        eprintln!("  quic    {quic_line}");
        eprintln!(
            "  mount   mount = \"prefer urn:iki:ledger:={}\"  (and the same for urn:iki:store:)",
            settings.socket.display()
        );
        let error = ikigai_web::serve_with_listener(
            http,
            doors::http_cap(doors::HttpDoor {
                anonymous: http_grants,
                port,
                passkeys: Some(passkeys),
            }),
            listener,
            doors::edge_config(),
        )
        .await;
        fail(&format!("the http door stopped: {error:?}"))
    })
}

/// Install the kernel in the job registry and schedule the backup, returning the banner's
/// backup line.
///
/// ★★ **A recurring timer's FIRST tick is one whole interval away, and that is the trap a
/// 24-hour backup on a supervised daemon falls into.** `ThreadTimer` sleeps the interval
/// before it fires, `KeepAlive` restarts this process on any exit, and a machine that
/// reboots (or a binary that is reinstalled) nightly therefore never reaches the first
/// tick — silently, with a schedule that looks correct in every readout. So startup asks
/// the rotation directory when the last backup actually happened and, if that is longer ago
/// than the cadence (or never), schedules a one-shot catch-up a minute out. A restart does
/// not cost a backup, and a restart loop does not spam one either: the catch-up fires only
/// while the newest archive is overdue.
fn start_backups(
    hub: &Arc<ikigai_core::Kernel>,
    jobs: &Option<JobRegistry>,
    settings: &Arc<backup::Settings>,
    socket: &std::path::Path,
) -> String {
    let Some(every) = settings.every else {
        return format!(
            "no schedule (--no-backup or gonk.backup.every = \"off\") — {} is still bound; \
             this process is the only thing that can export the dataset",
            backup::BACKUP
        );
    };
    let Some(jobs) = jobs else {
        return "no schedule — the registry was not built (this is a bug)".to_string();
    };
    jobs.set_resolver(Arc::clone(hub) as Arc<dyn ikigai_resolve::Resolver>);
    if let Err(e) = jobs.schedule_persistent(
        backup::BACKUP.to_string(),
        ikigai_core::Verb::Source,
        Schedule::Every(every),
        true,
        ikigai_core::Capability::scoped(backup::JOB_SCOPES),
    ) {
        // Not fatal: the ledger is served either way, and refusing to start would take the
        // system of record offline over its backup. Loud, because a server that says it is
        // backing up and is not is the failure this whole feature exists to prevent.
        eprintln!("ikigai-gonk: THE BACKUP IS NOT SCHEDULED: {e}");
        return format!("NOT SCHEDULED: {e}");
    }
    let overdue = overdue_by(settings, every);
    let catch_up = if overdue {
        match jobs.schedule(
            backup::BACKUP.to_string(),
            ikigai_core::Verb::Source,
            Schedule::Every(CATCH_UP),
            false,
            ikigai_core::Capability::scoped(backup::JOB_SCOPES),
        ) {
            Ok(_) => ", one due now (catching up in 1m)",
            Err(_) => "",
        }
    } else {
        ""
    };
    scheduled_line(every, &settings.dir, settings.keep, catch_up, socket)
}

/// The banner's backup line once the schedule is in — separate so a test can read it,
/// because **the command in it is one an operator will paste**.
///
/// ⚠ The status command names THIS server's socket. The backup family is bound only behind
/// it: no config-home `mount` claims `urn:iki:gonk:`, so a bare `ikigai -c 'source …'`
/// reaches nothing and HANGS rather than failing (ledger #381, an `ikigai-cli` defect). And
/// the path is the one this process binds, not `~/.ikigai/gonk.sock`, so a host configured
/// differently is told the truth.
fn scheduled_line(
    every: std::time::Duration,
    dir: &std::path::Path,
    keep: usize,
    catch_up: &str,
    socket: &std::path::Path,
) -> String {
    format!(
        "every {} into {} — keep {keep}{catch_up}. \
         `ikigai --connect {} -c 'source {}'` says when the last good one was",
        humanize(every),
        dir.display(),
        socket.display(),
        backup::STATUS,
    )
}

/// How long after startup the catch-up backup fires. Long enough that a restart loop does
/// not take one per restart, short enough that an operator watching a deploy sees it.
const CATCH_UP: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the newest archive on disk is older than one cadence — or there is none.
///
/// Read from the SIDECAR's timestamp rather than the file's mtime: a rotation directory
/// copied or restored from elsewhere keeps the archive's name and contents and loses its
/// mtime, and the name is what the sidecar agrees with.
fn overdue_by(settings: &backup::Settings, every: std::time::Duration) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    match backup::newest_taken_at_ms(&settings.dir) {
        None => true,
        Some(taken) => now.saturating_sub(taken) >= every.as_millis() as u64,
    }
}

/// A duration as the banner reads it.
fn humanize(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        s if s < 3600 => format!("{}m", s / 60),
        s if s % 86_400 == 0 => format!("{}h", s / 3600),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

/// ★ **Say, loudly, when this binary's graph choice and the store's contents disagree.**
///
/// The failure this exists for is the quietest one in the server: a gonk that names a graph,
/// run against a store whose browse quads are in another one, serves an EMPTY archive and an
/// EMPTY annotation list, with no error at any layer. Nothing else in the process can tell —
/// the reads succeed.
///
/// ⚠ It warns rather than refusing to start, and that is a judgement worth stating: the
/// ledger, the doors and the backup are unaffected by this condition, and turning a
/// browse-only data problem into a total outage would be the wrong trade for a server whose
/// job is holding the ledger. The operator sequence in the README puts the migration where it
/// belongs — with the server stopped, between two runs.
///
/// A failure to COUNT (an unreadable store) is reported the same way and is not fatal either:
/// this is a check, and a check that can stop the server is a new way to fail.
fn warn_unmigrated(
    store: &ikigai_store::Store,
    graph: &browse::Graph,
    store_path: &std::path::Path,
    socket: &std::path::Path,
) {
    match browse::unmigrated_quads(store, graph) {
        Ok(0) => {}
        Ok(stranded) => eprint!(
            "{}",
            unmigrated_warning(stranded, graph, store_path, socket)
        ),
        Err(e) => eprintln!("ikigai-gonk: could not check where browse's quads are: {e}"),
    }
}

/// The text [`warn_unmigrated`] prints — separate so a test can read it, because **the
/// commands in it are the operator's only lifeline out of this condition** and a wrong one
/// would be found at the worst moment.
///
/// ⚠ Every IRI here is `as_str()`. `NamedNode`'s `Display` brackets it, so `{iri}` inside
/// `<…>` writes `<<urn:…>>` and `--graph {iri}` hands the migration tool an argument it
/// refuses as "not an IRI". That is not hypothetical: it was written that way first, and
/// `the_migration_warning_names_a_command_that_would_run` is why it did not ship.
fn unmigrated_warning(
    stranded: u64,
    graph: &browse::Graph,
    store_path: &std::path::Path,
    socket: &std::path::Path,
) -> String {
    let Some(iri) = graph.named().map(oxigraph::model::NamedNode::as_str) else {
        // The default-graph arm: browse's quads are stranded in some NAMED graph, which only
        // a rollback produces. Say it without naming a migration command that cannot express
        // the reverse move.
        return format!(
            "ikigai-gonk: ⚠ {stranded} quad(s) ikigai-browse wrote are in a NAMED graph, and \
             this binary reads the default one — the archive and every annotation will read \
             EMPTY. This is a binary rolled back past a graph migration; roll it forward \
             again, or migrate the data back.\n"
        );
    };
    let store = store_path.display();
    let socket = socket.display();
    format!(
        "ikigai-gonk: ⚠⚠ THE BROWSE ARCHIVE IS INVISIBLE — {stranded} quad(s) are outside \
         <{iri}>\n  \
         This binary reads and writes browse's quads in that graph. The quads above are still \
         on disk and are no longer visible to ANY browse read: no annotation, no archived \
         explanation, no review finding, and no error anywhere. New writes land in <{iri}> \
         and the two sets will not join.\n  \
         Stop this server and migrate the store — it holds the RocksDB write lock, so nothing \
         else can:\n    \
         cargo install ikigai-browse --version 0.4.0 --locked --features migrate --bin \
         migrate-annotation-ns\n    \
         migrate-annotation-ns {store} --graph {iri}\n    \
         migrate-annotation-ns {store} --graph {iri} --commit\n  \
         The dry run (no --commit) reports this same count. ⚠ --commit is a one-shot \
         destructive rewrite and restarting does not undo it. Take a backup through this \
         server FIRST, while it is still up — it holds the lock, so nothing else can export \
         the dataset:\n    \
         ikigai --connect {socket} -c 'source urn:iki:gonk:backup'\n"
    )
}

/// The banner's browse line: which roots are served, which of them are watched — because
/// "watched" is exactly "its reads are cached", and an operator reading the banner should be
/// able to tell those apart without reading this source — and WHERE the family's quads land,
/// which is the one line an operator needs before writing a SPARQL query or a grant.
fn browse_line(
    roots: &[(String, std::path::PathBuf)],
    watched: &[Watched],
    graph: &browse::Graph,
) -> String {
    if roots.is_empty() {
        return "not composed — no gonk.browse.root; urn:repo:* and the git/gh facades are \
                not bound"
            .to_string();
    }
    let names: Vec<String> = roots
        .iter()
        .map(|(name, _)| {
            if watched.iter().any(|root| &root.name == name) {
                format!("{name} (watched)")
            } else {
                format!("{name} (live, unwatched)")
            }
        })
        .collect();
    let where_they_land = match graph.named() {
        // ⚠ `.as_str()`: `NamedNode`'s Display brackets the IRI itself (ledger #382).
        Some(iri) => format!("annotations and archive in <{}>", iri.as_str()),
        None => "annotations and archive in this dataset's DEFAULT graph — no token can name \
                 it, so every query over them is a root one"
            .to_string(),
    };
    format!("urn:repo:{{{}}}:* — {where_they_land}", names.join(", "))
}

/// The banner's LLM line: where `urn:llm:*` resolves, whether the explanation families are
/// bound because of it, and — when they are — what one derivation may cost.
///
/// The ceilings are on the banner because they are the per-call half of the spend gate, and
/// an operator who cannot see them has no way to know what a configured tier will spend
/// short of reading this source.
fn mount_line(settings: &config::Settings, explains: bool) -> String {
    let Some(mount) = settings.mounts.first() else {
        return "not mounted — no gonk.mount; urn:llm:* resolves nowhere, so explain, review \
                and the PR-derived layers are not bound"
            .to_string();
    };
    if !explains {
        return format!(
            "{} — mounted, but no gonk.browse.root, so nothing here derives through it",
            mount.target
        );
    }
    let tiers = &settings.explain;
    format!(
        "{} (prefer; dialled on first use) — explain/review bound; file {} @{}, dir {} @{}, \
         review {} @{}, pr {} @{} tokens. Deriving needs a net grant; this server mints none",
        mount.target,
        tiers.file.provider,
        tiers.file.max_tokens,
        tiers.dir.provider,
        tiers.dir.max_tokens,
        tiers.review.provider,
        tiers.review.max_tokens,
        tiers.pr.provider,
        tiers.pr.max_tokens,
    )
}

/// The render rules in effect: this deployment's `gonk/render-rules.ttl`, else the table
/// gonk ships. Parsed here so a table the renderer could not obey stops the server — the
/// house rule for configuration, and the reason [`web::Web`] carries the TEXT rather than a
/// parsed table: the text is what `urn:iki:gonk:render-rules` serves back.
fn read_render_rules(layout: &quic::Layout) -> Result<Arc<str>, String> {
    let path = layout.render_rules_ttl();
    let turtle: Arc<str> = match std::fs::read_to_string(&path) {
        Ok(text) => text.into(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ikigai_gonk::rules::DEFAULT_RULES.into())
        }
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    ikigai_gonk::rules::Rules::parse(&turtle)
        .map_err(|e| format!("{}: {e}", path.display()))
        .map(|_| turtle)
}

/// Every token an enrolment asks for: the ledgers' grants, plus — when `--browse-graph` was
/// given — the browse graph's two store doors, in first-seen order.
///
/// ★ One function for both minting paths, because a grant that means different things on a
/// certificate and on a passkey would be a difference nothing in this server could justify.
/// Every name is checked here, before anything is written: a typo refused at mint time is a
/// grant that never exists, and a typo written is a silent denial at first use.
fn scopes_for(ledgers: &[(String, Authority)], browse_graph: Option<Authority>) -> Vec<String> {
    let mut scopes: Vec<String> = Vec::new();
    let mut add = |token: String| {
        if !scopes.contains(&token) {
            scopes.push(token);
        }
    };
    for (ledger, authority) in ledgers {
        for token in grants::grants_for(ledger, *authority).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    if let Some(authority) = browse_graph {
        for token in grants::browse_graph_grants(authority).unwrap_or_else(|e| fail(&e)) {
            add(token);
        }
    }
    scopes
}

fn client_add(
    name: &str,
    cert: Option<&std::path::Path>,
    ledgers: &[(String, Authority)],
    browse_graph: Option<Authority>,
    force: bool,
) {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    let scopes = scopes_for(ledgers, browse_graph);
    let bundle = quic::add_client(&layout, name, cert, force).unwrap_or_else(|e| fail(&e));
    println!("client `{name}`  {}", bundle.dir.display());
    println!("  fingerprint  {}", bundle.fingerprint);
    if scopes.is_empty() {
        println!("  NOT enrolled — a trusted certificate with no grant is refused. Enrol it:");
        println!("    ikigai-gonk client add {name} --ledger default=write");
    } else {
        quic::enrol(&layout, name, &bundle.fingerprint, &scopes, force)
            .unwrap_or_else(|e| fail(&e));
        println!(
            "  enrolled     grant `{name}` ({} scopes) in {}",
            scopes.len(),
            layout.grants_json().display()
        );
    }
    println!("  restart ikigai-gonk: trusted certificates are read at startup");
    if cert.is_some() {
        // An imported certificate's bundle holds no private key: the client already has one.
        println!(
            "  copy {} into the client's own identity directory (beside its client.crt and \
             client.key), then from the client:",
            bundle.dir.join("server.crt").display()
        );
        println!("    ikigai --connect quic://<gonk host>:1060 --cert-dir <that directory>");
    } else {
        println!(
            "  this bundle holds the client's PRIVATE key and the server never reads it — move \
             the directory to the client, then from the client:"
        );
        println!("    ikigai --connect quic://<gonk host>:1060 --cert-dir <the moved directory>");
    }
}

fn passkey_invite(
    name: &str,
    ledgers: &[(String, Authority)],
    browse_graph: Option<Authority>,
    force: bool,
    minutes: u64,
    flags: &config::Flags,
) {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| fail(&e));
    let layout = quic::Layout::in_config_home(&homes.config);
    if ledgers.is_empty() && browse_graph.is_none() {
        fail(&format!(
            "an invite needs a grant: `ikigai-gonk passkey invite {name} --ledger default=delete`, \
             or `--browse-graph read` for the browse graph alone"
        ));
    }
    let scopes = scopes_for(ledgers, browse_graph);
    // ★ An identity must be STRICTLY stronger than an anonymous loopback caller, or signing
    // in would be a ceremony that changes nothing — and a grant that looked like it limited
    // someone would not.
    let anonymous = grants::grants_for_all(&settings.http_ledgers, Authority::Write)
        .unwrap_or_else(|e| fail(&e));
    if scopes.iter().all(|scope| anonymous.contains(scope)) {
        fail(&format!(
            "that grant adds nothing: an anonymous loopback caller already reads and writes {}. \
             Invite with more — `--ledger {}=delete`, or a ledger outside `gonk.http.ledger`",
            settings.http_ledgers.join(", "),
            settings
                .http_ledgers
                .first()
                .map(String::as_str)
                .unwrap_or("default")
        ));
    }
    let code = identity::invite(
        &layout,
        name,
        &scopes,
        force,
        minutes,
        identity::now_seconds(),
    )
    .unwrap_or_else(|e| fail(&e));
    println!(
        "passkey invite for `{name}` — grant `{name}` ({} scopes) in {}",
        scopes.len(),
        layout.grants_json().display()
    );
    println!("  valid for {minutes} minutes, once");
    println!(
        "  open  http://localhost:{}/#invite={code}",
        settings.http.port()
    );
    println!(
        "  in a browser on this machine (localhost, not 127.0.0.1); the passkey is enrolled in {}",
        layout.clients_json().display()
    );
}

fn fail(message: &str) -> ! {
    eprintln!("ikigai-gonk: {message}");
    std::process::exit(1);
}

/// `ikigai-gonk review request <repo> <path>` — drop one tuple and exit.
///
/// ★ **This is the whole of what a git hook runs.** No store is opened, no door is bound,
/// nothing is dialled: the tuple is written into the queue's inbox through the space's own
/// Sink (staging write, then an atomic rename, with the id blake3 of the content) and the
/// process exits. So a `post-commit` hook can call it once per changed file, in the
/// background, and the commit never waits — on this server being up, on a peer, or on a
/// model. A tuple is a REQUEST, not an authority: dropping one spends nothing and needs no
/// grant, which is exactly why the hook can be three lines and hold nothing.
///
/// The repository is NOT validated against `gonk.browse.root` here. It could be, and it is
/// not on purpose: the hook runs in a checkout, this command runs with whatever config it
/// can find, and a request for a root this server does not serve should fail where a pass
/// is run — with the kernel's own `Unresolved` naming the IRI — rather than be silently
/// dropped at the door by a second copy of the root list.
fn review_request(repo: &str, path: &str, flags: &config::Flags) {
    let homes = Homes::from_process().unwrap_or_else(|e| fail(&e));
    let (_, text) = config::read_config(flags, &homes).unwrap_or_else(|e| fail(&e));
    let settings = config::settings(flags, &text, &homes).unwrap_or_else(|e| fail(&e));
    let Some(queue) = settings.review else {
        fail(
            "no review queue is configured: add `gonk.review.space = \"reviews\"` to the \
             config home's config.toml. Binding the queue does not arm anything — that takes \
             `gonk.review.arm = true` AND a `gonk.review.grant` grants.json can honour; see \
             the README's \"Arming it\" section",
        )
    };
    let tuple = trigger::Tuple {
        repo: repo.to_string(),
        path: path.to_string(),
    };
    let id = trigger::drop_tuple(&queue, &tuple).unwrap_or_else(|e| fail(&e));
    println!("{id}");
}

/// The reviewer's scopes when this server is armed, `None` when it is not.
///
/// ★ **`arm` without a usable grant stops this server**, rather than serving a queue nothing
/// drains while the banner says otherwise. The two facts are independent on purpose (see
/// [`config`]), so the failure of one has to be loud.
fn resolve_reviewer(queue: &trigger::Trigger, layout: &quic::Layout) -> Option<Vec<String>> {
    if !queue.arm {
        return None;
    }
    let Some(grant) = &queue.grant else {
        fail(
            "gonk.review.arm is set and gonk.review.grant names nothing. Arming is a grant \
             plus a word, never a word alone: a pass runs under an authority an operator \
             wrote into grants.json, and there is nothing here to run one under",
        )
    };
    let scopes = quic::read_grants(&layout.grants_json())
        .and_then(|grants| trigger::reviewer_scopes(&grants, grant))
        .unwrap_or_else(|e| fail(&format!("gonk.review.arm is set, and {e}")));
    Some(scopes)
}

/// The host a `urn:cap:net:` grant must name — where the mounted peer lives.
///
/// ⚠ **The authority's HOST, not the whole authority and not `localhost`.**
/// `Capability::allows` is exact string containment, so `urn:cap:net:localhost` does not
/// satisfy a call to `127.0.0.1`: the two spellings are different hosts as far as a
/// capability is concerned, however the same they are to a resolver. A socket mount reaches
/// a peer on this machine and has no authority to take a host from, so it takes the name a
/// person would write.
fn mount_host(settings: &config::Settings) -> String {
    match settings.mounts.first().map(|m| &m.target) {
        Some(mount::Target::Quic { authority, .. }) => authority
            .rsplit_once(':')
            .map(|(host, _)| host.to_string())
            .unwrap_or_else(|| authority.clone()),
        _ => "localhost".to_string(),
    }
}

/// The banner's review line when the trigger IS armed.
///
/// ⚠ It says what bounds a push, because that is the question an operator has the moment
/// this is on: passes are serial, one per this process, and nothing bounds the wall clock.
fn armed_line(queue: &trigger::Trigger, scopes: usize, host: &str) -> String {
    format!(
        "urn:space:{} — ARMED: a drop fires {} under grant `{}` ({scopes} scopes, \
         net {host}, no urn:cap:annotate — a pass CANNOT publish). {} waiting now. Passes \
         run ONE AT A TIME in this process and nothing bounds wall clock, so a push \
         touching forty files occupies the reviewer for forty passes; watch `{}` or the \
         Queue badge. A queue that is not empty with nothing in flight is STUCK",
        queue.space,
        trigger::PASS,
        queue.grant.as_deref().unwrap_or("?"),
        trigger::pending(queue),
        trigger::DEPTH,
    )
}

/// The banner's queue line — which severities the Queue page asks a human about, and which
/// it only counts, from the contract this kernel binds (ledger #496).
///
/// ⚠ It names the words so an operator can see the gate they are running under; it is a
/// banner, and a banner is not something a review pass can read. Nothing in this crate puts
/// the gate where a pass could see it.
fn queue_line(settings: &config::Settings, declared: &[String]) -> String {
    let others = queue::other_severities(declared, &settings.queue);
    format!(
        "asks a human about {} ({}gonk.queue.serious) and any unrated finding; {} minted, \
         counted in the badge, listed under {}?{}={} — not queued",
        settings.queue.serious.join(", "),
        if settings.queue.configured {
            ""
        } else {
            "the default; "
        },
        if others.is_empty() {
            "nothing else is".to_string()
        } else {
            format!("{} are", others.join(", "))
        },
        queue::QUEUE_PATH,
        queue::SCOPE_ARG,
        queue::SCOPE_ALL,
    )
}

/// The banner's review line — the queue, what is waiting in it, and, when a grant is named,
/// whether `grants.json` can honour it.
///
/// ⚠ **It says plainly that nothing drains this** — because with `gonk.review.arm` unset
/// nothing does, and a queue filling with nobody reading it is the quietest way for this
/// feature to be useless. A person drains it one tuple at a time over the socket; the line
/// prints how, and what the word is that changes it.
fn review_line(settings: &config::Settings, layout: &quic::Layout) -> String {
    let Some(queue) = &settings.review else {
        return "not configured — no gonk.review.space; no queue is bound and \
                urn:iki:gonk:review:pass is not offered"
            .to_string();
    };
    let pending = trigger::pending(queue);
    let grant = match &queue.grant {
        None => "no gonk.review.grant named".to_string(),
        Some(name) => match quic::read_grants(&layout.grants_json())
            .and_then(|grants| trigger::reviewer_scopes(&grants, name))
        {
            Ok(scopes) => format!("grant `{name}` ({} scopes)", scopes.len()),
            Err(e) => format!("grant `{name}` UNUSABLE: {e}"),
        },
    };
    format!(
        "urn:space:{} — {pending} request(s) waiting, {grant}. NOT ARMED (no \
         gonk.review.arm = true): nothing in this process drains this queue. Run one with \
         `{}` over the socket",
        queue.space,
        trigger::DRAIN_ONE.replace("{space}", &queue.space),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The one message in this server that is itself a procedure: an operator reading it has
    /// an invisible archive and no other instructions. So the commands are checked — that they
    /// name THIS store, THIS graph, and spell the IRI the way a shell would have to.
    #[test]
    fn the_migration_warning_names_a_command_that_would_run() {
        let graph = browse::Graph::chosen();
        let iri = graph.named().expect("a named graph").as_str().to_string();
        let text = unmigrated_warning(
            10,
            &graph,
            std::path::Path::new("/tmp/store"),
            std::path::Path::new("/tmp/gonk.sock"),
        );

        assert!(text.contains("10 quad(s)"), "{text}");
        assert!(
            text.contains(&format!(
                "migrate-annotation-ns /tmp/store --graph {iri} --commit"
            )),
            "the command an operator will paste: {text}"
        );
        assert!(
            text.contains(&format!("migrate-annotation-ns /tmp/store --graph {iri}\n")),
            "the dry run comes first: {text}"
        );
        // ⚠ `NamedNode`'s Display brackets the IRI. A `{node}` anywhere in that message
        // produces `<<urn:…>>` in the prose and `--graph <urn:…>` in the command, which the
        // migration tool refuses as not an IRI.
        assert!(
            !text.contains("<<"),
            "an IRI was printed through Display: {text}"
        );
        assert!(text.contains(&format!("--graph {iri}")), "{text}");
        assert!(text.contains(&format!("<{iri}>")), "{text}");
        // The irreversible half is never implicit — and the backup command names the socket,
        // because the backup family is bound behind it and a bare `ikigai -c` does not reach
        // this server at all.
        assert!(
            text.contains("ikigai --connect /tmp/gonk.sock -c 'source urn:iki:gonk:backup'"),
            "{text}"
        );
        assert!(text.contains("restarting does not undo it"), "{text}");

        // The rollback arm names no command, because none of these would perform it.
        let back = unmigrated_warning(
            10,
            &browse::Graph::TheDefault,
            std::path::Path::new("/tmp/store"),
            std::path::Path::new("/tmp/gonk.sock"),
        );
        assert!(!back.contains("migrate-annotation-ns"), "{back}");
        assert!(back.contains("rolled back"), "{back}");
    }

    /// The backup line's command is one an operator pastes, so it must be the form that
    /// RETURNS: through the socket this server binds (#381), not a bare `ikigai -c`.
    #[test]
    fn the_backup_line_names_a_status_command_that_reaches_this_server() {
        let line = scheduled_line(
            std::time::Duration::from_secs(86_400),
            std::path::Path::new("/var/backups/gonk"),
            7,
            "",
            std::path::Path::new("/tmp/elsewhere/gonk.sock"),
        );
        assert!(
            line.contains(
                "`ikigai --connect /tmp/elsewhere/gonk.sock -c 'source urn:iki:gonk:backup:status'`"
            ),
            "{line}"
        );
        assert!(!line.contains("`source "), "the bare form hangs: {line}");
        assert!(
            line.contains("every 24h into /var/backups/gonk — keep 7."),
            "{line}"
        );
    }

    /// The browse line names the graph with exactly one pair of brackets (#382). "Contains
    /// the IRI" alone passes on `<<urn:…>>`, so the whole bracketed form is pinned and `<<`
    /// is refused.
    #[test]
    fn the_browse_line_brackets_the_graph_once() {
        let graph = browse::Graph::chosen();
        let iri = graph.named().expect("a named graph").as_str();
        let line = browse_line(&[("demo".to_string(), "/tmp/demo".into())], &[], &graph);
        assert!(
            line.ends_with(&format!("annotations and archive in <{iri}>")),
            "{line}"
        );
        assert!(!line.contains("<<") && !line.contains(">>"), "{line}");
    }
}
