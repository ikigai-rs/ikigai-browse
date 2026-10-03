//! Flags and the config home — never environment variables.
//!
//! Settings live in the machine's shared `config.toml` (`$XDG_CONFIG_HOME/ikigai`, else
//! `~/.config/ikigai`), under keys this server owns by prefix, in the same `key = "value"`
//! line grammar every ikigai process reads. The store's directory is NOT a key here: it is
//! `ikigai-store`'s own layered `store.toml` (with `gonk.store.toml` beside it), so one
//! setting has one spelling.
//!
//! ```toml
//! gonk.bind = "127.0.0.1:1060"          # the HTTP door (loopback only; this IS the default)
//! # gonk.port = 1060                    # shorthand for gonk.bind = "127.0.0.1:<port>"
//! gonk.socket = "~/.ikigai/gonk.sock"   # the owner-only socket (this IS the default)
//! # gonk.quic.bind = "0.0.0.0:1060"     # set it to REQUIRE the QUIC door; unset, it opens at
//!                                       # this default once a client certificate is enrolled
//! gonk.http.ledger = "default"          # ledgers the HTTP door may read and write; repeatable
//! gonk.browse.root = "core=~/git-personal/ikigai-core"   # a browsable repository; repeatable
//! gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 ~/.config/ikigai/gonk/quic/peers/plasma"
//! # gonk.explain.file.max_tokens = 400   # the per-call spend ceilings, per grain
//! gonk.review.space = "reviews"        # bind the git-event review QUEUE (urn:space:reviews)
//! # gonk.review.grant = "reviewer"     # the grant a pass runs under; naming it arms NOTHING
//! # gonk.review.arm = true             # ⚠ ARM it: watch the queue and review on every drop
//! # gonk.review.root = "~/.ikigai/spaces"   # the spaces tree (this IS the default)
//! # gonk.review.judge = "urn:llm:coder-next:ask"   # the JUDGE on each serious finding a pass
//!                                       # mints (default urn:llm:coder:ask, browse's); "off" for none
//! # gonk.queue.serious = "critical,major"   # the severities the Queue page asks a human about
//!                                       # (this IS the default); the rest are minted, not queued
//! gonk.backup.every = "24h"            # the backup cadence (this IS the default; "off" for none)
//! # gonk.backup.keep = 5               # how many archives the rotation keeps (this IS the default)
//! # gonk.backup.dir = "~/.ikigai/backups"   # where they land (this IS the default)
//! ```
//!
//! With no `gonk.browse.root` line this server composes exactly what it composed before: the
//! store and the ledgers. Each line adds `urn:repo:<name>:…` over that directory — and the
//! first one also binds `ikigai-repo`'s facades, which run `git` and `gh`. That is a real
//! widening of what is compiled in and reachable, so it is opt-in, per root, by path.
//!
//! **Flags override config wholesale**, the rule standalone `ikigai-web` set: `--port 9000`
//! against `gonk.bind = "127.0.0.1:1070"` serves loopback 9000, and a repeatable flag
//! replaces every line of its key rather than adding to them. A key under `gonk.` that this
//! server does not know is an error, not a no-op — a misspelled setting that silently did
//! nothing would look exactly like one that is in effect.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::grants::Authority;
use crate::mount::{self, Mount};

/// The HTTP door's default port — and the QUIC door's, on UDP. See the README for 1060.
pub const DEFAULT_PORT: u16 = 1060;

/// The ledger the HTTP door serves when no `gonk.http.ledger` line names one.
pub const DEFAULT_LEDGER: &str = "default";

/// The socket's file name under the data home.
pub const SOCKET_NAME: &str = "gonk.sock";

/// The longest Unix socket path accepted, exclusive: `sun_path` on macOS (Linux allows 108;
/// the smaller bound is taken so a config that works on one works on both).
pub const SOCKET_PATH_LIMIT: usize = 104;

/// How often a backup is taken when no `gonk.backup.every` line says otherwise, and the
/// requirement this feature was built to: every 24 hours, compressed, keep the last five.
pub const DEFAULT_BACKUP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How many archives the rotation keeps by default.
pub const DEFAULT_BACKUP_KEEP: usize = 5;

/// The rotation directory's name under the data home.
pub const BACKUP_DIR_NAME: &str = "backups";

/// The severities the Queue page asks a human about when no `gonk.queue.serious` line says
/// otherwise — `ikigai-browse`'s own `SERIOUS_SEVERITIES` prefix of its severity list, spelled
/// here because that constant is `pub(crate)` there.
///
/// ★ **This is the ONLY place this crate spells a severity word**, and it is checked rather
/// than trusted: `main` validates every word against the finding contract's own `one_of`
/// before a door opens ([`crate::queue::check_serious`]), so a browse release that renames a
/// severity stops this server at start, naming both lists, instead of serving an empty queue.
pub const DEFAULT_QUEUE_SERIOUS: &str = "critical,major";

/// Every key this server reads.
const KEYS: [&str; 25] = [
    "gonk.queue.serious",
    "gonk.bind",
    "gonk.port",
    "gonk.socket",
    "gonk.quic.bind",
    "gonk.http.ledger",
    "gonk.browse.root",
    "gonk.mount",
    "gonk.explain.file.provider",
    "gonk.explain.file.max_tokens",
    "gonk.explain.dir.provider",
    "gonk.explain.dir.max_tokens",
    "gonk.explain.review.provider",
    "gonk.explain.review.max_tokens",
    "gonk.explain.pr.provider",
    "gonk.explain.pr.max_tokens",
    "gonk.explain.max_prompt_bytes",
    "gonk.backup.dir",
    "gonk.backup.every",
    "gonk.backup.keep",
    "gonk.review.space",
    "gonk.review.grant",
    "gonk.review.root",
    "gonk.review.arm",
    "gonk.review.judge",
];

/// How to invoke the binary.
pub const USAGE: &str = "\
ikigai-gonk — a standalone ikigai work-ledger server

usage:
  ikigai-gonk [serve] [flags]      hold the durable store and serve the ledgers
  ikigai-gonk client add <name> [--ledger <ledger>=<read|write|delete|purge>]... [--browse-graph <read|write>] [--cert <client.crt>] [--force]
                                   trust a QUIC client: mint its identity (or import the
                                   certificate it generated with --cert) into a bundle, and
                                   with --ledger enrol its fingerprint under a grant
  ikigai-gonk passkey invite <name> --ledger <ledger>=<read|write|delete|purge>... [--browse-graph <read|write>] [--minutes N] [--port N] [--config PATH] [--force]
                                   write grant <name> and print a one-time
                                   http://localhost:<port>/#invite=… link; the browser that
                                   opens it enrols a passkey under that grant
  ikigai-gonk review request <repo> <path> [--config PATH]
                                   drop ONE review request into the queue `gonk.review.space`
                                   names. Writes one file and exits: no store, no door, no
                                   network — so a git hook can call it per changed file and a
                                   commit never waits. Identical requests collapse to one
                                   tuple (the drop is content-addressed)
  ikigai-gonk grants <ledger> [read|write|delete|purge]
                                   print the capability tokens for one ledger (JSON)
  ikigai-gonk grants --browse-graph [read|write]
                                   print the tokens for the BROWSE graph — SPARQL access to
                                   annotations, archived explanations and review findings as
                                   quads, through urn:iki:store:graph-*. Not the browse
                                   endpoints: no file contents, no gh, and no deriving

serve flags (each overrides its config key wholesale):
  --bind IP:PORT        the HTTP door (config `gonk.bind`); loopback only — default 127.0.0.1:1060
  --port N              shorthand for --bind 127.0.0.1:N (config `gonk.port`)
  --socket PATH         the owner-only socket (config `gonk.socket`); default ~/.ikigai/gonk.sock
  --quic-bind IP:PORT   the QUIC door (config `gonk.quic.bind`); default 0.0.0.0:1060 (UDP).
                        Unset, the door opens once a client certificate is enrolled; set,
                        it must open, and gonk refuses to start when no client can be admitted
  --no-quic             do not open the QUIC door this run
  --http-ledger NAME    a ledger the HTTP door may read and write (repeatable; config
                        `gonk.http.ledger`); default `default`
  --browse-root N=PATH  serve urn:repo:N:* over PATH (repeatable; config `gonk.browse.root`).
                        Unset, the browse family and ikigai-repo's git/gh facades are not
                        bound at all; the socket door's root capability reaches both
  --mount SPEC          `prefer urn:llm:=<target> [cert-dir]` (repeatable; config
                        `gonk.mount`) — the peer that serves urn:llm:*, which is what binds
                        the explain and review families over the browse roots. Unset, they
                        are not bound at all: an action no kernel can satisfy is an
                        over-offer. Deriving one requires a net grant, which no grant this
                        server MINTS carries — see `grants` and the README
  --no-backup           take no SCHEDULED backup this run (config `gonk.backup.every =
                        \"off\"`). urn:iki:gonk:backup and urn:iki:gonk:restore stay bound:
                        this server is the only thing that can export the dataset
  --config PATH         read this file instead of <config home>/config.toml

files (in the config home, ~/.config/ikigai unless XDG_CONFIG_HOME says otherwise):
  config.toml               the gonk.* keys above
  store.toml, gonk.store.toml   where the dataset lives (default ~/.ikigai/store)
  gonk/clients.json         identity -> grant name: certificates under `clients` (the QUIC door
                            opens when one is enrolled), passkeys under `passkeys` (HTTP only)
  gonk/grants.json          grant name -> capability scopes
  gonk/quic/                server identity (generated on first use) and clients/<name>/ bundles
";

/// A parsed command line.
#[derive(Debug)]
pub enum Command {
    /// Serve, with these flags.
    Serve(Flags),
    /// Trust a QUIC client, and optionally enrol it.
    ClientAdd {
        /// The bundle's name.
        name: String,
        /// A certificate the client generated itself, to import instead of minting.
        cert: Option<PathBuf>,
        /// Ledgers and authorities to enrol the client under.
        ledgers: Vec<(String, Authority)>,
        /// `--browse-graph`: also grant the browse graph's store tokens at this authority.
        browse_graph: Option<Authority>,
        /// Replace an existing bundle or enrolment.
        force: bool,
    },
    /// Drop one review request into the configured queue.
    ReviewRequest {
        /// The `gonk.browse.root` name.
        repo: String,
        /// The file's path within that root.
        path: String,
        /// Where to read the config from.
        flags: Flags,
    },
    /// Invite a browser to enrol a passkey under a grant.
    PasskeyInvite {
        /// The grant name (and the default label).
        name: String,
        /// Ledgers and authorities the grant holds.
        ledgers: Vec<(String, Authority)>,
        /// `--browse-graph`: also grant the browse graph's store tokens at this authority.
        browse_graph: Option<Authority>,
        /// Replace an existing grant of that name with different scopes.
        force: bool,
        /// How long the invite is valid.
        minutes: u64,
        /// `--config` / `--port`, so the printed URL names the port the server serves on.
        flags: Flags,
    },
    /// Print the tokens for one ledger, or for the browse graph.
    Grants {
        /// The ledger — `None` for `--browse-graph`, whose subject is not a ledger.
        ledger: Option<String>,
        /// The authority.
        authority: Authority,
    },
    /// Print the usage.
    Help,
}

/// The serve flags, unmerged.
#[derive(Debug, Default)]
pub struct Flags {
    /// `--bind`.
    pub bind: Option<String>,
    /// `--port`.
    pub port: Option<u16>,
    /// `--config`.
    pub config: Option<PathBuf>,
    /// `--socket`.
    pub socket: Option<String>,
    /// `--quic-bind`.
    pub quic_bind: Option<String>,
    /// `--no-quic`.
    pub no_quic: bool,
    /// `--http-ledger`, in order.
    pub http_ledgers: Vec<String>,
    /// `--browse-root`, in order, unparsed (`name=path`).
    pub browse_roots: Vec<String>,
    /// `--mount`, in order, unparsed.
    pub mounts: Vec<String>,
    /// `--no-backup`: take no SCHEDULED backup this run. The backup family stays bound —
    /// see [`BackupPolicy::every`].
    pub no_backup: bool,
}

/// Everything `serve` needs, merged and validated.
#[derive(Debug)]
pub struct Settings {
    /// The HTTP door's address. Always loopback — see [`refuse_non_loopback`].
    pub http: SocketAddr,
    /// The socket path.
    pub socket: PathBuf,
    /// The QUIC door's address, and whether anyone asked for it. Whether it actually opens
    /// is [`crate::quic::open_door`]'s decision.
    pub quic: QuicBind,
    /// The ledgers the HTTP door may read and write.
    pub http_ledgers: Vec<String>,
    /// The browsable roots, `(name, directory)`, `~/`-expanded and validated. Empty means
    /// the browse family is not composed at all.
    pub browse_roots: Vec<(String, PathBuf)>,
    /// The mounted peers. Empty means `urn:llm:*` resolves nowhere, and the explanation
    /// families are therefore not bound — see [`Settings::explains`].
    pub mounts: Vec<Mount>,
    /// The per-kind providers and spend ceilings the explanation families derive under.
    pub explain: ExplainTiers,
    /// Where backups land, how many are kept, and how often one is taken.
    pub backup: BackupPolicy,
    /// The git-event review queue, or `None` when no `gonk.review.space` line exists.
    ///
    /// ⚠ Configuring it binds the QUEUE; it does not arm anything. See [`crate::trigger`].
    pub review: Option<crate::trigger::Trigger>,
    /// Which severities the Queue page asks a human about (`gonk.queue.serious`).
    pub queue: QueuePolicy,
}

/// What the Queue page asks a human about, from `gonk.queue.serious` — the SERIOUS set.
///
/// Ledger [#496](http://localhost:1060/l/default/item/496). Brian, 2026-09-21: *"the preference
/// is to highlight issues that need addressing, so narrowing the squishy stuff is the
/// priority"* — and, the same day, on praise: *"positive signal is still signal and tells us
/// something about the code."* Both hold, about different places. **Minting** keeps every
/// severity; **triage** asks a human only about these. Everything else is still minted,
/// stored, anchored, counted in the badge and listed under `severity=all` — it just does not
/// ask for a decision.
///
/// ⚠ The words are parsed here and VALIDATED in `main`, against the finding contract's own
/// `severity` `one_of` ([`crate::queue::check_serious`]): a word the contract does not declare
/// is a refusal at start, named, never a silent empty queue. This type does not hold the
/// contract, so it cannot check; it only refuses the shapes that are wrong on their face.
///
/// ★ **An unrated finding is never hidden.** The gate is on a word, and a row with no word has
/// not been classed as a suggestion by anyone — the model proposed nothing. Hiding it would be
/// a finding nobody is ever asked about, which is the worse failure. See [`QueuePolicy::queues`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePolicy {
    /// The serious severities, in the order the line spelled them.
    pub serious: Vec<String>,
    /// Whether a `gonk.queue.serious` line was written, or the default is in effect. `main`
    /// uses it the way `gonk.review.arm` is used: a line that names a policy for a queue this
    /// server does not serve is a refusal, not a no-op.
    pub configured: bool,
}

impl Default for QueuePolicy {
    fn default() -> QueuePolicy {
        QueuePolicy {
            serious: split_words(DEFAULT_QUEUE_SERIOUS),
            configured: false,
        }
    }
}

impl QueuePolicy {
    /// Whether a severity WORD is in the serious set — the classification the serious-share
    /// number rests on ([`crate::trigger::Status`]). An unrated finding has no word and is
    /// not serious by this test; see [`QueuePolicy::queues`] for what the page shows.
    pub fn is_serious(&self, severity: &str) -> bool {
        self.serious.iter().any(|s| s == severity)
    }

    /// Whether the Queue page asks a human about a finding rated `severity` by default:
    /// serious, or unrated (`None`). ⚠ Callers pass `effective_severity` falling back to
    /// `severity`, so a human override — none exists today; 0 of 371 pending rows differed on
    /// 2026-09-21 — is what decides once one is set.
    pub fn queues(&self, severity: Option<&str>) -> bool {
        match severity {
            Some(word) => self.is_serious(word),
            None => true,
        }
    }
}

/// `gonk.queue.serious`, or the default. Refuses an empty list and a repeated word; the
/// contract check is `main`'s, once there is a kernel to ask.
fn queue_policy(text: &str) -> Result<QueuePolicy, String> {
    let Some(spelled) = value_for(text, "gonk.queue.serious") else {
        return Ok(QueuePolicy::default());
    };
    let serious = split_words(&spelled);
    if serious.is_empty() {
        return Err(format!(
            "gonk.queue.serious = `{spelled}` names no severity. It is a comma-separated list \
             of the severities the Queue page asks a human about (the default is \
             `{DEFAULT_QUEUE_SERIOUS}`); an empty list would be a queue that asks about \
             nothing, so it is refused rather than served"
        ));
    }
    for (i, word) in serious.iter().enumerate() {
        if serious[..i].contains(word) {
            return Err(format!(
                "gonk.queue.serious = `{spelled}` names `{word}` twice"
            ));
        }
    }
    Ok(QueuePolicy {
        serious,
        configured: true,
    })
}

/// A comma-separated list, trimmed, empties dropped.
fn split_words(spelled: &str) -> Vec<String> {
    spelled
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The backup rotation's settings, as configured — [`crate::backup::Settings`] is this
/// plus the live store's path, which `main` knows and this module deliberately does not
/// (the store's directory is `ikigai-store`'s own layered `store.toml`, so one setting has
/// one spelling).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPolicy {
    /// The rotation directory.
    pub dir: PathBuf,
    /// How many archives to keep, pruned oldest-first.
    pub keep: usize,
    /// The cadence, or `None` when `gonk.backup.every = "off"` (or `--no-backup`).
    ///
    /// ★ Turning the timer off does NOT unbind the backup family. An operator who has
    /// decided not to run a schedule still has a dataset only this process can export, and
    /// a `urn:iki:gonk:backup` that vanished with the schedule would leave them with no way
    /// to take one by hand.
    pub every: Option<Duration>,
}

impl Settings {
    /// Whether this server binds the explanation and review families at all.
    ///
    /// ★ **A mount line is the switch, and it is a `declared = enforced` switch rather than
    /// a convenience.** `urn:repo:{root}:explain`, `:review` and the PR-derived layers all
    /// resolve `urn:llm:{provider}:ask` through the kernel; with nothing bound under
    /// `urn:llm:`, every one of them would be an action the catalog offers, the manifold
    /// advertises and `urn:kernel:validate` passes, which the kernel can then never satisfy
    /// — an over-offer, which the module recipe calls the worse direction. Reachability is a
    /// different question from bindability: a CONFIGURED peer that happens to be down makes
    /// explain transiently `Unavailable`, which is an honest answer and what `prefer` means.
    pub fn explains(&self) -> bool {
        !self.mounts.is_empty() && !self.browse_roots.is_empty()
    }
}

/// What each explanation grain asks, and at what ceiling — `ikigai-browse`'s own defaults
/// unless the config says otherwise.
///
/// ★ **These ceilings are the per-call half of the spend gate.** `max_tokens` is mandatory
/// on every ask (a thinking model with no ceiling burns the budget on reasoning and returns
/// nothing), and it is also the only bound on what ONE derivation costs. The other halves
/// are the archive — an explanation is derived once per content version and reused forever —
/// and the capability, which decides who may derive at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainTiers {
    /// `urn:repo:{root}:explain:{file}` — the file grain.
    pub file: Tier,
    /// `urn:repo:{root}:explain[:{dir}]` — the directory rollup.
    pub dir: Tier,
    /// `urn:repo:{root}:review:{path}` — the review pass, whose findings are annotations.
    pub review: Tier,
    /// `urn:repo:{root}:pr:{n}:explain` and `:review`.
    pub pr: Tier,
    /// How much of a file (or a rollup's material) is fed to the model before truncation.
    pub max_prompt_bytes: usize,
    /// The provider the JUDGE asks about each serious finding a review pass mints
    /// (`gonk.review.judge`), or `None` for no judge (`gonk.review.judge = "off"`).
    ///
    /// ★ A key under `gonk.review`, not `gonk.explain`, because it is read as part of the
    /// review: the judge rides with the pass that mints the finding (ledger #696). It is parsed
    /// with the tiers because it is the same kind of fact — one `urn:llm:` provider an
    /// operator chose for one call — and lands in the same `ExplainConfig`.
    ///
    /// ⚠ Off means passes mint findings with no verdict, as every pass before 0.16.0 did. It
    /// does NOT unbind `urn:repo:{root}:judge-finding:{id}`: browse binds that with the review
    /// family and, with no judge configured, falls back to the REVIEW tier.
    pub judge: Option<String>,
}

/// One grain's backend and ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tier {
    /// The provider IRI, which must be under `urn:llm:` — the one prefix a mount may claim,
    /// so a provider outside it could never resolve.
    pub provider: String,
    /// The `max_tokens` ceiling for one call.
    pub max_tokens: u32,
}

impl Default for ExplainTiers {
    /// `ikigai-browse`'s design-of-record tiers, spelled here rather than left implicit:
    /// this is the table an operator is overriding, and the banner prints it.
    fn default() -> ExplainTiers {
        let tier = |provider: &str, max_tokens| Tier {
            provider: provider.to_string(),
            max_tokens,
        };
        ExplainTiers {
            file: tier("urn:llm:coder:ask", 400),
            dir: tier("urn:llm:ask", 600),
            review: tier("urn:llm:coder:ask", 800),
            pr: tier("urn:llm:coder:ask", 600),
            max_prompt_bytes: 16 * 1024,
            // browse 0.16.0's own default — the review tier's provider.
            judge: Some("urn:llm:coder:ask".to_string()),
        }
    }
}

/// The QUIC door's bind, and where it came from — which is half of whether it opens.
///
/// ★ The distinction exists because `gonk/clients.json` serves two doors. A passkey enrolment
/// writes that file too, so the file existing is not a decision to face the network; an
/// operator naming a bind is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuicBind {
    /// `--no-quic`.
    Off,
    /// Nobody named one: the default address, used only once a certificate is enrolled.
    Default(SocketAddr),
    /// `--quic-bind` or `gonk.quic.bind`: the operator asked for the door, so a door that
    /// cannot admit anyone stops the server rather than quietly staying shut.
    Explicit(SocketAddr),
}

impl QuicBind {
    /// The address, unless QUIC is off.
    pub fn addr(self) -> Option<SocketAddr> {
        match self {
            QuicBind::Off => None,
            QuicBind::Default(addr) | QuicBind::Explicit(addr) => Some(addr),
        }
    }
}

/// The two ikigai homes, and `$HOME` for `~/` expansion.
#[derive(Debug, Clone)]
pub struct Homes {
    /// `$HOME`.
    pub home: PathBuf,
    /// The config home (`$XDG_CONFIG_HOME/ikigai`, else `~/.config/ikigai`).
    pub config: PathBuf,
    /// The data home (`~/.ikigai`).
    pub data: PathBuf,
}

impl Homes {
    /// The process's real homes, from `ikigai_core::config` — the one place the ecosystem
    /// spells them. Fails rather than guessing a working-directory-relative path.
    pub fn from_process() -> Result<Homes, String> {
        let config = ikigai_core::config::config_home()
            .ok_or("no ikigai config home: neither XDG_CONFIG_HOME nor HOME is set")?;
        let data =
            ikigai_core::config::data_home().ok_or("no ikigai data home: HOME is not set")?;
        let home = data
            .parent()
            .map(Path::to_path_buf)
            .ok_or("the data home has no parent directory")?;
        Ok(Homes { home, config, data })
    }
}

/// Parse the argument vector (without the program name).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    match args.peek().map(String::as_str) {
        Some("client") => {
            args.next();
            return parse_client(args);
        }
        Some("passkey") => {
            args.next();
            return parse_passkey(args);
        }
        Some("review") => {
            args.next();
            return parse_review(args);
        }
        Some("grants") => {
            args.next();
            let subject = args
                .next()
                .ok_or("grants: expected <ledger> [read|write|delete|purge], or --browse-graph")?;
            // ★ A FLAG, not a reserved ledger name. `grants browse …` would have read
            // better and would have been a trap: `browse` is a name `Ledger::parse`
            // accepts, so a server with a ledger called that could never print its tokens.
            let ledger = match subject.as_str() {
                "--browse-graph" => None,
                other if other.starts_with('-') => {
                    return Err(format!("grants: unknown argument `{other}`"))
                }
                other => Some(other.to_string()),
            };
            let authority = match args.next() {
                Some(value) => value.parse()?,
                None => Authority::Write,
            };
            if let Some(extra) = args.next() {
                return Err(format!("grants: unexpected argument `{extra}`"));
            }
            return Ok(Command::Grants { ledger, authority });
        }
        Some("serve") => {
            args.next();
        }
        _ => {}
    }
    let mut flags = Flags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "--bind" => flags.bind = Some(value(&mut args, "--bind")?),
            "--port" => {
                let port = value(&mut args, "--port")?;
                flags.port = Some(
                    port.parse()
                        .map_err(|_| format!("--port: `{port}` is not a port number"))?,
                );
            }
            "--config" => flags.config = Some(PathBuf::from(value(&mut args, "--config")?)),
            "--socket" => flags.socket = Some(value(&mut args, "--socket")?),
            "--quic-bind" => flags.quic_bind = Some(value(&mut args, "--quic-bind")?),
            "--no-quic" => flags.no_quic = true,
            "--http-ledger" => flags.http_ledgers.push(value(&mut args, "--http-ledger")?),
            "--browse-root" => flags.browse_roots.push(value(&mut args, "--browse-root")?),
            "--mount" => flags.mounts.push(value(&mut args, "--mount")?),
            "--no-backup" => flags.no_backup = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Command::Serve(flags))
}

/// `ikigai-gonk review request <repo> <path> [--config PATH]` — drop one tuple.
///
/// ★ Deliberately tiny, because a `post-commit` hook calls it once per changed file. It
/// opens no store, binds no door and dials nothing: it writes one file into the configured
/// queue and exits. It therefore works while gonk is DOWN, which is the property a hook
/// needs — a commit must never wait on this server being up, let alone on a model.
fn parse_review(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        Some("request") => {}
        Some(other) => {
            return Err(format!(
                "review: `{other}` is not a subcommand (request <repo> <path>)"
            ))
        }
        None => return Err("review: expected `request <repo> <path>`".to_string()),
    }
    let repo = args.next().ok_or(
        "review request: expected <repo> <path> — the `gonk.browse.root` NAME, then \
                the file's path within it",
    )?;
    let path = args
        .next()
        .ok_or("review request: expected <path> after the repository name")?;
    let mut flags = Flags::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => flags.config = Some(PathBuf::from(value(&mut args, "--config")?)),
            other => return Err(format!("review request: unknown argument `{other}`")),
        }
    }
    if repo.is_empty() || path.is_empty() {
        return Err("review request: neither the repository nor the path may be empty".to_string());
    }
    Ok(Command::ReviewRequest { repo, path, flags })
}

fn parse_client(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        Some("add") => {}
        Some(other) => {
            return Err(format!(
                "client: unknown subcommand `{other}` (expected `add`)"
            ))
        }
        None => return Err("client: expected `add <name>`".to_string()),
    }
    let name = args
        .next()
        .filter(|name| !name.starts_with('-'))
        .ok_or("client add: expected <name>")?;
    let (mut cert, mut ledgers, mut browse_graph, mut force) = (None, Vec::new(), None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cert" => cert = Some(PathBuf::from(value(&mut args, "--cert")?)),
            "--ledger" => {
                let spec = value(&mut args, "--ledger")?;
                let (ledger, authority) = spec.split_once('=').ok_or_else(|| {
                    format!("--ledger: expected <ledger>=<read|write|delete|purge>, got `{spec}`")
                })?;
                ledgers.push((ledger.to_string(), authority.parse()?));
            }
            "--browse-graph" => {
                browse_graph = Some(value(&mut args, "--browse-graph")?.parse()?);
            }
            "--force" => force = true,
            other => return Err(format!("client add: unknown argument `{other}`")),
        }
    }
    Ok(Command::ClientAdd {
        name,
        cert,
        ledgers,
        browse_graph,
        force,
    })
}

fn parse_passkey(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        Some("invite") => {}
        Some(other) => {
            return Err(format!(
                "passkey: unknown subcommand `{other}` (expected `invite`)"
            ))
        }
        None => return Err("passkey: expected `invite <name>`".to_string()),
    }
    let name = args
        .next()
        .filter(|name| !name.starts_with('-'))
        .ok_or("passkey invite: expected <name>")?;
    let (mut ledgers, mut browse_graph, mut force, mut minutes, mut flags) = (
        Vec::new(),
        None,
        false,
        crate::identity::INVITE_MINUTES,
        Flags::default(),
    );
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ledger" => {
                let spec = value(&mut args, "--ledger")?;
                let (ledger, authority) = spec.split_once('=').ok_or_else(|| {
                    format!("--ledger: expected <ledger>=<read|write|delete|purge>, got `{spec}`")
                })?;
                ledgers.push((ledger.to_string(), authority.parse()?));
            }
            "--browse-graph" => {
                browse_graph = Some(value(&mut args, "--browse-graph")?.parse()?);
            }
            "--force" => force = true,
            "--minutes" => {
                let spelled = value(&mut args, "--minutes")?;
                minutes = spelled
                    .parse()
                    .ok()
                    .filter(|m| (1..=24 * 60).contains(m))
                    .ok_or_else(|| format!("--minutes: `{spelled}` is not 1 to 1440"))?;
            }
            "--port" => {
                let port = value(&mut args, "--port")?;
                flags.port = Some(
                    port.parse()
                        .map_err(|_| format!("--port: `{port}` is not a port number"))?,
                );
            }
            "--config" => flags.config = Some(PathBuf::from(value(&mut args, "--config")?)),
            other => return Err(format!("passkey invite: unknown argument `{other}`")),
        }
    }
    Ok(Command::PasskeyInvite {
        name,
        ledgers,
        browse_graph,
        force,
        minutes,
        flags,
    })
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// The config file's path and text. An explicit `--config` that cannot be read is an
/// error; the default file is optional, and absent means unconfigured.
pub fn read_config(flags: &Flags, homes: &Homes) -> Result<(PathBuf, String), String> {
    match &flags.config {
        Some(path) => std::fs::read_to_string(path)
            .map(|text| (path.clone(), text))
            .map_err(|e| format!("--config {}: {e}", path.display())),
        None => {
            let path = homes.config.join("config.toml");
            match std::fs::read_to_string(&path) {
                Ok(text) => Ok((path, text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((path, String::new())),
                Err(e) => Err(format!("reading {}: {e}", path.display())),
            }
        }
    }
}

/// Merge flags over the config text into validated [`Settings`].
pub fn settings(flags: &Flags, text: &str, homes: &Homes) -> Result<Settings, String> {
    check_keys(text)?;
    let http = resolve_bind(flags.bind.as_deref(), flags.port, text)?;
    refuse_non_loopback(http)?;
    let socket = flags
        .socket
        .clone()
        .or_else(|| value_for(text, "gonk.socket"))
        .map(|spelled| expand_home(&spelled, &homes.home))
        .unwrap_or_else(|| homes.data.join(SOCKET_NAME));
    let quic = if flags.no_quic {
        QuicBind::Off
    } else {
        match flags
            .quic_bind
            .clone()
            .or_else(|| value_for(text, "gonk.quic.bind"))
        {
            Some(spelled) => {
                QuicBind::Explicit(parse_bind(&spelled).map_err(|e| format!("quic {e}"))?)
            }
            None => QuicBind::Default(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT))),
        }
    };
    // A Unix socket path is bounded by `sun_path` — 104 bytes on macOS, 108 on Linux, the
    // terminator included. Refused here, before the store is opened, rather than as a door
    // that dies a moment after the banner says it is serving.
    let socket_bytes = socket.as_os_str().len();
    if socket_bytes >= SOCKET_PATH_LIMIT {
        return Err(format!(
            "socket path {} is {socket_bytes} bytes; a Unix socket path must be shorter than \
             {SOCKET_PATH_LIMIT} — set `gonk.socket` (or --socket) to a shorter path",
            socket.display()
        ));
    }
    let http_ledgers = if !flags.http_ledgers.is_empty() {
        flags.http_ledgers.clone()
    } else {
        let lines = values_for(text, "gonk.http.ledger");
        if lines.is_empty() {
            vec![DEFAULT_LEDGER.to_string()]
        } else {
            lines
        }
    };
    let browse_roots = browse_roots(flags, text, homes)?;
    let mounts = mounts(flags, text, homes)?;
    let explain = explain_tiers(text)?;
    let backup = backup_policy(flags, text, homes)?;
    let review = review_trigger(text, homes)?;
    Ok(Settings {
        http,
        socket,
        quic,
        http_ledgers,
        browse_roots,
        mounts,
        explain,
        backup,
        review,
        queue: queue_policy(text)?,
    })
}

/// The review queue, from `gonk.review.*` — `None` unless `gonk.review.space` names one.
///
/// ★ **One line is the whole switch**, the shape `gonk.mount` established: with no
/// `gonk.review.space` this server binds no space, offers no `urn:iki:gonk:review:pass`,
/// and serves exactly the catalog it served before. A queue nothing can fill would be an
/// over-offer, which the module recipe calls the worse direction.
///
/// ⚠ `gonk.review.grant` is READ whether or not anything runs under it. It is printed by
/// the banner so an operator can see the authority they wrote down, and it is refused early
/// if `grants.json` cannot honour it — a grant that is a typo should not first be noticed
/// on the day the drainer is armed.
///
/// ★★ **`gonk.review.arm` is the second of two facts, and neither is implicit.** Naming a
/// grant is deliberately NOT arming: this server spent a release inviting operators to write
/// one down *so they could see it* (this doc said so, and so did the README), and a line
/// written under that invitation must not silently start spending inference on every commit.
/// So arming takes its own word — and `arm` without a usable grant is a startup refusal
/// rather than a server that quietly reviews nothing (`main`, [`crate::trigger::arm`]).
fn review_trigger(text: &str, homes: &Homes) -> Result<Option<crate::trigger::Trigger>, String> {
    let Some(space) = value_for(text, "gonk.review.space") else {
        if value_for(text, "gonk.review.arm").is_some() {
            return Err(
                "gonk.review.arm is set and gonk.review.space is not: there is no \
                        queue to arm. Name the space first"
                    .to_string(),
            );
        }
        return Ok(None);
    };
    crate::trigger::check_space_name(&space)?;
    let root = value_for(text, "gonk.review.root")
        .map(|spelled| expand_home(&spelled, &homes.home))
        .unwrap_or_else(|| homes.data.join(crate::trigger::SPACES_DIR));
    let grant = value_for(text, "gonk.review.grant");
    let arm = match value_for(text, "gonk.review.arm").as_deref() {
        None => false,
        Some("true") => true,
        Some("false") => false,
        Some(other) => {
            return Err(format!(
                "gonk.review.arm = `{other}` is neither `true` nor `false`. It decides \
                 whether this server runs review passes unattended, so it is not a setting \
                 to guess at"
            ))
        }
    };
    let trigger = crate::trigger::Trigger {
        space,
        grant,
        root,
        arm,
    };
    crate::trigger::refuse_cap_file(&trigger)?;
    Ok(Some(trigger))
}

/// The backup rotation, from flags then config then the defaults Brian's requirement
/// states: every 24 hours, keep the last five, under the data home beside the store.
///
/// ★ **The default is ON.** A backup feature that has to be switched on is a backup feature
/// that is off on the machine that needed it, and this dataset is the system of record for
/// the whole workflow. `--no-backup` and `gonk.backup.every = "off"` are how an operator
/// says otherwise, and the banner and `urn:iki:gonk:backup:status` both say which it is.
fn backup_policy(flags: &Flags, text: &str, homes: &Homes) -> Result<BackupPolicy, String> {
    let dir = value_for(text, "gonk.backup.dir")
        .map(|spelled| expand_home(&spelled, &homes.home))
        .unwrap_or_else(|| homes.data.join(BACKUP_DIR_NAME));
    let keep = match value_for(text, "gonk.backup.keep") {
        None => DEFAULT_BACKUP_KEEP,
        Some(spelled) => spelled
            .parse()
            .ok()
            .filter(|keep| *keep > 0)
            .ok_or_else(|| {
                format!(
                    "gonk.backup.keep: `{spelled}` is not a count of archives to keep (1 or \
                     more). To stop taking backups set `gonk.backup.every = \"off\"`; a \
                     rotation that keeps zero would delete the backup it just took"
                )
            })?,
    };
    let every = if flags.no_backup {
        None
    } else {
        match value_for(text, "gonk.backup.every") {
            None => Some(DEFAULT_BACKUP_EVERY),
            Some(spelled) if spelled.trim().eq_ignore_ascii_case("off") => None,
            Some(spelled) => Some(parse_every(&spelled)?),
        }
    };
    Ok(BackupPolicy { dir, keep, every })
}

/// A cadence in `ikigai-time`'s own compact grammar (`30m`, `6h`, `24h`), checked HERE so a
/// spelling this server cannot obey stops it rather than producing a job that never fires.
///
/// ⚠ Not `xsd:duration`: `ikigai-time` parses `1m`/`2h`, not `PT1M`, and the error says so
/// rather than leaving an operator to discover which grammar this is.
fn parse_every(spelled: &str) -> Result<Duration, String> {
    let schedule = ikigai_time::parse_schedule(spelled).map_err(|e| {
        format!(
            "gonk.backup.every: {e} — a duration like `24h`, `6h` or `30m`, or `off` to take \
             no scheduled backups"
        )
    })?;
    let interval = schedule.interval();
    // A cadence under a minute is a typo with a filesystem cost: every tick writes an
    // archive and prunes the rotation, so `24s` for `24h` would churn the disk and roll
    // the whole keep-five window past in two minutes.
    if interval < Duration::from_secs(60) {
        return Err(format!(
            "gonk.backup.every: `{spelled}` is under a minute. Every tick writes an archive \
             and prunes the rotation, so a cadence that short destroys the history it is \
             meant to keep — did you mean `{spelled}` with `h` rather than `s`?"
        ));
    }
    Ok(interval)
}

/// The mounted peers, from flags then config — one [`crate::mount::parse`] per line, and at
/// most one mount per prefix.
fn mounts(flags: &Flags, text: &str, homes: &Homes) -> Result<Vec<Mount>, String> {
    let lines = if flags.mounts.is_empty() {
        values_for(text, "gonk.mount")
    } else {
        flags.mounts.clone()
    };
    let mut mounts: Vec<Mount> = Vec::new();
    for line in lines {
        let mount = mount::parse(&line, &homes.home)?;
        if mounts.iter().any(|seen| seen.prefix == mount.prefix) {
            return Err(format!(
                "gonk.mount `{}`: `{}` is mounted twice — one prefix, one peer. (The cli                  orders overlapping mounts by prefix length; there is nothing to order here,                  because this server mounts one prefix.)",
                line, mount.prefix
            ));
        }
        mounts.push(mount);
    }
    Ok(mounts)
}

/// The explanation tiers, from the config over [`ExplainTiers::default`].
///
/// A provider outside `urn:llm:` is refused here rather than at the first derivation: the
/// only prefix a `gonk.mount` line may claim is `urn:llm:`, so such a provider could not
/// resolve anywhere in this process — and the symptom, an `Unresolved` in the middle of an
/// explain, would read as a peer problem rather than as the typo it is.
fn explain_tiers(text: &str) -> Result<ExplainTiers, String> {
    let mut tiers = ExplainTiers::default();
    for (kind, tier) in [
        ("file", &mut tiers.file),
        ("dir", &mut tiers.dir),
        ("review", &mut tiers.review),
        ("pr", &mut tiers.pr),
    ] {
        if let Some(provider) = value_for(text, &format!("gonk.explain.{kind}.provider")) {
            if !provider.starts_with(mount::LLM_PREFIX) {
                return Err(format!(
                    "gonk.explain.{kind}.provider: `{provider}` is not under                      `{}` — the only prefix a gonk.mount line may claim, so nothing in this                      process could resolve it",
                    mount::LLM_PREFIX
                ));
            }
            tier.provider = provider;
        }
        if let Some(spelled) = value_for(text, &format!("gonk.explain.{kind}.max_tokens")) {
            tier.max_tokens = spelled.parse().ok().filter(|t| *t > 0).ok_or_else(|| {
                format!("gonk.explain.{kind}.max_tokens: `{spelled}` is not a token count")
            })?;
        }
    }
    if let Some(spelled) = value_for(text, "gonk.explain.max_prompt_bytes") {
        tiers.max_prompt_bytes = spelled.parse().ok().filter(|b| *b > 0).ok_or_else(|| {
            format!("gonk.explain.max_prompt_bytes: `{spelled}` is not a byte count")
        })?;
    }
    if let Some(spelled) = value_for(text, "gonk.review.judge") {
        tiers.judge = judge_provider(&spelled)?;
    }
    Ok(tiers)
}

/// `gonk.review.judge`: a provider under `urn:llm:`, or `off` for no judge.
///
/// ⚠ An empty value is refused rather than read as `off`: switching off a check that confirms
/// or refutes every serious finding is a word an operator writes, not a blank they leave.
fn judge_provider(spelled: &str) -> Result<Option<String>, String> {
    let spelled = spelled.trim();
    if spelled.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    if !spelled.starts_with(mount::LLM_PREFIX) || spelled.len() == mount::LLM_PREFIX.len() {
        return Err(format!(
            "gonk.review.judge: `{spelled}` is neither a provider under `{}` — the only prefix \
             a gonk.mount line may claim, so nothing in this process could resolve it — nor \
             `off`, which runs review passes with no judge",
            mount::LLM_PREFIX
        ));
    }
    Ok(Some(spelled.to_string()))
}

/// The browsable roots, from flags then config — `name=path` per line, `~/`-expanded, the
/// name checked the way `ikigai-browse` checks it (which it does by panicking) and the
/// directory required to exist.
///
/// ★ **A root that is not there is refused, not skipped.** A missing directory is the shape
/// of a typo or a moved checkout, and the symptom of skipping it is a resolution MISS on
/// `urn:repo:<name>:tree` — which is what an unconfigured root looks like too, so the operator
/// would be told nothing at all.
fn browse_roots(
    flags: &Flags,
    text: &str,
    homes: &Homes,
) -> Result<Vec<(String, PathBuf)>, String> {
    let lines = if flags.browse_roots.is_empty() {
        values_for(text, "gonk.browse.root")
    } else {
        flags.browse_roots.clone()
    };
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for line in lines {
        let (name, path) = line.split_once('=').ok_or_else(|| {
            format!("browse root `{line}`: expected <name>=<path>, e.g. core=~/git/ikigai-core")
        })?;
        let (name, path) = (name.trim(), path.trim());
        crate::browse::check_root_name(name)?;
        if roots.iter().any(|(seen, _)| seen == name) {
            return Err(format!(
                "browse root `{name}` is named twice — one name, one directory"
            ));
        }
        let dir = expand_home(path, &homes.home);
        if !dir.is_dir() {
            return Err(format!(
                "browse root `{name}`: {} is not a directory",
                dir.display()
            ));
        }
        roots.push((name.to_string(), dir));
    }
    Ok(roots)
}

/// Parse an `IP:PORT` socket address — an IP, not a hostname: a bind is a listening
/// posture, and a posture should not depend on what a resolver says today.
pub fn parse_bind(spelled: &str) -> Result<SocketAddr, String> {
    spelled.parse().map_err(|_| {
        format!("bind `{spelled}`: expected IP:PORT (e.g. 127.0.0.1:1060, [::1]:1060)")
    })
}

/// The HTTP door's address, from flags then config then the default — standalone
/// `ikigai-web`'s rule with `gonk.` keys. `bind` and `port` are one setting spelled two ways,
/// so both at one level is an error; a port alone always means loopback.
pub fn resolve_bind(
    bind_flag: Option<&str>,
    port_flag: Option<u16>,
    text: &str,
) -> Result<SocketAddr, String> {
    if bind_flag.is_some() && port_flag.is_some() {
        return Err("--bind and --port conflict — --bind names the full IP:PORT".to_string());
    }
    let bind_key = value_for(text, "gonk.bind");
    let port_key = value_for(text, "gonk.port");
    if bind_key.is_some() && port_key.is_some() {
        return Err(
            "config sets both `gonk.bind` and `gonk.port` — they are one setting spelled two \
             ways; keep `gonk.bind` and drop `gonk.port`"
                .to_string(),
        );
    }
    if let Some(spelled) = bind_flag {
        return parse_bind(spelled);
    }
    if let Some(port) = port_flag {
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    if let Some(spelled) = bind_key {
        return parse_bind(&spelled).map_err(|e| format!("gonk.{e}"));
    }
    if let Some(port) = port_key {
        let port: u16 = port
            .parse()
            .map_err(|_| format!("gonk.port: `{port}` is not a port number"))?;
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    Ok(SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)))
}

/// ★ **The HTTP door binds loopback, or the server does not start.**
///
/// It is unauthenticated: it grants read and write on its ledgers to whatever reaches it,
/// and on loopback that is this machine's own processes. On a LAN it would be everyone on
/// the LAN, with the same grant — and HTTP to a LAN cannot do WebAuthn either (no secure
/// context), so nothing could be layered on it to tell callers apart. A warning would scroll
/// past in a launchd log while the door stayed open; a refusal is legible and has a fix, the
/// QUIC door, which authenticates every connection.
pub fn refuse_non_loopback(addr: SocketAddr) -> Result<(), String> {
    if crate::doors::is_loopback(addr.ip()) {
        return Ok(());
    }
    Err(format!(
        "refusing to serve HTTP on {addr}: the HTTP door is unauthenticated — it grants read \
         and write on its ledgers to whoever reaches it — so it binds loopback only \
         (127.0.0.1 or [::1]). To reach gonk from another machine, use the QUIC door, which \
         admits a connection only by a client certificate enrolled with `ikigai-gonk client \
         add`."
    ))
}

/// Refuse any `gonk.*` key this server does not read.
fn check_keys(text: &str) -> Result<(), String> {
    let unknown: Vec<&str> = lines(text)
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("gonk.") && !KEYS.contains(name))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unknown config key(s): {} — this server reads {}",
            unknown.join(", "),
            KEYS.join(", ")
        ))
    }
}

/// `~/`-expansion against `home`.
fn expand_home(spelled: &str, home: &Path) -> PathBuf {
    match spelled.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(spelled),
    }
}

/// The first `key = value` line for `key`, unquoted.
fn value_for(text: &str, key: &str) -> Option<String> {
    lines(text).find(|(name, _)| *name == key).map(|(_, v)| v)
}

/// Every `key = value` line for `key`, in file order.
fn values_for(text: &str, key: &str) -> Vec<String> {
    lines(text)
        .filter(|(name, _)| *name == key)
        .map(|(_, v)| v)
        .collect()
}

fn lines(text: &str) -> impl Iterator<Item = (&str, String)> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| {
            (
                name.trim(),
                value.trim().trim_matches(['"', '\'']).trim().to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn homes() -> Homes {
        Homes {
            home: PathBuf::from("/home/u"),
            config: PathBuf::from("/home/u/.config/ikigai"),
            data: PathBuf::from("/home/u/.ikigai"),
        }
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// Ledger #496: absent, the line means exactly `DEFAULT_QUEUE_SERIOUS`; present, it is
    /// the words as written; and the two shapes that are wrong on their face are refused
    /// here, before there is a contract to check the words against.
    #[test]
    fn the_serious_set_defaults_and_is_parsed_and_the_face_wrong_shapes_are_refused() {
        let absent = settings(&Flags::default(), "", &homes()).unwrap();
        assert_eq!(absent.queue, QueuePolicy::default());
        assert!(!absent.queue.configured);
        assert_eq!(
            absent.queue.serious.join(","),
            DEFAULT_QUEUE_SERIOUS,
            "the default is the documented line, verbatim"
        );
        // ★ Unrated is queued under every policy; a word is queued only when it is serious.
        assert!(absent.queue.queues(None));
        let first = absent.queue.serious[0].clone();
        assert!(absent.queue.queues(Some(&first)));
        assert!(absent.queue.is_serious(&first));
        assert!(!absent.queue.queues(Some("whatever")));
        assert!(!absent.queue.is_serious("whatever"));

        let one = settings(
            &Flags::default(),
            "gonk.queue.serious = \" alpha , beta \"",
            &homes(),
        )
        .unwrap();
        assert_eq!(one.queue.serious, ["alpha", "beta"]);
        assert!(one.queue.configured);

        let empty =
            settings(&Flags::default(), "gonk.queue.serious = \",\"", &homes()).unwrap_err();
        assert!(empty.contains("names no severity"), "{empty}");
        assert!(empty.contains(DEFAULT_QUEUE_SERIOUS), "{empty}");

        let twice = settings(
            &Flags::default(),
            "gonk.queue.serious = \"alpha,alpha\"",
            &homes(),
        )
        .unwrap_err();
        assert!(twice.contains("names `alpha` twice"), "{twice}");
    }

    /// ★ The requirement as Brian stated it, as a test: every 24 hours, keep the last five,
    /// and ON unless someone says otherwise — a backup feature that has to be switched on is
    /// a backup feature that is off on the machine that needed it.
    #[test]
    fn a_backup_is_daily_keeps_five_and_is_on_by_default() {
        let settings = settings(&Flags::default(), "", &homes()).unwrap();
        assert_eq!(settings.backup.every, Some(DEFAULT_BACKUP_EVERY));
        assert_eq!(settings.backup.every, Some(Duration::from_secs(86_400)));
        assert_eq!(settings.backup.keep, 5);
        assert_eq!(
            settings.backup.dir,
            PathBuf::from("/home/u/.ikigai/backups")
        );
    }

    #[test]
    fn the_backup_schedule_can_be_turned_off_without_unbinding_the_family() {
        for text in [
            "gonk.backup.every = \"off\"\n",
            "gonk.backup.every = \"OFF\"\n",
        ] {
            let settings = settings(&Flags::default(), text, &homes()).unwrap();
            assert_eq!(settings.backup.every, None);
            // And the rotation is still configured: `urn:iki:gonk:backup` is still bound,
            // so an operator with no schedule can still take one by hand.
            assert_eq!(settings.backup.keep, 5);
        }
        let flagged = Flags {
            no_backup: true,
            ..Flags::default()
        };
        assert_eq!(
            settings(&flagged, "gonk.backup.every = \"6h\"\n", &homes())
                .unwrap()
                .backup
                .every,
            None,
            "the flag overrides the key, like every other flag here"
        );
    }

    /// ⚠ A cadence this server cannot obey stops it. The sub-minute refusal is not
    /// pedantry: every tick writes an archive and prunes, so `24s` for `24h` would roll the
    /// whole keep-five window past in two minutes — destroying the history it exists to keep,
    /// in the shape of a setting that looks like it is working.
    #[test]
    fn a_cadence_this_server_cannot_obey_stops_it() {
        let six_hourly = settings(&Flags::default(), "gonk.backup.every = \"6h\"\n", &homes());
        assert_eq!(
            six_hourly.unwrap().backup.every,
            Some(Duration::from_secs(21_600))
        );
        let hourly = settings(&Flags::default(), "gonk.backup.every = \"90m\"\n", &homes());
        assert_eq!(
            hourly.unwrap().backup.every,
            Some(Duration::from_secs(5_400))
        );

        let iso = settings(
            &Flags::default(),
            "gonk.backup.every = \"PT24H\"\n",
            &homes(),
        )
        .expect_err("ISO 8601 is not this grammar");
        assert!(iso.contains("gonk.backup.every"), "{iso}");
        let churn = settings(&Flags::default(), "gonk.backup.every = \"24s\"\n", &homes())
            .expect_err("a sub-minute cadence");
        assert!(churn.contains("under a minute"), "{churn}");
        let zero = settings(&Flags::default(), "gonk.backup.keep = \"0\"\n", &homes())
            .expect_err("a rotation that keeps nothing");
        assert!(zero.contains("delete the backup it just took"), "{zero}");
    }

    #[test]
    fn no_config_is_loopback_1060_with_the_default_ledger() {
        let settings = settings(&Flags::default(), "", &homes()).unwrap();
        assert_eq!(settings.http, addr("127.0.0.1:1060"));
        assert_eq!(settings.socket, PathBuf::from("/home/u/.ikigai/gonk.sock"));
        assert_eq!(settings.quic, QuicBind::Default(addr("0.0.0.0:1060")));
        // Naming the bind — even to the default address — is a decision.
        let named = super::settings(
            &Flags::default(),
            "gonk.quic.bind = \"0.0.0.0:1060\"\n",
            &homes(),
        )
        .unwrap();
        assert_eq!(named.quic, QuicBind::Explicit(addr("0.0.0.0:1060")));
        let flagged = Flags {
            quic_bind: Some("127.0.0.1:4000".into()),
            ..Flags::default()
        };
        assert_eq!(
            super::settings(&flagged, "", &homes()).unwrap().quic,
            QuicBind::Explicit(addr("127.0.0.1:4000"))
        );
        assert_eq!(settings.http_ledgers, ["default"]);
    }

    #[test]
    fn bind_resolution_walks_flags_then_config_then_default() {
        assert_eq!(
            resolve_bind(Some("[::1]:9001"), None, "gonk.port = 7\n"),
            Ok(addr("[::1]:9001"))
        );
        // A port flag means loopback even against a config bind.
        assert_eq!(
            resolve_bind(None, Some(9002), "gonk.bind = \"127.0.0.2:1\"\n"),
            Ok(addr("127.0.0.1:9002"))
        );
        assert_eq!(
            resolve_bind(None, None, "gonk.port = '9999'\n"),
            Ok(addr("127.0.0.1:9999"))
        );
        assert!(resolve_bind(Some("127.0.0.1:1"), Some(2), "").is_err());
        assert!(resolve_bind(None, None, "gonk.bind = \"127.0.0.1:1\"\ngonk.port = 2\n").is_err());
        assert!(resolve_bind(None, None, "gonk.bind = \"localhost:1060\"\n").is_err());
    }

    /// ★ The decision: an unauthenticated door never faces a network.
    #[test]
    fn a_non_loopback_http_bind_is_refused() {
        for spelled in ["0.0.0.0:1060", "192.168.1.10:1060", "[::]:1060"] {
            let flags = Flags {
                bind: Some(spelled.to_string()),
                ..Flags::default()
            };
            let refused = settings(&flags, "", &homes()).unwrap_err();
            assert!(refused.contains("loopback only"), "{spelled}: {refused}");
            assert!(
                refused.contains("QUIC"),
                "the refusal names the door that works"
            );
        }
        let refused = settings(
            &Flags::default(),
            "gonk.bind = \"0.0.0.0:1060\"\n",
            &homes(),
        );
        assert!(refused.is_err(), "the config spelling is refused too");
        assert!(settings(
            &Flags {
                bind: Some("[::1]:1060".into()),
                ..Flags::default()
            },
            "",
            &homes()
        )
        .is_ok());
    }

    #[test]
    fn an_unknown_gonk_key_is_an_error_and_other_keys_are_not_ours() {
        let refused = settings(&Flags::default(), "gonk.prot = 1060\n", &homes()).unwrap_err();
        assert!(refused.contains("gonk.prot"), "{refused}");
        // The shared config.toml carries everyone's keys; only gonk.* is checked.
        assert!(settings(
            &Flags::default(),
            "mount = \"prefer urn:x:=y\"\nweb.port = 8642\n",
            &homes()
        )
        .is_ok());
    }

    /// A socket path the kernel cannot bind is refused before the store is opened.
    #[test]
    fn an_unbindable_socket_path_is_refused_up_front() {
        let long = format!("/{}/gonk.sock", "d".repeat(SOCKET_PATH_LIMIT));
        let flags = Flags {
            socket: Some(long),
            ..Flags::default()
        };
        let refused = settings(&flags, "", &homes()).unwrap_err();
        assert!(refused.contains("shorter than 104"), "{refused}");
    }

    #[test]
    fn repeatable_flags_replace_the_config_lines() {
        let text = "gonk.http.ledger = \"acme\"\ngonk.http.ledger = \"bosatsu\"\ngonk.socket = \"~/s/g.sock\"\n";
        let from_file = settings(&Flags::default(), text, &homes()).unwrap();
        assert_eq!(from_file.http_ledgers, ["acme", "bosatsu"]);
        assert_eq!(from_file.socket, PathBuf::from("/home/u/s/g.sock"));
        let flags = Flags {
            http_ledgers: vec!["default".into()],
            no_quic: true,
            ..Flags::default()
        };
        let overridden = settings(&flags, text, &homes()).unwrap();
        assert_eq!(overridden.http_ledgers, ["default"]);
        assert_eq!(overridden.quic, QuicBind::Off);
    }

    /// The mount key, end to end: the cli's spelling, at most one per prefix, and a flag
    /// that replaces the config lines wholesale like every other repeatable one.
    #[test]
    fn the_mount_key_reads_the_clis_spelling() {
        let text = "gonk.mount = \"prefer urn:llm:=~/.ikigai/host.sock\"\n";
        let settings = settings(&Flags::default(), text, &homes()).unwrap();
        assert_eq!(
            settings.mounts,
            [Mount {
                prefix: "urn:llm:".to_string(),
                target: crate::mount::Target::Socket(PathBuf::from("/home/u/.ikigai/host.sock")),
            }]
        );
        assert!(!settings.explains(), "no browse root, nothing to explain");

        let twice = format!("{text}{text}");
        let refused = super::settings(&Flags::default(), &twice, &homes()).unwrap_err();
        assert!(refused.contains("mounted twice"), "{refused}");

        let flagged = Flags {
            mounts: vec!["prefer urn:llm:=/tmp/other.sock".into()],
            ..Flags::default()
        };
        assert_eq!(
            super::settings(&flagged, text, &homes()).unwrap().mounts[0].target,
            crate::mount::Target::Socket(PathBuf::from("/tmp/other.sock"))
        );
        assert!(super::settings(&Flags::default(), "", &homes())
            .unwrap()
            .mounts
            .is_empty());
    }

    /// ★ The ceilings are the per-call half of the spend gate, so a typo in one must stop the
    /// server rather than silently leave the default in place.
    #[test]
    fn the_explain_tiers_default_to_browses_own_and_are_checked_when_set() {
        let defaults = settings(&Flags::default(), "", &homes()).unwrap().explain;
        assert_eq!(defaults, ExplainTiers::default());
        assert_eq!(defaults.file.provider, "urn:llm:coder:ask");
        assert_eq!(defaults.review.max_tokens, 800);

        let text = "gonk.explain.file.provider = \"urn:llm:qwen:ask\"\n\
                    gonk.explain.file.max_tokens = 250\n\
                    gonk.explain.max_prompt_bytes = 8192\n";
        let tiers = super::settings(&Flags::default(), text, &homes())
            .unwrap()
            .explain;
        assert_eq!(tiers.file.provider, "urn:llm:qwen:ask");
        assert_eq!(tiers.file.max_tokens, 250);
        assert_eq!(tiers.max_prompt_bytes, 8192);
        // …and the untouched tiers keep browse's defaults.
        assert_eq!(tiers.dir, ExplainTiers::default().dir);

        for (spelling, expected) in [
            (
                "gonk.explain.file.provider = \"urn:mistral:ask\"\n",
                "urn:llm:",
            ),
            (
                "gonk.explain.dir.max_tokens = \"lots\"\n",
                "is not a token count",
            ),
            ("gonk.explain.pr.max_tokens = 0\n", "is not a token count"),
            (
                "gonk.explain.max_prompt_bytes = -1\n",
                "is not a byte count",
            ),
        ] {
            let refused = super::settings(&Flags::default(), spelling, &homes()).unwrap_err();
            assert!(refused.contains(expected), "{spelling}: {refused}");
        }
    }

    /// `gonk.review.judge` (ledger #696): browse's own default when no line is written, a
    /// provider under `urn:llm:` when one is, no judge at all for `off` — and anything else,
    /// a blank included, stops the server naming the line.
    #[test]
    fn the_judge_key_names_a_provider_or_off_and_nothing_else() {
        let judge = |text: &str| {
            super::settings(&Flags::default(), text, &homes()).map(|s| s.explain.judge)
        };
        assert_eq!(judge("").unwrap().as_deref(), Some("urn:llm:coder:ask"));
        assert_eq!(
            judge("gonk.review.judge = \"urn:llm:coder-next:ask\"\n")
                .unwrap()
                .as_deref(),
            Some("urn:llm:coder-next:ask")
        );
        assert_eq!(judge("gonk.review.judge = \"off\"\n").unwrap(), None);
        // The judge is set independently of the review queue: no space line is needed.
        assert!(judge("gonk.review.judge = \"off\"\n").is_ok());
        for spelling in [
            "gonk.review.judge = \"urn:mistral:ask\"\n",
            "gonk.review.judge = \"\"\n",
            "gonk.review.judge = \"urn:llm:\"\n",
        ] {
            let refused = judge(spelling).unwrap_err();
            assert!(
                refused.contains("gonk.review.judge") && refused.contains("off"),
                "{spelling}: {refused}"
            );
        }
    }

    #[test]
    fn subcommands_parse() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        match parse_args(args(
            "client add laptop --ledger default=write --ledger acme=read",
        ))
        .unwrap()
        {
            Command::ClientAdd {
                name,
                ledgers,
                browse_graph,
                cert,
                force,
            } => {
                assert_eq!(name, "laptop");
                assert_eq!(
                    ledgers,
                    [
                        ("default".to_string(), Authority::Write),
                        ("acme".to_string(), Authority::Read)
                    ]
                );
                assert!(cert.is_none() && !force && browse_graph.is_none());
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_args(args("grants acme delete")).unwrap(),
            Command::Grants {
                authority: Authority::Delete,
                ..
            }
        ));
        assert!(matches!(
            parse_args(args("serve --port 1070")).unwrap(),
            Command::Serve(_)
        ));
        assert!(parse_args(args("client add laptop --ledger default=admin")).is_err());
        assert!(parse_args(args("--bogus")).is_err());
    }

    /// ★ The browse graph is a SUBJECT of a grant, not a ledger — every spelling, because
    /// the tokens it mints are the point of the graph decision and an operator who cannot
    /// spell the command holds none of them.
    #[test]
    fn the_browse_graph_is_grantable_on_every_minting_path() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        match parse_args(args("grants --browse-graph read")).unwrap() {
            Command::Grants { ledger, authority } => {
                assert_eq!(ledger, None, "its subject is not a ledger name");
                assert_eq!(authority, Authority::Read);
            }
            other => panic!("{other:?}"),
        }
        // The authority is optional here exactly as it is for a ledger.
        assert!(matches!(
            parse_args(args("grants --browse-graph")).unwrap(),
            Command::Grants {
                ledger: None,
                authority: Authority::Write
            }
        ));
        match parse_args(args("client add laptop --browse-graph read")).unwrap() {
            Command::ClientAdd {
                ledgers,
                browse_graph,
                ..
            } => {
                assert!(ledgers.is_empty());
                assert_eq!(browse_graph, Some(Authority::Read));
            }
            other => panic!("{other:?}"),
        }
        match parse_args(args(
            "passkey invite reader --ledger default=read --browse-graph read",
        ))
        .unwrap()
        {
            Command::PasskeyInvite {
                ledgers,
                browse_graph,
                ..
            } => {
                assert_eq!(ledgers, [("default".to_string(), Authority::Read)]);
                assert_eq!(browse_graph, Some(Authority::Read));
            }
            other => panic!("{other:?}"),
        }
        // ⚠ `browse` is a name `Ledger::parse` accepts, so the subject had to be a flag: this
        // asserts the ledger spelling still means a LEDGER called browse.
        assert!(matches!(
            parse_args(args("grants browse read")).unwrap(),
            Command::Grants { ledger: Some(name), .. } if name == "browse"
        ));
        assert!(parse_args(args("grants --browse read")).is_err(), "a typo");
        assert!(parse_args(args("client add x --browse-graph nonsense")).is_err());
    }
}
