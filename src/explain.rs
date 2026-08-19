//! `urn:repo:{repo}:explain[:{path}]` — LLM-derived **orientation
//! explanations** of files and directories, ARCHIVED so each one is derived
//! once per content version and reused forever. The economic heart of the
//! browser: pay tokens only for what changed.
//!
//! ## Derived through the kernel
//!
//! The endpoint never reads the filesystem or the network directly: it
//! `source`s `urn:repo:{repo}:hash:{path}` (the archive key),
//! `urn:repo:{repo}:file:{path}` / `urn:repo:{repo}:tree:{path}` (the
//! material), and the children's own `explain` resources (the hierarchy), and
//! `issue`s `urn:llm:{provider}:ask` for the derivation — so every sub-request
//! is dependency-recorded and freshness ties itself.
//!
//! ## Archive ≠ cache
//!
//! Results persist as RDF in an Oxigraph store the HOST injects
//! ([`ExplainConfig::new`]) — one shared store; S2's annotations join it.
//! The key is `(path, content-hash, version-tag)` where the tag folds the
//! prompt version and the model identity (`code-v1@qwen3-coder:30b`). Prior
//! versions STAY ADDRESSABLE: default resolve = the current tag, `version=`
//! fetches an older one, and `urn:repo:{repo}:explain-versions:{path}` lists
//! what the archive holds.
//!
//! ## Model identity in the tag
//!
//! The `@model` half of the tag is resolved, in precedence order: an explicit
//! [`ExplainConfig::file_model_label`] / [`ExplainConfig::dir_model_label`]
//! (the operator's override) → the provider's `urn:llm:{provider}:model`
//! resource sourced through the kernel (ikigai-llm ≥ 0.10 — the TRUE
//! configured model id, so a model swap re-keys the archive without any
//! browse-side config) → the provider-IRI heuristic (`urn:llm:coder:ask` ⇒
//! `coder`) when neither exists. Resolution failure never fails the explain —
//! tags degrade to the heuristic. CAVEAT: `:model` reports the provider's
//! CONFIGURED default, not necessarily the model that answered a specific ask
//! (an explicit `model=` arg or ikigai-llm's cheapest-installed 404 fallback
//! can differ); for version tags the configured identity is the intended
//! semantics — per-response truth lives in the ask's `as=application/json`
//! envelope.
//!
//! ## Choosing the backend per request
//!
//! `provider={iri}` derives THIS explanation against a backend the caller
//! names rather than the tier's configured one. Three things about it:
//!
//! - **Authority is the operator's, not the caller's.** The selectable set is
//!   the two configured tier providers plus whatever
//!   [`ExplainConfig::allow_provider`] added; anything else is `Denied`,
//!   naming what was asked for and what is on offer. It never falls back
//!   silently. The reason is that `explain`'s declared capability
//!   (`urn:cap:net:*`) cannot vary by argument value — it means "may derive",
//!   not "may derive against the metered vendor" — so the argument would
//!   otherwise let anyone who may explain spend the operator's money.
//! - **The label follows the backend that answered** (see
//!   [`ExplainEndpoint::model_label`]): a request-selected provider does NOT
//!   inherit an operator's `file_model_label`.
//! - **★ The tag folds MODEL identity, not backend identity.** Two providers
//!   serving the same model id produce the same tag and therefore the same
//!   archive key: the second is a HIT, not a second entry. That is deliberate
//!   — the explanation is a function of the model and the prompt, not of the
//!   serving layer — and it means `provider=` buys a genuinely different
//!   explanation only when the backends differ in model, while against the
//!   same model it buys latency/throughput shape and costs nothing new.
//!
//! ## The option menu (`explain-versions … as=text/html`)
//!
//! The HTML faces carry a `<details>` disclosure beside their explain button.
//! Its panel is `urn:repo:{repo}:explain-versions[:{path}]` under
//! `as=text/html`, fetched when the disclosure opens: the archived entries the
//! CURRENT content can reopen (free — a store read) above the models this host
//! will derive a new one with (one model call each). **One row per MODEL**,
//! because that is what the archive keys on — grouping by backend would offer
//! a second entry that cannot exist. Opening a menu costs one archive read,
//! one content hash, and ONE resolve of `urn:llm:models` — never one probe per
//! backend, and never anything at all until a human opens it.
//!
//! ## Hierarchical grains, merkle keys
//!
//! A directory's explanation derives from its entry list plus its CHILDREN'S
//! explanations (sourced through the kernel — recursion bottoms at files),
//! and its content hash is the merkle construction of `urn:repo:…:hash` —
//! so one file edit re-keys and re-derives exactly the path from that file to
//! the root; siblings are archive hits.
//!
//! ## Model tiers (2026-08-07 bake-off)
//!
//! File grain → the `coder` provider (`urn:llm:coder:ask`); directory rollup →
//! the default `urn:llm:ask`. Provider IDs are CONFIG, never hardcoded.
//! Per-call `max_tokens` ceilings are MANDATORY (thinking models burn a small
//! budget on reasoning and return nothing): file ~400, rollup ~600, both
//! config. `temperature=0.2`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Iri, Representation,
    Request, Result, Verb,
};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use oxigraph::store::Store;

use crate::annotate::{self, Included, TargetFilter};
use crate::hash::hash_iri;
use crate::{
    crumbs_html, esc, file_iri, granted, human_size, include_annotations, iri_encode,
    media_type_for, path_binding, repo_root, repr, repr_utf8, resolve, tree_iri, ttl_str, Roots,
    CAP_WILDCARD,
};

/// The network capability the explain action declares (wildcard offering
/// form): deriving calls an LLM backend, which is a network act even against
/// localhost. Enforced by the kernel's baseline (declared = enforced) — a
/// capability with no `urn:cap:net:…` grant is denied before dispatch.
pub const CAP_NET: &str = "urn:cap:net:*";

// --- prompts (each constant is versioned; edit ⇒ bump its version) ----------

/// The shared system prompt for every grain. Editing it changes every
/// explanation, so bump ALL the per-kind versions below when it changes.
pub(crate) const SYSTEM_PROMPT: &str =
    "You are the explanation layer of a repository browser. Write a concise \
     orientation for a developer seeing this item for the first time: what it \
     is, what it is for, and what stands out. Plain prose, no headings, no \
     bullet lists, no preamble — start directly with the substance.";

/// Version of [`CODE_PROMPT`] (folded into the archive key: a prompt edit
/// bumps this, old explanations stay addressable under the old tag).
const CODE_PROMPT_VERSION: &str = "code-v1";
/// The file-grain prompt for source code.
const CODE_PROMPT: &str =
    "Explain this source file: its purpose, main constructs, and how it fits \
     the codebase around it.";

/// Version of [`NOTE_PROMPT`].
const NOTE_PROMPT_VERSION: &str = "note-v1";
/// The file-grain prompt for markdown/org notes and documents.
const NOTE_PROMPT: &str =
    "Explain this note or document: its subject, key points, and who would read it.";

/// Version of [`SKILL_PROMPT`].
const SKILL_PROMPT_VERSION: &str = "skill-v1";
/// The file-grain prompt for agent/skill definitions (CLAUDE.md, AGENTS.md,
/// .claude/… files).
const SKILL_PROMPT: &str =
    "Explain this agent or skill definition: what behavior it configures, for \
     which tool or agent, and the rules it imposes.";

/// Version of [`TEXT_PROMPT`].
const TEXT_PROMPT_VERSION: &str = "text-v1";
/// The file-grain prompt for plain text that is neither code nor a note.
const TEXT_PROMPT: &str = "Explain this text file: what it contains and what role it plays in the \
     repository.";

/// Version of [`DIR_PROMPT`].
const DIR_PROMPT_VERSION: &str = "dir-v1";
/// The directory-grain rollup prompt (fed the entry list and the children's
/// explanations).
const DIR_PROMPT: &str =
    "Explain this directory as a whole: what this part of the repository does \
     and how its entries relate. Synthesize from the entry list and the \
     per-entry explanations below; do not re-describe every entry.";

/// The version tag of canned binary stubs (no model involved, so no `@model`).
const BINARY_TAG: &str = "binary-v1";

// --- configuration ----------------------------------------------------------

/// Configuration for the explanation family — the store handle plus the model
/// tiers. Everything has a default except the store: the archive must outlive
/// this process by the HOST's choice, so the handle is always injected.
///
/// ```no_run
/// # use std::sync::Arc;
/// let store = Arc::new(oxigraph::store::Store::new().unwrap());
/// let config = ikigai_browse::ExplainConfig::new(store)
///     .file_model_label("qwen3-coder:30b")
///     .dir_model_label("llama3.3:70b");
/// ```
#[derive(Clone)]
pub struct ExplainConfig {
    pub(crate) store: Arc<Store>,
    file_provider: String,
    dir_provider: String,
    pub(crate) review_provider: String,
    pub(crate) pr_provider: String,
    file_model_label: Option<String>,
    dir_model_label: Option<String>,
    pub(crate) review_model_label: Option<String>,
    pub(crate) pr_model_label: Option<String>,
    file_max_tokens: u32,
    dir_max_tokens: u32,
    pub(crate) review_max_tokens: u32,
    pub(crate) pr_max_tokens: u32,
    pub(crate) temperature: String,
    /// Provider IRIs a REQUEST may select beyond the two configured tiers —
    /// the operator's opt-in half of [`ExplainConfig::selectable`].
    selectable_providers: BTreeSet<String>,
    pub(crate) ignore: BTreeSet<String>,
    pub(crate) max_prompt_bytes: usize,
}

impl ExplainConfig {
    /// A config over the host's shared Oxigraph store, with the design-of-record
    /// defaults: file grain → `urn:llm:coder:ask` at 400 tokens, directory
    /// rollup → `urn:llm:ask` at 600, the review passes → `urn:llm:coder:ask` at
    /// 800 (findings carry quotes — they need headroom), the pull-request
    /// explain → `urn:llm:coder:ask` at 600 (a diff walk reads like code but
    /// summarizes like a rollup), `temperature=0.2`, the standard ignore set,
    /// prompts fed at most 16 KiB of content.
    pub fn new(store: Arc<Store>) -> Self {
        ExplainConfig {
            store,
            file_provider: "urn:llm:coder:ask".to_string(),
            dir_provider: "urn:llm:ask".to_string(),
            review_provider: "urn:llm:coder:ask".to_string(),
            pr_provider: "urn:llm:coder:ask".to_string(),
            file_model_label: None,
            dir_model_label: None,
            review_model_label: None,
            pr_model_label: None,
            file_max_tokens: 400,
            dir_max_tokens: 600,
            review_max_tokens: 800,
            pr_max_tokens: 600,
            temperature: "0.2".to_string(),
            selectable_providers: BTreeSet::new(),
            ignore: crate::hash::default_ignore(),
            max_prompt_bytes: 16 * 1024,
        }
    }

    /// The provider IRI the file grain asks (default `urn:llm:coder:ask`).
    pub fn file_provider(mut self, iri: impl Into<String>) -> Self {
        self.file_provider = iri.into();
        self
    }

    /// The provider IRI the directory rollup asks (default `urn:llm:ask`).
    pub fn dir_provider(mut self, iri: impl Into<String>) -> Self {
        self.dir_provider = iri.into();
        self
    }

    /// The provider IRI the review pass asks (default `urn:llm:coder:ask` —
    /// the same coder tier as the file grain).
    pub fn review_provider(mut self, iri: impl Into<String>) -> Self {
        self.review_provider = iri.into();
        self
    }

    /// The model identity folded into file-grain version tags (e.g.
    /// `"qwen3-coder:30b"` ⇒ tag `code-v1@qwen3-coder:30b`) — the OPERATOR'S
    /// OVERRIDE, taking precedence over the resolved
    /// `urn:llm:{provider}:model` identity. Unset (the default), the true
    /// configured model id is resolved through the kernel at explain time, so
    /// a model swap re-keys the archive with no browse-side config; set a
    /// label only to pin tags independently of what the llm module reports.
    pub fn file_model_label(mut self, label: impl Into<String>) -> Self {
        self.file_model_label = Some(label.into());
        self
    }

    /// The model identity folded into directory-grain version tags — the
    /// operator's override, with the same precedence as
    /// [`Self::file_model_label`].
    pub fn dir_model_label(mut self, label: impl Into<String>) -> Self {
        self.dir_model_label = Some(label.into());
        self
    }

    /// The file-grain `max_tokens` ceiling (default 400). Mandatory on every
    /// call — thinking models with no ceiling burn the budget on reasoning
    /// and return nothing.
    pub fn file_max_tokens(mut self, tokens: u32) -> Self {
        self.file_max_tokens = tokens;
        self
    }

    /// The directory-rollup `max_tokens` ceiling (default 600).
    pub fn dir_max_tokens(mut self, tokens: u32) -> Self {
        self.dir_max_tokens = tokens;
        self
    }

    /// The model identity folded into review version tags — the operator's
    /// override, with the same precedence as [`Self::file_model_label`].
    pub fn review_model_label(mut self, label: impl Into<String>) -> Self {
        self.review_model_label = Some(label.into());
        self
    }

    /// The review pass's `max_tokens` ceiling (default 800 — each finding
    /// carries a verbatim quote plus commentary).
    pub fn review_max_tokens(mut self, tokens: u32) -> Self {
        self.review_max_tokens = tokens;
        self
    }

    /// The provider IRI the pull-request explain asks (default
    /// `urn:llm:coder:ask` — a diff reads like code).
    pub fn pr_provider(mut self, iri: impl Into<String>) -> Self {
        self.pr_provider = iri.into();
        self
    }

    /// The model identity folded into pull-request explain version tags — the
    /// operator's override, with the same precedence as
    /// [`Self::file_model_label`].
    pub fn pr_model_label(mut self, label: impl Into<String>) -> Self {
        self.pr_model_label = Some(label.into());
        self
    }

    /// The pull-request explain's `max_tokens` ceiling (default 600).
    pub fn pr_max_tokens(mut self, tokens: u32) -> Self {
        self.pr_max_tokens = tokens;
        self
    }

    /// Sampling temperature passed to every ask (default `0.2`).
    pub fn temperature(mut self, temperature: impl Into<String>) -> Self {
        self.temperature = temperature.into();
        self
    }

    /// Widen the set of provider IRIs a REQUEST may name with `provider=`,
    /// beyond the two this endpoint already asks ([`Self::file_provider`] and
    /// [`Self::dir_provider`] are always selectable — naming one of them
    /// points the host at a backend it already uses to explain, so it grants
    /// no reach the caller did not already have).
    ///
    /// ★ THIS IS THE AUTHORITY BOUNDARY. `explain` declares one network
    /// capability (`urn:cap:net:*`) and an `ActionSpec` cannot express a
    /// capability that varies by ARGUMENT VALUE, so the cap says only "may
    /// derive" — it cannot say "may derive against the metered vendor". A
    /// caller who may explain at all could otherwise direct the host at any
    /// backend the registry happens to hold, which the day a remote,
    /// api-keyed provider is configured means spending the operator's money
    /// and shipping the operator's source to a vendor. The default set is
    /// therefore exactly what the operator already chose; widening it is the
    /// operator's explicit act, and the widened set is published in the
    /// action's `provider` ArgSpec (`one_of`) so the manifold states it.
    pub fn allow_provider(mut self, iri: impl Into<String>) -> Self {
        self.selectable_providers.insert(iri.into());
        self
    }

    /// [`Self::allow_provider`] for several at once.
    pub fn allow_providers(mut self, iris: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.selectable_providers
            .extend(iris.into_iter().map(Into::into));
        self
    }

    /// Every provider IRI a `provider=` argument may name here: the two
    /// configured tiers plus whatever [`Self::allow_provider`] added.
    /// Computed on demand rather than snapshotted, so builder order never
    /// matters and a later `file_provider(…)` stays selectable.
    pub(crate) fn selectable(&self) -> BTreeSet<String> {
        let mut set = self.selectable_providers.clone();
        set.insert(self.file_provider.clone());
        set.insert(self.dir_provider.clone());
        set
    }

    /// Entry names excluded from the hierarchy — from directory hashing,
    /// rollups, and tree-feeding (default: `.git`, `target`, `node_modules`,
    /// `venv`, `.venv`, `dist`, `__pycache__`).
    pub fn ignore(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.ignore = names.into_iter().collect();
        self
    }

    /// How much of a file's content (or a rollup's material) is fed to the
    /// model before truncation (default 16 KiB). Part of prompt shape: bump
    /// the prompt versions if you change truncation policy semantics.
    pub fn max_prompt_bytes(mut self, bytes: usize) -> Self {
        self.max_prompt_bytes = bytes;
        self
    }
}

/// The last-resort model label when nothing resolves: the provider IRI's
/// middle segment (`urn:llm:coder:ask` ⇒ `coder`, `urn:llm:ask` ⇒ `ask`),
/// else the whole IRI.
pub(crate) fn provider_label(provider: &str) -> String {
    let tail = provider.strip_prefix("urn:llm:").unwrap_or(provider);
    tail.strip_suffix(":ask").unwrap_or(tail).to_string()
}

/// The TRUE model identity for an ask IRI, through the kernel — `None` when
/// it cannot be determined (llm module absent, a pre-0.10 host without
/// `:model`, an unrecognized provider IRI).
///
/// `urn:llm:{p}:ask` ⇒ one resolve of `urn:llm:{p}:model` (ikigai-llm 0.10's
/// identity face — cacheable on the llm side, so per-child repeats during a
/// rollup are kernel cache hits). The bare facade `urn:llm:ask` has no
/// provider segment and 0.10 binds no `urn:llm:model` — for it the default
/// provider name is read from `urn:llm:config` (also cacheable) and THAT
/// provider's `:model` resolved: two cheap hops instead of one.
pub(crate) async fn resolve_model(inv: &Invocation<'_>, provider: &str) -> Option<String> {
    if let Some(p) = provider
        .strip_prefix("urn:llm:")
        .and_then(|tail| tail.strip_suffix(":ask"))
    {
        return source_str(inv, &format!("urn:llm:{p}:model")).await;
    }
    if provider == "urn:llm:ask" {
        let config = source_str(inv, "urn:llm:config").await?;
        let parsed: serde_json::Value = serde_json::from_str(&config).ok()?;
        let default = parsed.get("default")?.as_str()?;
        return source_str(inv, &format!("urn:llm:{default}:model")).await;
    }
    None
}

/// Source an IRI and return its trimmed UTF-8 body — `None` on any failure or
/// an empty body (an empty model id must not produce `code-v1@` tags).
async fn source_str(inv: &Invocation<'_>, iri: &str) -> Option<String> {
    let iri = Iri::parse(iri).ok()?;
    let repr = inv.source(&iri).await.ok()?;
    let text = String::from_utf8_lossy(&repr.bytes).trim().to_string();
    (!text.is_empty()).then_some(text)
}

// --- grains and classification ----------------------------------------------

/// What kind of thing is being explained — picks the prompt, its version, and
/// the model tier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grain {
    Code,
    Note,
    Skill,
    Text,
    Binary,
    Directory,
}

impl Grain {
    fn label(self) -> &'static str {
        match self {
            Grain::Code => "code",
            Grain::Note => "note",
            Grain::Skill => "skill",
            Grain::Text => "text",
            Grain::Binary => "binary",
            Grain::Directory => "directory",
        }
    }
}

const NOTE_EXTS: &[&str] = &["md", "markdown", "org", "rst", "adoc", "txt"];
const CODE_EXTS: &[&str] = &[
    "rs", "py", "js", "mjs", "ts", "jsx", "tsx", "go", "c", "h", "cpp", "hpp", "cc", "java", "rb",
    "sh", "fish", "swift", "kt", "scala", "sql", "html", "htm", "css", "toml", "yaml", "yml",
    "json", "jsonld", "xml", "ttl", "nt", "sparql", "lisp", "scm", "el", "ex", "exs", "zig", "lua",
    "pl", "r",
];

/// Classify a (UTF-8) file by extension + path heuristics. Skill/agent
/// definitions win over the note extension: `CLAUDE.md`, `AGENTS.md`,
/// `AGENT.md`, `SKILL.md` by name, or anything under a `.claude` directory.
fn classify_file(rel: &str, utf8: bool) -> Grain {
    if !utf8 {
        return Grain::Binary;
    }
    let path = Path::new(rel);
    let base = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(base, "CLAUDE.md" | "AGENTS.md" | "AGENT.md" | "SKILL.md")
        || path.components().any(|c| c.as_os_str() == ".claude")
    {
        return Grain::Skill;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if NOTE_EXTS.contains(&ext) => Grain::Note,
        Some(ext) if CODE_EXTS.contains(&ext) => Grain::Code,
        _ => Grain::Text,
    }
}

/// The archive version tag for a grain: `{prompt-version}@{model-label}`
/// (binary stubs involve no model, so no `@`). The model half is resolved
/// once per explain request ([`ExplainEndpoint::model_label`]) and passed in
/// — the same value keys the archive lookup and stamps any new entry.
fn version_tag(grain: Grain, model: &str) -> String {
    match grain {
        Grain::Code => format!("{CODE_PROMPT_VERSION}@{model}"),
        Grain::Note => format!("{NOTE_PROMPT_VERSION}@{model}"),
        Grain::Skill => format!("{SKILL_PROMPT_VERSION}@{model}"),
        Grain::Text => format!("{TEXT_PROMPT_VERSION}@{model}"),
        Grain::Binary => BINARY_TAG.to_string(),
        Grain::Directory => format!("{DIR_PROMPT_VERSION}@{model}"),
    }
}

fn grain_prompt(grain: Grain) -> &'static str {
    match grain {
        Grain::Code => CODE_PROMPT,
        Grain::Note => NOTE_PROMPT,
        Grain::Skill => SKILL_PROMPT,
        Grain::Text => TEXT_PROMPT,
        Grain::Directory => DIR_PROMPT,
        Grain::Binary => unreachable!("binary stubs are canned, not prompted"),
    }
}

/// Truncate at a char boundary with an explicit marker — silently feeding a
/// model half a file invites confident nonsense about the missing half.
pub(crate) fn truncate(text: &str, max_bytes: usize) -> String {
    let end = truncated_len(text, max_bytes);
    if end == text.len() {
        return text.to_string();
    }
    format!("{}\n… (content truncated)", &text[..end])
}

/// How many bytes of `text` [`truncate`] actually feeds the model — the
/// honest-reporting number (`reviewed_bytes`) the review passes record next
/// to the input's full size.
pub(crate) fn truncated_len(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return text.len();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

// --- the archive (RDF in the shared store) ----------------------------------

pub(crate) const IK: &str = "https://ikigai-rs.dev/ns#";

pub(crate) fn ik(term: &str) -> NamedNode {
    NamedNode::new(format!("{IK}{term}")).expect("ik terms are valid IRIs")
}

/// One archived explanation — the RDF shape of an archive entry (skolemized;
/// no blank nodes):
///
/// ```turtle
/// <urn:ikigai:browse:explain:{repo}:{hash}:{tag}:{path}> a ik:Explanation ;
///     ik:repo "demo" ; ik:path "src/lib.rs" ;
///     ik:about <urn:repo:demo:file:src/lib.rs> ;
///     ik:contentHash "sha256:…" ; ik:versionTag "code-v1@qwen3-coder:30b" ;
///     ik:model "qwen3-coder:30b" ; ik:promptKind "code" ;
///     ik:explanation "…the text…" ;
///     ik:derivedAt "2026-08-08T17:00:00.000Z"^^xsd:dateTime .
/// ```
pub(crate) struct ArchiveEntry {
    pub(crate) iri: String,
    pub(crate) repo: String,
    pub(crate) rel: String,
    pub(crate) target_iri: String,
    pub(crate) hash: String,
    pub(crate) tag: String,
    pub(crate) model: String,
    pub(crate) kind: String,
    pub(crate) text: String,
    pub(crate) derived_at: Option<String>,
}

/// The stable, addressable archive key. Tag and path are percent-encoded
/// (they may carry spaces or non-URN characters); the hash and repo embed
/// cleanly by construction.
pub(crate) fn entry_iri(repo: &str, rel: &str, hash: &str, tag: &str) -> String {
    format!(
        "urn:ikigai:browse:explain:{repo}:{hash}:{}:{}",
        iri_encode(tag),
        iri_encode(rel)
    )
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: explanation archive: {e}"))
}

pub(crate) fn store_entry(store: &Store, entry: &ArchiveEntry) -> Result<()> {
    let subject = NamedNode::new(&entry.iri).map_err(store_err)?;
    let target = NamedNode::new(&entry.target_iri).map_err(store_err)?;
    let mut quads: Vec<Quad> = vec![
        Quad::new(
            subject.clone(),
            oxigraph::model::vocab::rdf::TYPE,
            ik("Explanation"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("repo"),
            Literal::new_simple_literal(&entry.repo),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("path"),
            Literal::new_simple_literal(&entry.rel),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("about"),
            target,
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("contentHash"),
            Literal::new_simple_literal(&entry.hash),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("versionTag"),
            Literal::new_simple_literal(&entry.tag),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("model"),
            Literal::new_simple_literal(&entry.model),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("promptKind"),
            Literal::new_simple_literal(&entry.kind),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            subject.clone(),
            ik("explanation"),
            Literal::new_simple_literal(&entry.text),
            GraphName::DefaultGraph,
        ),
    ];
    if let Some(at) = &entry.derived_at {
        quads.push(Quad::new(
            subject,
            ik("derivedAt"),
            Literal::new_typed_literal(at, oxigraph::model::vocab::xsd::DATE_TIME),
            GraphName::DefaultGraph,
        ));
    }
    for quad in &quads {
        store.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Load one archived entry by its key IRI — `None` on a miss (no
/// `ik:explanation` triple under that subject).
pub(crate) fn load_entry(store: &Store, iri: &str) -> Result<Option<ArchiveEntry>> {
    let subject = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    let mut entry = ArchiveEntry {
        iri: iri.to_string(),
        repo: String::new(),
        rel: String::new(),
        target_iri: String::new(),
        hash: String::new(),
        tag: String::new(),
        model: String::new(),
        kind: String::new(),
        text: String::new(),
        derived_at: None,
    };
    let mut found = false;
    for quad in store.quads_for_pattern(Some(subject.as_ref().into()), None, None, None) {
        let quad = quad.map_err(store_err)?;
        let literal = |term: &Term| match term {
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        };
        match quad.predicate.as_str().strip_prefix(IK) {
            Some("explanation") => {
                entry.text = literal(&quad.object);
                found = true;
            }
            Some("repo") => entry.repo = literal(&quad.object),
            Some("path") => entry.rel = literal(&quad.object),
            Some("contentHash") => entry.hash = literal(&quad.object),
            Some("versionTag") => entry.tag = literal(&quad.object),
            Some("model") => entry.model = literal(&quad.object),
            Some("promptKind") => entry.kind = literal(&quad.object),
            Some("derivedAt") => entry.derived_at = Some(literal(&quad.object)),
            Some("about") => {
                if let Term::NamedNode(node) = &quad.object {
                    entry.target_iri = node.as_str().to_string();
                }
            }
            // Legacy term (pre-0.2.2 archives wrote ik:target; the routing
            // family owns that term now). Read-only compatibility: never
            // written, and ik:about wins if both are somehow present.
            Some("target") if entry.target_iri.is_empty() => {
                if let Term::NamedNode(node) = &quad.object {
                    entry.target_iri = node.as_str().to_string();
                }
            }
            _ => {}
        }
    }
    Ok(found.then_some(entry))
}

/// Every archived entry whose `ik:about` is the given resource — the
/// `explain-versions` listing. Also matches the legacy `ik:target` predicate
/// (pre-0.2.2 archives), so old entries stay listed without a migration.
/// Sorted newest-first by `ik:derivedAt` (entries without a timestamp sort
/// last), then by tag for determinism.
fn list_versions(store: &Store, target_iri: &str) -> Result<Vec<ArchiveEntry>> {
    let target = match NamedNode::new(target_iri) {
        Ok(node) => node,
        Err(_) => return Ok(Vec::new()),
    };
    let mut subjects = std::collections::BTreeSet::new();
    for predicate in [ik("about"), ik("target")] {
        for quad in store.quads_for_pattern(
            None,
            Some(predicate.as_ref()),
            Some(target.as_ref().into()),
            None,
        ) {
            let quad = quad.map_err(store_err)?;
            subjects.insert(quad.subject.to_string());
        }
    }
    let mut entries = Vec::new();
    for subject in &subjects {
        let iri = subject.trim_start_matches('<').trim_end_matches('>');
        if let Some(entry) = load_entry(store, iri)? {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        (b.derived_at.as_deref().unwrap_or(""), &a.tag, &a.hash).cmp(&(
            a.derived_at.as_deref().unwrap_or(""),
            &b.tag,
            &b.hash,
        ))
    });
    Ok(entries)
}

/// Epoch milliseconds → ISO 8601 UTC (`2026-08-08T17:00:00.000Z`), for
/// `ik:derivedAt`. Civil-from-days (Hinnant); no leap-second pretensions.
pub(crate) fn iso8601(millis: u64) -> String {
    let secs = millis / 1000;
    let ms = millis % 1000;
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let z = (secs / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe as i64 + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

// --- the explain endpoint ---------------------------------------------------

pub(crate) fn bind(space: EndpointSpace, roots: &Roots, config: ExplainConfig) -> EndpointSpace {
    let config = Arc::new(config);
    let versions: Arc<dyn Endpoint> = Arc::new(VersionsEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(&config),
    });
    let explain: Arc<dyn Endpoint> = Arc::new(ExplainEndpoint {
        roots: Arc::clone(roots),
        config,
    });
    let space = crate::bind_family(
        space,
        roots,
        versions,
        Some("explain-versions"),
        Some("explain-versions:{path}"),
    );
    crate::bind_family(
        space,
        roots,
        explain,
        Some("explain"),
        Some("explain:{path}"),
    )
}

pub(crate) fn explain_iri(repo: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("urn:repo:{repo}:explain")
    } else {
        format!("urn:repo:{repo}:explain:{}", iri_encode(rel))
    }
}

fn versions_iri(repo: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("urn:repo:{repo}:explain-versions")
    } else {
        format!("urn:repo:{repo}:explain-versions:{}", iri_encode(rel))
    }
}

struct ExplainEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

/// The result being served: the archived (or just-derived) entry plus whether
/// this resolution paid for a derivation.
struct Served {
    entry: ArchiveEntry,
    derived: bool,
}

#[async_trait]
impl Endpoint for ExplainEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-explain does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        let target = resolve(root, &rel)?;
        let config = &self.config;

        // The caller's backend choice, validated BEFORE any work: an unknown
        // or non-allowed provider is a refusal that names what was asked for,
        // never a silent fall back to the configured one. A caller who asked
        // for one model, got another, and had it archived under that other
        // model's tag has been lied to durably — the entry is thereafter
        // indistinguishable from a legitimate one.
        let requested_provider = inv.inline_str("provider").ok().map(str::to_string);
        if let Some(provider) = &requested_provider {
            let selectable = config.selectable();
            if !selectable.contains(provider) {
                return Err(Error::Denied(format!(
                    "browse: `{provider}` is not a provider this host offers to explain \
                     with; selectable here: {}",
                    selectable.into_iter().collect::<Vec<_>>().join(", ")
                )));
            }
            if let Ok(version) = inv.inline_str("version") {
                return Err(Error::Endpoint(format!(
                    "browse: `version={version}` and `provider={provider}` both name the \
                     model for this explain, and they can disagree — a version tag addresses \
                     an archived entry outright and derives nothing. Pass one, not both."
                )));
            }
        }

        // The archive key's backbone, THROUGH the kernel (dependency-recorded).
        let hash_repr = inv.source(&parse_iri(&hash_iri(repo, &rel))?).await?;
        let hash = String::from_utf8_lossy(&hash_repr.bytes).trim().to_string();

        // Classification must be deterministic BEFORE the lookup (the tag is
        // part of the key), and binary-ness needs the bytes — which the
        // resolution reads anyway (hashing already walked them); tokens, not
        // file reads, are the currency here.
        let (grain, target_iri, material) = if target.is_dir() {
            (Grain::Directory, tree_iri(repo, &rel), None)
        } else {
            let content = inv.source(&parse_iri(&file_iri(repo, &rel))?).await?;
            let text = String::from_utf8(content.bytes.clone()).ok();
            let grain = classify_file(&rel, text.is_some());
            (grain, file_iri(repo, &rel), Some((content, text)))
        };

        // The backend that will answer, and the model identity describing IT
        // — resolved ONCE per request; the same value serves the archive
        // lookup (hit path) and stamps a new entry (miss path). Binary stubs
        // involve no model, but a bad `provider=` still failed above.
        let tier = match grain {
            Grain::Directory => Tier::Dir,
            _ => Tier::File,
        };
        let provider = self.provider_for(tier, requested_provider.as_deref());
        let model = match grain {
            Grain::Binary => None,
            _ => Some(self.model_label(inv, tier, &provider).await),
        };
        let current_tag = version_tag(grain, model.as_deref().unwrap_or(""));
        let requested = inv.inline_str("version").ok().map(str::to_string);
        let tag = requested.clone().unwrap_or_else(|| current_tag.clone());

        let iri = entry_iri(repo, &rel, &hash, &tag);
        if let Some(entry) = load_entry(&config.store, &iri)? {
            let included = self
                .included(inv, repo, &rel, grain == Grain::Directory)
                .await?;
            return face(
                inv,
                repo,
                &rel,
                Served {
                    entry,
                    derived: false,
                },
                included.as_ref(),
            );
        }
        if let Some(tag) = requested {
            return Err(Error::NotFound(format!(
                "browse: no archived explanation of `{rel}` at version `{tag}` for the current \
                 content ({} lists what the archive holds)",
                versions_iri(repo, &rel)
            )));
        }

        // Miss under the current tag: derive, archive, serve.
        let text = match grain {
            Grain::Directory => self.derive_dir(inv, repo, &rel, &provider).await?,
            Grain::Binary => {
                let (content, _) = material.as_ref().expect("file grains carry material");
                format!(
                    "Binary file ({}, {}).",
                    media_type_for(Path::new(&rel), &content.bytes).media_type,
                    human_size(content.bytes.len() as u64)
                )
            }
            _ => {
                let (_, text) = material.as_ref().expect("file grains carry material");
                let text = text.as_deref().expect("non-binary grains are UTF-8");
                self.derive_file(inv, repo, &rel, grain, text, &provider)
                    .await?
            }
        };
        let model = model.unwrap_or_else(|| "none".to_string());
        let entry = ArchiveEntry {
            iri,
            repo: repo.to_string(),
            rel: rel.clone(),
            target_iri,
            hash,
            tag: current_tag,
            model,
            kind: grain.label().to_string(),
            text,
            derived_at: inv.now().map(|t| iso8601(t.as_millis())),
        };
        store_entry(&config.store, &entry)?;
        let included = self
            .included(inv, repo, &rel, grain == Grain::Directory)
            .await?;
        face(
            inv,
            repo,
            &rel,
            Served {
                entry,
                derived: true,
            },
            included.as_ref(),
        )
    }

    fn name(&self) -> &str {
        "browse-explain"
    }

    fn describe(&self) -> Description {
        explain_description(&self.config)
    }
}

/// Which model tier a grain asks — file grain vs directory rollup, each with
/// its own provider and label override.
#[derive(Clone, Copy)]
enum Tier {
    File,
    Dir,
}

impl ExplainEndpoint {
    /// The `annotations=include` payload for this explain, when asked for — a
    /// file target folds its own annotations, a directory rollup its
    /// subtree's (the whole repo at the root). Same drift-reconciliation as
    /// the listing endpoint.
    async fn included(
        &self,
        inv: &Invocation<'_>,
        repo: &str,
        rel: &str,
        dir: bool,
    ) -> Result<Option<Included>> {
        if !include_annotations(inv)? {
            return Ok(None);
        }
        let filter = if dir {
            TargetFilter::Subtree(rel)
        } else {
            TargetFilter::File(rel)
        };
        annotate::included_for(inv, &self.config.store, &self.roots, repo, filter)
            .await
            .map(Some)
    }

    /// The backend this request derives against for `tier`: the caller's
    /// already-validated `provider=` when given, else the tier's configured
    /// provider. Absent the argument this is the identity function on today's
    /// behaviour.
    fn provider_for(&self, tier: Tier, requested: Option<&str>) -> String {
        match (requested, tier) {
            (Some(provider), _) => provider.to_string(),
            (None, Tier::File) => self.config.file_provider.clone(),
            (None, Tier::Dir) => self.config.dir_provider.clone(),
        }
    }

    /// The model identity folded into this request's version tag, in
    /// precedence order: the explicit config label (the operator's override)
    /// → the provider's `urn:llm:…:model` resolved through the kernel
    /// ([`resolve_model`] — the true configured id) → the provider-IRI
    /// heuristic ([`provider_label`]). Infallible by design: a host without
    /// the llm module (or a pre-0.10 one) still explains, with heuristic tags.
    ///
    /// ★ THE OPERATOR'S LABEL DESCRIBES THE OPERATOR'S BACKEND — it is keyed
    /// to the provider, not to the tier, so it applies only while `provider`
    /// IS the tier's configured one. A request that selects a different
    /// backend resolves THAT backend's own identity instead of inheriting a
    /// label written for another model. The alternative would stamp (say)
    /// `file_model_label = "coder-30b"` onto a 70B model's output, writing a
    /// wrong model identity into the archive KEY — and a wrong key is
    /// indistinguishable from a legitimate cache entry forever after.
    async fn model_label(&self, inv: &Invocation<'_>, tier: Tier, provider: &str) -> String {
        let config = &self.config;
        let (configured, explicit) = match tier {
            Tier::File => (&config.file_provider, &config.file_model_label),
            Tier::Dir => (&config.dir_provider, &config.dir_model_label),
        };
        if provider == configured {
            if let Some(label) = explicit {
                return label.clone();
            }
        }
        match resolve_model(inv, provider).await {
            Some(model) => model,
            None => provider_label(provider),
        }
    }

    /// Derive a file-grain explanation: the grain's versioned prompt plus the
    /// (truncated) content, asked of `provider` (the request's choice or the
    /// configured file tier) under the mandatory token ceiling.
    #[allow(clippy::too_many_arguments)]
    async fn derive_file(
        &self,
        inv: &Invocation<'_>,
        repo: &str,
        rel: &str,
        grain: Grain,
        content: &str,
        provider: &str,
    ) -> Result<String> {
        let config = &self.config;
        let prompt = format!(
            "{}\n\nRepository: {repo}\nPath: {rel}\n\n```\n{}\n```",
            grain_prompt(grain),
            truncate(content, config.max_prompt_bytes),
        );
        self.ask(inv, provider, &prompt, config.file_max_tokens)
            .await
    }

    /// Derive a directory rollup: the entry list (ignore-filtered) plus each
    /// child's OWN explanation, sourced through the kernel — the recursion
    /// that bottoms out at files, and the reason one edit recomputes exactly
    /// one path up the tree (unchanged children are archive hits).
    ///
    /// `provider` selects the backend for THIS rollup only. The children are
    /// plain sub-resolutions of their own `explain` IRIs and carry no
    /// argument, so they derive (or hit) under their own tier's tag — the
    /// same mixing the two-tier default already does, and the reason a
    /// rollup's tag has never described the material fed to it.
    async fn derive_dir(
        &self,
        inv: &Invocation<'_>,
        repo: &str,
        rel: &str,
        provider: &str,
    ) -> Result<String> {
        let config = &self.config;
        let tree = inv.source(&parse_iri(&tree_iri(repo, rel))?).await?;
        let listing = String::from_utf8_lossy(&tree.bytes).to_string();
        let mut kept: Vec<(String, String)> = Vec::new(); // (name, kind)
        for line in listing.lines() {
            let mut cols = line.split('\t');
            let (Some(name), Some(kind)) = (cols.next(), cols.next()) else {
                continue;
            };
            if config.ignore.contains(name) || kind == "link" {
                continue;
            }
            kept.push((name.to_string(), kind.to_string()));
        }
        let mut sections = String::new();
        for (name, kind) in &kept {
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let child = inv
                .source(&parse_iri(&explain_iri(repo, &child_rel))?)
                .await?;
            let text = String::from_utf8_lossy(&child.bytes).trim().to_string();
            sections.push_str(&format!("### {name} ({kind})\n{text}\n\n"));
        }
        let entries: Vec<String> = kept
            .iter()
            .map(|(name, kind)| format!("{name} ({kind})"))
            .collect();
        let where_ = if rel.is_empty() {
            "the repository root".to_string()
        } else {
            format!("`{rel}`")
        };
        let prompt = format!(
            "{}\n\nRepository: {repo}\nDirectory: {}\nEntries: {}\n\n{}",
            DIR_PROMPT,
            where_,
            entries.join(", "),
            truncate(&sections, config.max_prompt_bytes),
        );
        self.ask(inv, provider, &prompt, config.dir_max_tokens)
            .await
    }

    /// One ask, through the kernel: system + prompt + `temperature` + the
    /// MANDATORY `max_tokens` ceiling. An empty answer is an error and is NOT
    /// archived — a truncated-to-nothing response must not poison the archive
    /// under a key that will never re-derive.
    async fn ask(
        &self,
        inv: &Invocation<'_>,
        provider: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let request = Request::new(Verb::Source, parse_iri(provider)?)
            .with_arg("prompt", ArgRef::Inline(prompt.as_bytes().to_vec()))
            .with_arg("system", ArgRef::Inline(SYSTEM_PROMPT.as_bytes().to_vec()))
            .with_arg(
                "temperature",
                ArgRef::Inline(self.config.temperature.clone().into_bytes()),
            )
            .with_arg(
                "max_tokens",
                ArgRef::Inline(max_tokens.to_string().into_bytes()),
            );
        let answer = inv.issue(request).await?;
        let text = String::from_utf8_lossy(&answer.bytes).trim().to_string();
        if text.is_empty() {
            return Err(Error::Endpoint(format!(
                "browse: `{provider}` returned an empty explanation (max_tokens {max_tokens} — \
                 thinking models may need a higher ceiling); nothing archived"
            )));
        }
        Ok(text)
    }
}

pub(crate) fn parse_iri(iri: &str) -> Result<Iri> {
    Iri::parse(iri).map_err(|e| Error::Endpoint(format!("browse: bad IRI `{iri}`: {e}")))
}

// --- faces ------------------------------------------------------------------

fn face(
    inv: &Invocation<'_>,
    repo: &str,
    rel: &str,
    served: Served,
    included: Option<&Included>,
) -> Result<Representation> {
    let entry = &served.entry;
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => {
            let mut json = serde_json::json!({
                "text": entry.text,
                "content_hash": entry.hash,
                "version_tag": entry.tag,
                "derived": served.derived,
                "model": entry.model,
                "prompt_kind": entry.kind,
                "about": entry.target_iri,
            });
            if let Some(included) = included {
                json["annotations"] = included.json();
            }
            Ok(repr("application/json", json.to_string()))
        }
        t if t.starts_with("text/html") => Ok(repr_utf8(
            "text/html",
            explain_html(repo, rel, entry, served.derived, included),
        )),
        t if t.starts_with("text/turtle") => Ok(repr("text/turtle", explain_turtle(entry))),
        _ => {
            let mut text = entry.text.clone();
            if let Some(included) = included {
                text.push_str("\n\n");
                text.push_str(&included.margin_text());
            }
            Ok(repr_utf8("text/plain", text))
        }
    }
}

/// The S0 page style: crumbs, a backlink to the explained resource, the
/// explanation as paragraphs, and the provenance line — "explained by
/// {model} · {tag}" — that makes derivation citable at a glance. With
/// `annotations=include`, the target's annotation cards follow, the same
/// markup the file face's panel renders.
fn explain_html(
    repo: &str,
    rel: &str,
    entry: &ArchiveEntry,
    derived: bool,
    included: Option<&Included>,
) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    // The backlink: an explanation is ABOUT a resource, and the face says so
    // navigably — a directory's target is its tree face, a file's its view.
    let is_dir = entry.target_iri == tree_iri(repo, rel);
    out.push_str(&format!(
        "<nav class=\"browse-actions\"><button class=\"browse-view-link\" \
         hx-get=\"/k/source {} as=text/html\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">{}</button></nav>",
        entry.target_iri,
        if is_dir {
            "view directory"
        } else {
            "view file"
        },
    ));
    // The menu belongs here most of all: the provenance line below names the
    // model that answered, and this is where a reader decides they want
    // another one — or the one they already paid for.
    out.push_str(&menu_html(repo, rel));
    out.push_str("<div class=\"browse-explain\">");
    for paragraph in entry.text.split("\n\n").filter(|p| !p.trim().is_empty()) {
        out.push_str(&format!("<p>{}</p>", esc(paragraph.trim())));
    }
    out.push_str("</div>");
    let hash_short: String = entry.hash.chars().take(19).collect(); // "sha256:" + 12 hex
    out.push_str(&format!(
        "<p class=\"browse-provenance\">explained by {} · {} · {}… · {}</p>",
        esc(&entry.model),
        esc(&entry.tag),
        esc(&hash_short),
        if derived {
            "derived now"
        } else {
            "from the archive"
        },
    ));
    if let Some(included) = included {
        out.push_str(&included.panel_html((!is_dir).then_some(entry.target_iri.as_str())));
    }
    out.push_str("</div>");
    out
}

/// The archive entry as Turtle — the same skolemized shape the store holds.
pub(crate) fn explain_turtle(entry: &ArchiveEntry) -> String {
    let mut props = vec![
        "a ik:Explanation".to_string(),
        format!("ik:repo {}", ttl_str(&entry.repo)),
        format!("ik:path {}", ttl_str(&entry.rel)),
        format!("ik:about <{}>", entry.target_iri),
        format!("ik:contentHash {}", ttl_str(&entry.hash)),
        format!("ik:versionTag {}", ttl_str(&entry.tag)),
        format!("ik:model {}", ttl_str(&entry.model)),
        format!("ik:promptKind {}", ttl_str(&entry.kind)),
        format!("ik:explanation {}", ttl_str(&entry.text)),
    ];
    if let Some(at) = &entry.derived_at {
        props.push(format!(
            "ik:derivedAt \"{at}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"
        ));
    }
    format!(
        "@prefix ik: <{IK}> .\n\n<{}> {} .\n",
        entry.iri,
        props.join(" ;\n    ")
    )
}

/// `repo` is not an ArgSpec: every advertised row fixes the root in its
/// pattern (see `crate::bind_family`); the binding is grammar-injected.
///
/// Takes the config because the `provider` argument's `one_of` IS this host's
/// allowlist ([`ExplainConfig::selectable`]) — the manifold must state which
/// backends a caller may actually name, not a hard-coded guess, so that
/// `urn:kernel:validate` can reject a bad one before dispatch and a UI can
/// build its menu from the description alone.
fn explain_description(config: &ExplainConfig) -> Description {
    Description::new("browse-explain")
        .title("Repository explanation (archived derivation)")
        .summary(
            "An LLM-derived orientation explanation of a file or directory — \
             urn:repo:{repo}:explain[:{path}] — ARCHIVED by (path, content-hash, \
             version-tag) so it derives once per content version and is reused forever. \
             Directories synthesize their children's explanations (one edit re-derives \
             exactly the path to the root); the version tag folds prompt version and model \
             identity; version= addresses an older tag; provider= derives this one against \
             a different (host-allowed) backend, keyed by that backend's own model identity. \
             text/plain (default) is the text; \
             as=application/json adds {content_hash, version_tag, derived}; as=text/html \
             renders the page face with provenance; as=text/turtle emits the archive \
             entry's graph. annotations=include folds the target's annotations in \
             (drift-reconciled like the listing): the json face gains an annotations \
             array, the text face appends a margin-notes section, the html face renders \
             the annotation cards; a directory rollup folds its subtree's.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .summary("path within the root, percent-encoded (omitted = the whole-repo rollup)"),
        )
        .input(ArgSpec::new("version").optional().summary(
            "an archived version tag (e.g. code-v1@qwen3-coder:30b) instead of the current one",
        ))
        .input(
            ArgSpec::new("provider")
                // The value is an endpoint IRI, not free text — the class says
                // so, so type-based selection can offer it a resource rather
                // than a string.
                .class("http://www.w3.org/2001/XMLSchema#anyURI")
                .optional()
                .summary(
                    "the LLM provider IRI that derives THIS explanation, instead of the \
                     configured tier default (file grain vs directory rollup); one_of is what \
                     this host allows. That backend's own model identity keys the archive \
                     entry, so two models yield two coexisting entries. A directory rollup \
                     applies it to the rollup only — the children keep their own tags. \
                     Mutually exclusive with version=, which addresses an entry instead of \
                     deriving one.",
                )
                .one_of(config.selectable()),
        )
        .input(
            ArgSpec::new("annotations")
                .optional()
                .summary(
                    "include folds the target's annotations into the json, text, and \
                     html faces (a directory rollup folds its subtree's)",
                )
                .one_of(["include", "true", "false"])
                .default_value("false"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- the option menu --------------------------------------------------------

/// One row of the "explain with" menu: a MODEL, and every selectable provider
/// that serves it.
///
/// ★ THE MENU'S AXIS IS THE MODEL, because the ARCHIVE'S axis is. The version
/// tag folds model identity, not backend identity (see this module's header),
/// so two providers serving one model share an archive key: choosing "the
/// other one" returns the first one's text instantly, from the archive. A menu
/// of BACKENDS would therefore promise a second explanation it cannot deliver,
/// and its no-op would read as a bug. One model, one row — the serving
/// backends are named beside it as a fact, never offered as a second choice.
struct ModelOption {
    /// The model identity `urn:llm:models` attributes to these providers, or
    /// `None` when nothing reports one: no llm module bound, a selectable
    /// provider the inventory does not list, or — since ikigai-llm 0.12, where
    /// a provider may pin no model and discover it — a discovering provider
    /// whose backend the inventory could not reach.
    ///
    /// ★ THE MENU MAY KNOW LESS THAN THE TAG WILL, and that asymmetry is
    /// deliberate. The version tag folds `urn:llm:{p}:model`, which for a
    /// discovering provider PROBES; the menu folds `urn:llm:models`, whose own
    /// discovery pass is best-effort. So a discovering backend can be an
    /// "unknown model" row here and still archive under a real discovered id.
    /// A menu is allowed to know less than the derivation it starts; what it
    /// may never do is claim more, which is why an unknown is shown as unknown
    /// rather than guessed from the provider IRI without saying so.
    ///
    /// ★ UNKNOWN IS NOT A VALUE THAT GROUPS. Two providers of unknown model
    /// are two rows: they may well be two models, and collapsing them would
    /// hide a real choice behind the menu's own ignorance. Every `None` row
    /// therefore holds exactly one provider.
    model: Option<String>,
    /// The selectable provider IRIs serving it, sorted; the first is the one
    /// the row's button names. Longer than one only when `model` is `Some`.
    providers: Vec<String>,
    /// Which configured tier already points at this model ("files",
    /// "directories", "files and directories") — the row a plain `explain`
    /// click would have taken, marked so the menu does not read as a set of
    /// alternatives to something unnamed.
    default_for: Option<&'static str>,
}

impl ModelOption {
    /// The row's button text: the model id when known, else the provider
    /// heuristic ([`provider_label`]) — which is exactly the label the version
    /// tag will carry if this row is clicked, so the menu and the archive
    /// agree even when neither knows the model.
    fn label(&self) -> String {
        match &self.model {
            Some(model) => model.clone(),
            None => provider_label(&self.providers[0]),
        }
    }

    /// The provider IRI the row's `provider=` names.
    ///
    /// Any provider in the row keys the SAME archive entry, so on a hit the
    /// choice is invisible. On a MISS it is not: some backend actually runs
    /// the model. `providers` is ordered so a configured tier comes first
    /// (see [`menu_options`]) — a row that is the operator's default derives
    /// on the operator's backend, and only a row with no configured backend
    /// falls through to lexicographic order.
    fn provider(&self) -> &str {
        &self.providers[0]
    }
}

/// Every selectable provider, grouped into menu rows by the model it serves.
///
/// ★ ONE SUB-REQUEST FOR THE WHOLE MENU. The identity resource the version
/// tags fold is `urn:llm:{p}:model`, which PROBES the backend, declares
/// `urn:cap:net:*` and is uncacheable — one network call per provider, which
/// would make opening a menu cost as much as the derivation it is trying to
/// let you avoid. `urn:llm:models` reports the same configured identity for
/// every provider in one document, cacheable while no provider is discovering,
/// and requires no capability of its own (its discovery pass consults the
/// invocation's capability and simply skips what it may not reach). So a menu
/// costs at most one sub-request however many backends the host has, and a
/// browse-only session still gets the declared inventory.
///
/// The set of rows is drawn from [`ExplainConfig::selectable`] — the exact set
/// `provider=` accepts — so the menu can never offer a click that would come
/// back `Denied`.
async fn menu_options(inv: &Invocation<'_>, config: &ExplainConfig) -> Vec<ModelOption> {
    let inventory = model_inventory(inv).await;
    let mut by_model: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut unknown: Vec<String> = Vec::new();
    // `selectable()` is a BTreeSet, so both loops below are already sorted.
    for provider in config.selectable() {
        match inventory
            .as_ref()
            .and_then(|inventory| inventory_model(inventory, &provider))
        {
            Some(model) => by_model.entry(model).or_default().push(provider),
            None => unknown.push(provider),
        }
    }
    let mut options: Vec<ModelOption> = by_model
        .into_iter()
        .map(|(model, mut providers)| {
            // A configured tier first, so the row's single button derives on
            // the backend the operator chose. Which one answers cannot change
            // the archive KEY — that is the whole point of grouping by model —
            // but it does decide which machine spends the time on a miss, and
            // silently routing the default row's first derivation through some
            // alphabetically-earlier backend would be a surprise nobody asked
            // for.
            providers.sort_by_key(|p| {
                (
                    *p != config.file_provider,
                    *p != config.dir_provider,
                    p.clone(),
                )
            });
            ModelOption {
                default_for: default_for(config, &providers),
                model: Some(model),
                providers,
            }
        })
        .collect();
    options.extend(unknown.into_iter().map(|provider| ModelOption {
        default_for: default_for(config, std::slice::from_ref(&provider)),
        model: None,
        providers: vec![provider],
    }));
    // The tier defaults first — what a plain `explain` click already does —
    // then alphabetically by label. Total and deterministic either way.
    options.sort_by_key(|o| (o.default_for.is_none(), o.label()));
    options
}

/// Which configured tier (if either) is served by one of these providers.
fn default_for(config: &ExplainConfig, providers: &[String]) -> Option<&'static str> {
    let file = providers.contains(&config.file_provider);
    let dir = providers.contains(&config.dir_provider);
    match (file, dir) {
        (true, true) => Some("files and directories"),
        (true, false) => Some("files"),
        (false, true) => Some("directories"),
        (false, false) => None,
    }
}

/// `urn:llm:models` as JSON, or `None` — no llm module, an older one, a
/// malformed body. Never an error: a host with no LLM bound at all still
/// renders the menu (from provider IRIs alone), because the plain explain
/// button it sits beside has always been offered on such a host too.
async fn model_inventory(inv: &Invocation<'_>) -> Option<serde_json::Value> {
    serde_json::from_str(&source_str(inv, "urn:llm:models").await?).ok()
}

/// The model an inventory attributes to a provider IRI — the same two IRI
/// shapes [`resolve_model`] resolves, read out of one document instead of one
/// probe each: `urn:llm:{p}:ask` names its own entry, and the bare facade
/// `urn:llm:ask` routes to the inventory's `default` provider.
fn inventory_model(inventory: &serde_json::Value, provider: &str) -> Option<String> {
    let name = if provider == "urn:llm:ask" {
        inventory.get("default")?.as_str()?.to_string()
    } else {
        provider
            .strip_prefix("urn:llm:")?
            .strip_suffix(":ask")?
            .to_string()
    };
    let model = inventory.get("models")?.get(name)?.get("model")?.as_str()?;
    let model = model.trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// The closed disclosure that sits under a face's action row: markup only, no
/// resolution. The panel's content is a SEPARATE resolution of this path's
/// `explain-versions … as=text/html`, fetched on the `toggle` event, so a
/// directory listing of a thousand entries costs nothing until a human opens
/// a menu — and opening one costs one archive read and one inventory resolve,
/// not one per entry.
///
/// `<details>`/`<summary>` and not a scripted dropdown: it is focusable and
/// operable from the keyboard with no CSS and no JavaScript of ours, it is
/// announced as a disclosure, it is never hover-only, and being block-level it
/// lays its panel out at any width — a phone included.
pub(crate) fn menu_html(repo: &str, rel: &str) -> String {
    format!(
        "<details class=\"browse-explain-menu\"><summary>explain with…</summary>\
         <div class=\"browse-explain-menu-body\" hx-get=\"/k/source {iri} as=text/html\" \
         hx-trigger=\"toggle once from:closest details\" hx-target=\"this\" \
         hx-swap=\"innerHTML\"><p>loading options…</p></div></details>",
        iri = versions_iri(repo, rel),
    )
}

/// A value safe to pass through the host's `/k/` adapter, which splits its
/// command on whitespace: anything containing whitespace cannot be named as an
/// argument there, so its row renders inert rather than emitting a request the
/// host would mis-parse. Model ids and provider IRIs never contain whitespace
/// in practice; a discovered one might.
fn command_safe(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

/// The menu panel: what the archive already holds for this path (free to
/// reopen — a pure store read) above what could still be derived for it (one
/// model call each, then archived forever). Both lists are keyed by MODEL,
/// which is what the archive is keyed by.
fn menu_panel_html(
    repo: &str,
    rel: &str,
    options: &[ModelOption],
    entries: &[ArchiveEntry],
    current_hash: Option<&str>,
) -> String {
    let explain = explain_iri(repo, rel);
    let mut out = String::from("<div class=\"browse-explain-menu-panel\">");

    // --- already explained, at the CURRENT content ---------------------------
    //
    // ONLY the current content's entries are offered. `version=` addresses
    // `(path, current hash, tag)`, so a row archived under earlier content is
    // not reachable through it — offering one would be a click that 404s.
    let reopenable: Vec<&ArchiveEntry> = entries
        .iter()
        .filter(|e| current_hash.is_some_and(|hash| hash == e.hash))
        .collect();
    let earlier = entries.len() - reopenable.len();
    out.push_str(
        "<p class=\"browse-explain-menu-heading\">already explained \
         <span class=\"browse-size\">from the archive — no model call</span></p>",
    );
    if reopenable.is_empty() {
        let note = match (entries.is_empty(), current_hash) {
            (true, _) => "nothing yet — no explanation of this path has been derived.".to_string(),
            (false, None) => format!(
                "{earlier} archived, none matchable — this path has no current content \
                 (deleted?), so none of them can be reopened here."
            ),
            (false, Some(_)) => format!(
                "nothing for the current content — {earlier} archived under earlier \
                 versions of it, which a new derivation does not replace."
            ),
        };
        out.push_str(&format!(
            "<p class=\"browse-explain-menu-empty\">{}</p>",
            esc(&note)
        ));
    } else {
        out.push_str("<ul class=\"browse-entries browse-explain-archived\">");
        for entry in &reopenable {
            let detail = format!(
                "{} · {}",
                entry.tag,
                entry.derived_at.as_deref().unwrap_or("no timestamp")
            );
            let label = if entry.model.is_empty() {
                entry.tag.clone()
            } else {
                entry.model.clone()
            };
            if command_safe(&entry.tag) {
                out.push_str(&format!(
                    "<li><button class=\"browse-explain-link\" hx-get=\"/k/source {explain} \
                     as=text/html version={tag}\" hx-target=\"#browse\" hx-swap=\"innerHTML\" \
                     title=\"reopen this archived explanation — no model call\">{label}</button> \
                     <span class=\"browse-size\">{detail}</span></li>",
                    tag = esc(&entry.tag),
                    label = esc(&label),
                    detail = esc(&detail),
                ));
            } else {
                out.push_str(&format!(
                    "<li><span class=\"browse-explain-inert\">{}</span> \
                     <span class=\"browse-size\">{} · not addressable from here</span></li>",
                    esc(&label),
                    esc(&detail),
                ));
            }
        }
        out.push_str("</ul>");
        if earlier > 0 {
            out.push_str(&format!(
                "<p class=\"browse-explain-menu-empty\">{earlier} more archived under \
                 earlier content, listed by explain-versions.</p>"
            ));
        }
    }

    // --- what could still be derived ----------------------------------------
    out.push_str(
        "<p class=\"browse-explain-menu-heading\">explain with \
         <span class=\"browse-size\">derives — one model call, then archived</span></p>",
    );
    out.push_str("<ul class=\"browse-entries browse-explain-choices\">");
    for option in options {
        let mut notes: Vec<String> = Vec::new();
        if let Some(tier) = option.default_for {
            notes.push(format!("default for {tier}"));
        }
        if option.providers.len() > 1 {
            // The backend named as a FACT, not offered as a second button:
            // either of these produces the one entry this row already names.
            notes.push(format!(
                "served by {}",
                option
                    .providers
                    .iter()
                    .map(|p| provider_label(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if option.model.is_none() {
            notes.push("backend reports no model id".to_string());
        }
        let detail = if notes.is_empty() {
            String::new()
        } else {
            format!(
                " <span class=\"browse-size\">{}</span>",
                esc(&notes.join(" · "))
            )
        };
        let label = option.label();
        if command_safe(option.provider()) {
            out.push_str(&format!(
                "<li><button class=\"browse-explain-link\" hx-get=\"/k/source {explain} \
                 as=text/html provider={provider}\" hx-target=\"#browse\" \
                 hx-swap=\"innerHTML\">{label}</button>{detail}</li>",
                provider = esc(option.provider()),
                label = esc(&label),
            ));
        } else {
            out.push_str(&format!(
                "<li><span class=\"browse-explain-inert\">{}</span>{detail}</li>",
                esc(&label),
            ));
        }
    }
    out.push_str("</ul>");
    // Secondary by construction: `browse-size` is the muted, smaller class the
    // faces already use for detail, so the note reads as a footnote on any
    // host that styles the family at all — without this crate inventing CSS.
    out.push_str(
        "<p class=\"browse-explain-menu-note\"><span class=\"browse-size\">One row per model, \
         not per backend: an explanation is archived by the model and the prompt that made \
         it, so two backends serving one model give one explanation — the second request is \
         an archive hit, not a second opinion.</span></p>",
    );
    out.push_str("</div>");
    out
}

// --- the versions listing ---------------------------------------------------

/// `urn:repo:{repo}:explain-versions[:{path}]` — what the archive holds for a
/// path, across content versions and tags, and (in the HTML face) what could
/// still be derived for it. The text and JSON faces are a pure store read:
/// they require only the browse grant (no net — they never derive), and work
/// for paths that no longer exist on disk (the archive outlives deletions).
///
/// The HTML face is the **option menu** — see [`menu_panel_html`]. It reads
/// two more resources, both best-effort and neither a new capability: the
/// path's current content hash (to know which archived entries `version=` can
/// actually reopen) and `urn:llm:models` (the inventory the menu groups by).
/// A failure of either degrades the panel, never the resource.
struct VersionsEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for VersionsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-explain-versions does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, _root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        // A path is a file target or a tree target; the archive knows which —
        // list both (no filesystem touch, so deleted paths still answer).
        let mut entries = list_versions(&self.config.store, &file_iri(repo, &rel))?;
        entries.extend(list_versions(&self.config.store, &tree_iri(repo, &rel))?);
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => {
                let rows: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "version_tag": e.tag,
                            "content_hash": e.hash,
                            "model": e.model,
                            "prompt_kind": e.kind,
                            "derived_at": e.derived_at,
                            "entry": e.iri,
                        })
                    })
                    .collect();
                Ok(repr(
                    "application/json",
                    serde_json::Value::Array(rows).to_string(),
                ))
            }
            t if t.starts_with("text/html") => {
                // BEST-EFFORT, both of them. The hash tells which archived
                // entries the current content can reopen — a path that no
                // longer exists has none, and that is a rendering difference,
                // not an error (this resource answers for deleted paths by
                // design). The inventory names the models; without it the menu
                // still offers every selectable backend, labelled by the same
                // heuristic the version tags fall back to.
                let current = source_str(inv, &hash_iri(repo, &rel)).await;
                let options = menu_options(inv, &self.config).await;
                Ok(repr_utf8(
                    "text/html",
                    menu_panel_html(repo, &rel, &options, &entries, current.as_deref()),
                ))
            }
            _ => {
                let lines: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        format!(
                            "{}\t{}\t{}\t{}",
                            e.tag,
                            e.hash,
                            e.model,
                            e.derived_at.as_deref().unwrap_or("-")
                        )
                    })
                    .collect();
                Ok(repr_utf8("text/plain", lines.join("\n")))
            }
        }
    }

    fn name(&self) -> &str {
        "browse-explain-versions"
    }

    fn describe(&self) -> Description {
        versions_description()
    }
}

/// `repo` is not an ArgSpec — see [`explain_description`]'s note.
fn versions_description() -> Description {
    Description::new("browse-explain-versions")
        .title("Archived explanation versions")
        .summary(
            "What the explanation archive holds for a path — \
             urn:repo:{repo}:explain-versions[:{path}]: one row per archived entry \
             (version tag, content hash, model, derived-at), newest first, across content \
             versions and prompt/model tags. Derives nothing and never requires the network: \
             it answers even for paths that no longer exist on disk. text/plain \
             (default) is tag<TAB>hash<TAB>model<TAB>derivedAt lines; as=application/json \
             the structured rows; as=text/html is the OPTION MENU the browse faces open \
             beside their explain button — the entries reopenable for the current content \
             (free), above the models this host will derive a new one with (one model call \
             each), one row per MODEL because that is what the archive keys on. The html \
             face additionally reads the path's hash and urn:llm:models, both best-effort \
             and neither adding a capability: without them the menu degrades rather than \
             failing.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .summary("path within the root, percent-encoded (omitted = the root rollup)"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("application/json for the structured rows, text/html for the option menu")
                .one_of(["text/plain", "application/json", "text/html"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Kernel};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-explain-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One recorded ask, exactly as the fake provider received it.
    #[derive(Clone)]
    struct Ask {
        provider: String,
        prompt: String,
        system: String,
        temperature: String,
        max_tokens: String,
    }

    #[derive(Default)]
    struct Log {
        asks: Mutex<Vec<Ask>>,
    }

    impl Log {
        fn count(&self, provider: &str) -> usize {
            self.asks
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.provider == provider)
                .count()
        }
        fn last(&self) -> Ask {
            self.asks.lock().unwrap().last().unwrap().clone()
        }
    }

    const FILE_PROVIDER: &str = "urn:llm:coder:ask";
    const DIR_PROVIDER: &str = "urn:llm:ask";
    /// A third bound backend that NO default config asks — the one the
    /// allowlist tests gate on.
    const BATCH_PROVIDER: &str = "urn:llm:batch:ask";

    /// Deterministic fake LLM backends: each answer is `EXPL#{n}` (n = global
    /// ask count), each ask recorded — the counters are how the tests observe
    /// "archive hit" (no new ask) vs "derive" (one more ask). Both declare
    /// the net wildcard, like the real module.
    fn fake_llm_space(log: &Arc<Log>) -> EndpointSpace {
        let mut space = EndpointSpace::new();
        for provider in [FILE_PROVIDER, DIR_PROVIDER, BATCH_PROVIDER] {
            let log = Arc::clone(log);
            space = space.bind(
                Exact::new(provider),
                FnEndpoint::new("fake-llm", move |inv: &Invocation<'_>| {
                    let n = {
                        let mut asks = log.asks.lock().unwrap();
                        asks.push(Ask {
                            provider: provider.to_string(),
                            prompt: inv.inline_str("prompt").unwrap_or("").to_string(),
                            system: inv.inline_str("system").unwrap_or("").to_string(),
                            temperature: inv.inline_str("temperature").unwrap_or("").to_string(),
                            max_tokens: inv.inline_str("max_tokens").unwrap_or("").to_string(),
                        });
                        asks.len()
                    };
                    Ok(repr_utf8("text/plain", format!("EXPL#{n}")))
                })
                .with_description(
                    Description::new("fake-llm")
                        .verb(Verb::Source)
                        .requires(CAP_NET),
                ),
            );
        }
        space
    }

    fn kernel_with(
        root: &Path,
        store: &Arc<Store>,
        log: &Arc<Log>,
        config: impl FnOnce(ExplainConfig) -> ExplainConfig,
    ) -> Kernel {
        let cfg = config(
            ExplainConfig::new(Arc::clone(store))
                .file_model_label("m1")
                .dir_model_label("d1"),
        );
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_llm_space(log)),
        ])))
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"])
    }

    fn source(
        kernel: &Kernel,
        iri: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        block_on(kernel.issue(request, cap))
    }

    fn body(repr: &Representation) -> String {
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    fn json(kernel: &Kernel, iri: &str, extra: &[(&str, &str)]) -> serde_json::Value {
        let mut args = vec![("as", "application/json")];
        args.extend_from_slice(extra);
        serde_json::from_str(&body(&source(kernel, iri, &args, &cap()).unwrap())).unwrap()
    }

    #[test]
    fn a_file_explanation_derives_once_then_serves_the_archive() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        let first = body(&source(&k, "urn:repo:demo:explain:src/lib.rs", &[], &cap()).unwrap());
        assert_eq!(first, "EXPL#1");
        assert_eq!(log.count(FILE_PROVIDER), 1);

        // Same content, same tag: an archive hit — no new ask, derived=false.
        let hit = json(&k, "urn:repo:demo:explain:src/lib.rs", &[]);
        assert_eq!(hit["text"], "EXPL#1");
        assert_eq!(hit["derived"], false);
        assert_eq!(hit["version_tag"], "code-v1@m1");
        assert!(hit["content_hash"].as_str().unwrap().starts_with("sha256:"));
        assert_eq!(log.count(FILE_PROVIDER), 1, "hit must not re-derive");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn one_edit_rederives_exactly_the_path_to_the_root() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("sub2")).unwrap();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        std::fs::write(root.join("sub/b.rs"), "// b\n").unwrap();
        std::fs::write(root.join("sub2/c.rs"), "// c\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // The whole-repo rollup: three files, three directories (sub, sub2, root).
        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.count(FILE_PROVIDER), 3);
        assert_eq!(log.count(DIR_PROVIDER), 3);

        // Resolving again is all archive hits.
        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.count(FILE_PROVIDER), 3);
        assert_eq!(log.count(DIR_PROVIDER), 3);

        // Edit ONE nested file: the merkle cascade re-keys b.rs, sub, and the
        // root — and nothing else. a.rs, c.rs, and sub2 stay archive hits.
        std::fs::write(root.join("sub/b.rs"), "// b, edited\n").unwrap();
        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.count(FILE_PROVIDER), 4, "only b.rs re-derives");
        assert_eq!(
            log.count(DIR_PROVIDER),
            5,
            "only sub and the root re-derive"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_version_bump_rederives_lazily_while_the_old_tag_stays_addressable() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());

        // Derived under the m1 tag…
        let k1 = kernel_with(&root, &store, &log, |c| c);
        let first = body(&source(&k1, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap());
        assert_eq!(first, "EXPL#1");

        // …then the model (= tag) changes: same store, lazily re-derived.
        let k2 = kernel_with(&root, &store, &log, |c| c.file_model_label("m2"));
        let second = json(&k2, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(second["text"], "EXPL#2");
        assert_eq!(second["version_tag"], "code-v1@m2");
        assert_eq!(log.count(FILE_PROVIDER), 2);

        // The old tag remains addressable — from the archive, no new ask.
        let old = json(
            &k2,
            "urn:repo:demo:explain:a.rs",
            &[("version", "code-v1@m1")],
        );
        assert_eq!(old["text"], "EXPL#1");
        assert_eq!(old["derived"], false);
        assert_eq!(log.count(FILE_PROVIDER), 2);

        // An unknown version is a typed miss naming the versions resource.
        let err = source(
            &k2,
            "urn:repo:demo:explain:a.rs",
            &[("version", "code-v9@x")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        assert!(format!("{err:?}").contains("explain-versions"), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prompts_are_type_aware_and_tags_carry_the_prompt_version() {
        let root = temp_dir();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("NOTES.md"), "# notes\n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "# agent rules\n").unwrap();
        std::fs::write(root.join("LICENSE"), "MIT terms\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        for (path, marker, tag) in [
            ("main.rs", "source file", "code-v1@m1"),
            ("NOTES.md", "note or document", "note-v1@m1"),
            ("CLAUDE.md", "agent or skill definition", "skill-v1@m1"),
            ("LICENSE", "text file", "text-v1@m1"),
        ] {
            let iri = format!("urn:repo:demo:explain:{path}");
            let row = json(&k, &iri, &[]);
            assert_eq!(row["version_tag"], tag, "{path}");
            let prompt = log.last().prompt;
            assert!(prompt.contains(marker), "{path}: {prompt}");
            assert!(prompt.contains(path), "{path}: {prompt}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn token_ceilings_and_temperature_pass_through_per_grain() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // File grain: the coder provider under the 400-token default ceiling.
        source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();
        let ask = log.last();
        assert_eq!(ask.provider, FILE_PROVIDER);
        assert_eq!(ask.max_tokens, "400");
        assert_eq!(ask.temperature, "0.2");
        assert_eq!(ask.system, SYSTEM_PROMPT);

        // Directory grain: the default provider under the 600-token ceiling.
        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        let ask = log.last();
        assert_eq!(ask.provider, DIR_PROVIDER);
        assert_eq!(ask.max_tokens, "600");

        // And every knob is config, not constant.
        let store2 = Arc::new(Store::new().unwrap());
        let k2 = kernel_with(&root, &store2, &log, |c| {
            c.file_max_tokens(123)
                .dir_max_tokens(456)
                .temperature("0.7")
        });
        source(&k2, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();
        assert_eq!(log.last().max_tokens, "123");
        assert_eq!(log.last().temperature, "0.7");
        source(&k2, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.last().max_tokens, "456");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn binary_files_get_a_canned_stub_without_an_ask() {
        let root = temp_dir();
        std::fs::write(root.join("img.png"), [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]).unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        let text = body(&source(&k, "urn:repo:demo:explain:img.png", &[], &cap()).unwrap());
        assert!(text.contains("Binary file (image/png"), "{text}");
        assert_eq!(log.count(FILE_PROVIDER), 0, "no tokens for binaries");

        // Archived like everything else — the second resolve is a hit.
        let row = json(&k, "urn:repo:demo:explain:img.png", &[]);
        assert_eq!(row["derived"], false);
        assert_eq!(row["version_tag"], "binary-v1");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignored_entries_stay_out_of_rollups_and_keys() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        std::fs::write(root.join("target/junk.rs"), "// junk\n").unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.count(FILE_PROVIDER), 1, "only a.rs is explained");
        assert_eq!(
            log.count(DIR_PROVIDER),
            1,
            "target/ and .git/ are not rolled up"
        );
        let rollup = log.last().prompt;
        assert!(!rollup.contains("target"), "{rollup}");
        assert!(!rollup.contains(".git"), "{rollup}");

        // Build churn does not re-key: the rollup stays an archive hit.
        std::fs::write(root.join("target/junk.rs"), "// churn\n").unwrap();
        source(&k, "urn:repo:demo:explain", &[], &cap()).unwrap();
        assert_eq!(log.count(DIR_PROVIDER), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn explain_is_denied_without_a_net_grant_but_versions_is_not() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // A browse-only capability: explain declares urn:cap:net:* and the
        // kernel's baseline (declared = enforced) denies before any work.
        let browse_only = Capability::scoped(["urn:cap:browse:read:demo"]);
        let err = source(&k, "urn:repo:demo:explain:a.rs", &[], &browse_only).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert_eq!(log.count(FILE_PROVIDER), 0);

        // The versions listing is a pure archive read — no net required.
        assert!(source(&k, "urn:repo:demo:explain-versions:a.rs", &[], &browse_only).is_ok());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_html_face_backlinks_its_target_and_folds_annotations() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);
        let cap = Capability::scoped([
            "urn:cap:browse:read:demo",
            "urn:cap:net:localhost",
            crate::CAP_ANNOTATE,
        ]);
        // An annotation on the file, created through the kernel.
        let mut req = Request::new(Verb::Sink, Iri::parse("urn:annotation:n1").unwrap());
        for (name, value) in [
            ("target", "urn:repo:demo:file:src/lib.rs"),
            ("exact", "fn main()"),
            ("body", "the entrypoint"),
        ] {
            req = req.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        block_on(k.issue(req, &cap)).unwrap();

        // The file explain backlinks its file…
        let html = body(
            &source(
                &k,
                "urn:repo:demo:explain:src/lib.rs",
                &[("as", "text/html")],
                &cap,
            )
            .unwrap(),
        );
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:file:src/lib.rs as=text/html\""),
            "{html}"
        );
        assert!(html.contains("view file"), "{html}");
        // …and folds no annotations unless asked.
        assert!(!html.contains("browse-annotations"), "{html}");

        // annotations=include folds the file's cards in, create form and all
        // — the same markup the file face's panel renders.
        let html = body(
            &source(
                &k,
                "urn:repo:demo:explain:src/lib.rs",
                &[("as", "text/html"), ("annotations", "include")],
                &cap,
            )
            .unwrap(),
        );
        assert!(html.contains("browse-annotation"), "{html}");
        assert!(html.contains("the entrypoint"), "{html}");
        assert!(
            html.contains("hx-post=\"/k/sink urn:annotation\""),
            "{html}"
        );

        // A directory rollup backlinks its tree; its subtree cards carry
        // their paths and no create form (a create needs one target).
        let html = body(
            &source(
                &k,
                "urn:repo:demo:explain:src",
                &[("as", "text/html"), ("annotations", "include")],
                &cap,
            )
            .unwrap(),
        );
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:tree:src as=text/html\""),
            "{html}"
        );
        assert!(html.contains("view directory"), "{html}");
        assert!(html.contains("browse-annotation-path"), "{html}");
        assert!(!html.contains("hx-post"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn versions_lists_every_archived_content_version() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// v1\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();
        std::fs::write(root.join("a.rs"), "// v2\n").unwrap();
        source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();

        let listing =
            body(&source(&k, "urn:repo:demo:explain-versions:a.rs", &[], &cap()).unwrap());
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 2, "{listing}");
        assert!(lines.iter().all(|l| l.contains("code-v1@m1")), "{listing}");
        let hashes: std::collections::BTreeSet<&str> = lines
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap())
            .collect();
        assert_eq!(hashes.len(), 2, "two content versions: {listing}");

        let rows = json(&k, "urn:repo:demo:explain-versions:a.rs", &[]);
        assert_eq!(rows.as_array().unwrap().len(), 2, "{rows}");
        assert!(rows[0]["entry"]
            .as_str()
            .unwrap()
            .starts_with("urn:ikigai:browse:explain:demo:sha256:"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pre-0.2.2 archives wrote `ik:target` where the code now writes
    /// `ik:about` (the term was ceded to the routing family). Read
    /// compatibility, no migration: the entry-IRI lookup never keyed on the
    /// predicate, the loader accepts the legacy term, and the versions
    /// listing matches both.
    #[test]
    fn a_legacy_ik_target_archive_stays_addressable_and_listed() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// v1\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // Archive an entry, then rewrite it to the legacy shape in place.
        source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();
        let legacy: Vec<Quad> = store
            .quads_for_pattern(None, Some(ik("about").as_ref()), None, None)
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(legacy.len(), 1);
        for quad in &legacy {
            store.remove(quad).unwrap();
            store
                .insert(&Quad::new(
                    quad.subject.clone(),
                    ik("target"),
                    quad.object.clone(),
                    quad.graph_name.clone(),
                ))
                .unwrap();
        }

        // The archive hit does not re-derive (the key is the entry IRI)...
        let asks = log.count(FILE_PROVIDER);
        let row = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(log.count(FILE_PROVIDER), asks, "served from the archive");
        // ...the loader fills the subject from the legacy predicate...
        assert_eq!(row["about"], "urn:repo:demo:file:a.rs", "{row}");
        // ...and the versions listing still finds the legacy entry.
        let rows = json(&k, "urn:repo:demo:explain-versions:a.rs", &[]);
        assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A canned OpenAI-shaped completion transport for tests that mount the
    /// REAL ikigai-llm space (the `:model` identity resolution goes through
    /// genuine 0.10 endpoints; only the network hop is faked).
    struct CannedTransport;

    #[async_trait]
    impl ikigai_http::HttpTransport for CannedTransport {
        async fn send(
            &self,
            _request: ikigai_http::HttpRequest,
        ) -> std::result::Result<ikigai_http::HttpResponse, String> {
            Ok(ikigai_http::HttpResponse {
                status: 200,
                headers: vec![],
                body: br#"{"model":"m","choices":[{"message":{"role":"assistant","content":"An explanation."},"finish_reason":"stop"}]}"#.to_vec(),
            })
        }
    }

    /// The bake-off registry shape against real ikigai-llm: `coder` binds
    /// urn:llm:coder:ask (the file grain's provider), `rollup` is the registry
    /// default so the facade urn:llm:ask (the dir grain's provider) routes to
    /// it — each with a known model id for the tags to fold.
    fn real_llm_space() -> EndpointSpace {
        let mut coder = ikigai_llm::OpenAiConfig::ollama("qwen-test:9b");
        coder.provider = "coder".to_string();
        let mut rollup = ikigai_llm::OpenAiConfig::ollama("big-test:70b");
        rollup.provider = "rollup".to_string();
        let registry = ikigai_llm::Registry {
            default: "rollup".to_string(),
            providers: vec![coder, rollup],
        };
        ikigai_llm::space(Arc::new(CannedTransport), registry)
    }

    fn kernel_with_real_llm(root: &Path, config: ExplainConfig) -> Kernel {
        let browse =
            crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], config);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(real_llm_space()),
        ])))
    }

    #[test]
    fn version_tags_fold_the_true_model_identity_from_the_llm_module() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        // NO explicit labels: the tags must come from urn:llm:{provider}:model.
        let k = kernel_with_real_llm(&root, ExplainConfig::new(Arc::clone(&store)));

        // File grain: urn:llm:coder:ask ⇒ one resolve of urn:llm:coder:model.
        let file = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(file["version_tag"], "code-v1@qwen-test:9b");
        assert_eq!(file["model"], "qwen-test:9b");

        // Dir grain: the bare facade urn:llm:ask has no :model of its own —
        // the default provider comes from urn:llm:config, then ITS :model.
        let dir = json(&k, "urn:repo:demo:explain", &[]);
        assert_eq!(dir["version_tag"], "dir-v1@big-test:70b");
        assert_eq!(dir["model"], "big-test:70b");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_explicit_label_overrides_the_resolved_model_identity() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        // urn:llm:coder:model would say qwen-test:9b — the operator's label wins.
        let k = kernel_with_real_llm(
            &root,
            ExplainConfig::new(Arc::clone(&store)).file_model_label("pinned"),
        );
        let file = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(file["version_tag"], "code-v1@pinned");
        assert_eq!(file["model"], "pinned");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn without_a_model_resource_tags_fall_back_to_the_provider_heuristic() {
        // fake_llm_space binds only the :ask endpoints — the pre-0.10 shape
        // (no :model, no :config). Resolution failure must degrade the tag to
        // the provider-IRI heuristic, never fail the explain.
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let browse = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            ExplainConfig::new(Arc::clone(&store)), // no labels either
        );
        let k = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_llm_space(&log)),
        ])));

        let file = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(file["version_tag"], "code-v1@coder");
        let dir = json(&k, "urn:repo:demo:explain", &[]);
        assert_eq!(dir["version_tag"], "dir-v1@ask");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_answer_is_an_error_and_never_archived() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let empties = EndpointSpace::new().bind(
            Exact::new(FILE_PROVIDER),
            FnEndpoint::new("fake-llm", |_inv: &Invocation<'_>| {
                Ok(repr_utf8("text/plain", "   \n".to_string()))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        );
        let browse = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            ExplainConfig::new(Arc::clone(&store)),
        );
        let k = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(empties),
        ])));

        // A ceiling-starved model answering nothing must not poison the
        // archive: the resolution errors, and the archive stays empty.
        let err = source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("empty explanation"), "{err:?}");
        let listing =
            body(&source(&k, "urn:repo:demo:explain-versions:a.rs", &[], &cap()).unwrap());
        assert_eq!(listing, "");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn html_and_turtle_faces_render_the_provenance() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        let html = body(
            &source(
                &k,
                "urn:repo:demo:explain:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(html.contains("explained by m1 · code-v1@m1"), "{html}");
        assert!(html.contains("browse-explain"), "{html}");

        let out = source(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("as", "text/turtle")],
            &cap(),
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "text/turtle");
        let triples: Vec<_> = oxttl::TurtleParser::new()
            .for_slice(out.bytes.as_slice())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("turtle face must parse: {e}\n{}", body(&out)));
        assert!(!triples.is_empty());
        for t in &triples {
            assert!(!t.subject.to_string().starts_with("_:"), "no blank nodes");
        }
        let ttl = body(&out);
        assert!(ttl.contains("ik:versionTag \"code-v1@m1\""), "{ttl}");
        assert!(ttl.contains("ik:contentHash \"sha256:"), "{ttl}");
        assert!(ttl.contains("ik:about <urn:repo:demo:file:a.rs>"), "{ttl}");
        assert!(!ttl.contains("ik:target"), "the retired term: {ttl}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn describe_declares_the_capability_contract() {
        let root = temp_dir();
        let roots: Roots = Arc::new(std::collections::BTreeMap::from([(
            "demo".to_string(),
            root.clone(),
        )]));
        let config = Arc::new(ExplainConfig::new(Arc::new(Store::new().unwrap())));

        let explain = ExplainEndpoint {
            roots: Arc::clone(&roots),
            config: Arc::clone(&config),
        };
        let description = explain.describe();
        assert!(description.requires.contains(&CAP_WILDCARD.to_string()));
        assert!(description.requires.contains(&CAP_NET.to_string()));
        // No `repo` ArgSpec: every advertised row fixes the root in its
        // pattern; the binding is grammar-injected.
        let names: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["path", "version", "provider", "annotations", "as"]);

        // The manifold publishes the allowlist: `provider`'s one_of IS what
        // this host permits, so validate can reject before dispatch and a UI
        // can build its menu from the description alone. By default that is
        // exactly the two backends explain already asks — no new reach.
        let provider_spec = description
            .inputs
            .iter()
            .find(|i| i.name == "provider")
            .unwrap();
        assert_eq!(provider_spec.one_of, ["urn:llm:ask", "urn:llm:coder:ask"]);
        let widened = ExplainEndpoint {
            roots: Arc::clone(&roots),
            config: Arc::new(
                ExplainConfig::new(Arc::new(Store::new().unwrap()))
                    .allow_provider("urn:llm:batch:ask"),
            ),
        }
        .describe();
        let widened = widened
            .inputs
            .iter()
            .find(|i| i.name == "provider")
            .unwrap();
        assert_eq!(
            widened.one_of,
            ["urn:llm:ask", "urn:llm:batch:ask", "urn:llm:coder:ask"]
        );

        // The versions listing never derives — it must NOT demand net.
        use ikigai_core::Endpoint as _;
        let versions = VersionsEndpoint {
            roots: Arc::clone(&roots),
            config: Arc::clone(&config),
        }
        .describe();
        assert!(versions.requires.contains(&CAP_WILDCARD.to_string()));
        assert!(!versions.requires.contains(&CAP_NET.to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn explain_annotations_include_folds_the_targets_annotations() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // Annotate through the kernel — space_with_explain carries S2 too.
        let annotate_cap = Capability::scoped(["urn:cap:browse:read:demo", crate::CAP_ANNOTATE]);
        let request = Request::new(Verb::Sink, Iri::parse("urn:annotation:n1").unwrap())
            .with_arg(
                "target",
                ArgRef::Inline(b"urn:repo:demo:file:src/lib.rs".to_vec()),
            )
            .with_arg("exact", ArgRef::Inline(b"fn main()".to_vec()))
            .with_arg("body", ArgRef::Inline(b"the entry point".to_vec()));
        block_on(k.issue(request, &annotate_cap)).unwrap();

        // The json face gains an annotations array — the same row shape the
        // listing endpoint serves.
        let row = json(
            &k,
            "urn:repo:demo:explain:src/lib.rs",
            &[("annotations", "include")],
        );
        assert_eq!(row["annotations"][0]["exact"], "fn main()");
        assert_eq!(row["annotations"][0]["body"], "the entry point");
        assert_eq!(row["annotations"][0]["line"], 1);
        assert_eq!(row["annotations"][0]["orphaned"], false);

        // Without the arg the json shape is unchanged.
        let bare = json(&k, "urn:repo:demo:explain:src/lib.rs", &[]);
        assert!(bare.get("annotations").is_none(), "{bare}");

        // The text face appends the margin-notes section.
        let text = body(
            &source(
                &k,
                "urn:repo:demo:explain:src/lib.rs",
                &[("annotations", "include")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(text.contains("--- annotations (1) ---"), "{text}");
        assert!(
            text.contains("L1 \"fn main()\" -- the entry point"),
            "{text}"
        );

        // A directory rollup folds its subtree's annotations, path-prefixed.
        let rollup = body(
            &source(
                &k,
                "urn:repo:demo:explain",
                &[("annotations", "include")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(
            rollup.contains("src/lib.rs L1 \"fn main()\" -- the entry point"),
            "{rollup}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The arc: a caller names the backend for THIS derivation, and the
    /// archive keys on that backend's OWN model identity — so two models
    /// coexist, both list, and either is addressable afterwards.
    #[test]
    fn a_request_provider_derives_against_it_and_keys_by_its_own_model() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        // No labels: the tags must come from each provider's own :model.
        let k = kernel_with_real_llm(&root, ExplainConfig::new(Arc::clone(&store)));

        // Default: the configured file tier (coder ⇒ qwen-test:9b).
        let default = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(default["version_tag"], "code-v1@qwen-test:9b");

        // The same file, the OTHER backend — allowed by default because it is
        // the configured dir tier. Its own model identity keys the entry.
        let picked = json(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", "urn:llm:ask")],
        );
        assert_eq!(picked["version_tag"], "code-v1@big-test:70b");
        assert_eq!(picked["model"], "big-test:70b");
        assert_eq!(picked["derived"], true);

        // Two coexisting entries at ONE content hash, both listed.
        let rows = json(&k, "urn:repo:demo:explain-versions:a.rs", &[]);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        let tags: std::collections::BTreeSet<&str> = rows
            .iter()
            .map(|r| r["version_tag"].as_str().unwrap())
            .collect();
        assert_eq!(
            tags,
            ["code-v1@big-test:70b", "code-v1@qwen-test:9b"]
                .into_iter()
                .collect()
        );
        let hashes: std::collections::BTreeSet<&str> = rows
            .iter()
            .map(|r| r["content_hash"].as_str().unwrap())
            .collect();
        assert_eq!(hashes.len(), 1, "same content, two models: {rows:?}");

        // And version= still fetches either, from the archive.
        for tag in ["code-v1@qwen-test:9b", "code-v1@big-test:70b"] {
            let old = json(&k, "urn:repo:demo:explain:a.rs", &[("version", tag)]);
            assert_eq!(old["version_tag"], tag);
            assert_eq!(old["derived"], false, "{tag}");
        }

        // Re-asking for the picked backend is an archive hit, not a re-derive.
        let again = json(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", "urn:llm:ask")],
        );
        assert_eq!(again["derived"], false);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The precedence trap: the operator's label describes the OPERATOR'S
    /// backend. A request that picks a different one must resolve that one's
    /// identity — stamping the override onto another model's output would
    /// write a wrong model identity into the archive KEY, indistinguishable
    /// from a legitimate entry ever after.
    #[test]
    fn a_request_provider_never_inherits_the_operators_label() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel_with_real_llm(
            &root,
            ExplainConfig::new(Arc::clone(&store)).file_model_label("pinned"),
        );

        // Omitted: exactly today's behaviour — the operator's label wins.
        let default = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(default["version_tag"], "code-v1@pinned");

        // Naming the configured provider explicitly is the SAME request: the
        // label describes that backend, so it still applies (archive hit).
        let same = json(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", "urn:llm:coder:ask")],
        );
        assert_eq!(same["version_tag"], "code-v1@pinned");
        assert_eq!(same["derived"], false, "identical to omitting the argument");

        // A DIFFERENT backend resolves its own identity, never "pinned".
        let other = json(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", "urn:llm:ask")],
        );
        assert_eq!(other["version_tag"], "code-v1@big-test:70b");
        assert_eq!(other["model"], "big-test:70b");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The authority boundary: only what the operator configured or allowed.
    /// A refusal names what was asked for and what is on offer, and nothing
    /// is asked of any backend.
    #[test]
    fn a_provider_outside_the_allowlist_is_refused_loudly() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        // Bound in the kernel, but not offered by this host's explain config.
        let err = source(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", BATCH_PROVIDER)],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        let text = format!("{err:?}");
        assert!(
            text.contains(BATCH_PROVIDER),
            "names what was asked: {text}"
        );
        assert!(
            text.contains(FILE_PROVIDER),
            "names what is offered: {text}"
        );
        // A refusal is read by a human: no collapsed line-continuation runs.
        assert!(!text.contains("  "), "message reads as prose: {text}");
        assert_eq!(log.count(BATCH_PROVIDER), 0, "refused before any ask");
        assert_eq!(log.count(FILE_PROVIDER), 0, "and never fell back");

        // A provider that does not exist at all is the same refusal.
        let err = source(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", "urn:llm:vendor:ask")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // The operator opts in, and the same request now derives — under the
        // NEW backend's label (never the operator's dir/file override).
        let store2 = Arc::new(Store::new().unwrap());
        let k2 = kernel_with(&root, &store2, &log, |c| c.allow_provider(BATCH_PROVIDER));
        let row = json(
            &k2,
            "urn:repo:demo:explain:a.rs",
            &[("provider", BATCH_PROVIDER)],
        );
        assert_eq!(row["version_tag"], "code-v1@batch");
        assert_eq!(log.count(BATCH_PROVIDER), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The overrides are scoped to the TIER KNOB they live on, not to the
    /// backend: a file-grain request that picks the dir tier's provider does
    /// not inherit `dir_model_label`. Falling through resolves that
    /// backend's true id instead, so the worst case is losing the operator's
    /// nickname — never gaining a wrong identity in the key.
    #[test]
    fn a_tiers_label_does_not_leak_across_to_the_other_grain() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        // kernel_with sets file_model_label("m1") AND dir_model_label("d1").
        let k = kernel_with(&root, &store, &log, |c| c);

        let row = json(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("provider", DIR_PROVIDER)],
        );
        // Neither the file tier's label (wrong backend) nor the dir tier's
        // (right backend, wrong knob) — the resolved identity, here the
        // heuristic because the fake space binds no :model.
        assert_eq!(row["version_tag"], "code-v1@ask");
        assert_eq!(row["model"], "ask");
        assert_eq!(log.count(DIR_PROVIDER), 1, "the file grain asked it");
        assert_eq!(log.count(FILE_PROVIDER), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A rollup applies the chosen backend to ITSELF; the children are plain
    /// sub-resolutions carrying no argument, so they keep their own tier and
    /// their own tags — the same mixing the two-tier default already does.
    #[test]
    fn a_rollups_provider_does_not_propagate_to_its_children() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        std::fs::write(root.join("b.rs"), "// b\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c.allow_provider(BATCH_PROVIDER));

        let row = json(&k, "urn:repo:demo:explain", &[("provider", BATCH_PROVIDER)]);
        assert_eq!(row["version_tag"], "dir-v1@batch");
        assert_eq!(log.count(BATCH_PROVIDER), 1, "the rollup only");
        assert_eq!(log.count(FILE_PROVIDER), 2, "children keep their tier");
        assert_eq!(log.count(DIR_PROVIDER), 0);

        // The children archived under the untouched file-tier tag.
        let child = json(&k, "urn:repo:demo:explain:a.rs", &[]);
        assert_eq!(child["version_tag"], "code-v1@m1");
        assert_eq!(child["derived"], false);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two ways to name the model in one request, and they can disagree —
    /// `version=` addresses an archived entry and derives nothing, so pairing
    /// it with `provider=` is a contradiction, not a preference.
    #[test]
    fn version_and_provider_together_are_refused() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, |c| c);

        let err = source(
            &k,
            "urn:repo:demo:explain:a.rs",
            &[("version", "code-v1@m1"), ("provider", FILE_PROVIDER)],
            &cap(),
        )
        .unwrap_err();
        let text = format!("{err:?}");
        assert!(
            text.contains("code-v1@m1") && text.contains(FILE_PROVIDER),
            "{text}"
        );
        // A refusal is read by a human: no collapsed line-continuation runs.
        assert!(!text.contains("  "), "message reads as prose: {text}");
        assert_eq!(log.count(FILE_PROVIDER), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the option menu ----------------------------------------------------

    /// A fake `urn:llm:models` over a literal inventory, counting resolves —
    /// the counter is how the tests observe that a menu costs ONE inventory
    /// read whatever the listing holds, and that a render costs none.
    fn fake_models_space(body: &str, calls: &Arc<AtomicU32>) -> EndpointSpace {
        let inventory: serde_json::Value = serde_json::from_str(body).unwrap();
        let text = body.to_string();
        let counter = Arc::clone(calls);
        let mut space = EndpointSpace::new().bind(
            Exact::new("urn:llm:models"),
            FnEndpoint::new("fake-llm-models", move |_inv: &Invocation<'_>| {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(repr("application/json", text.clone()))
            })
            .with_description(Description::new("fake-llm-models").verb(Verb::Source)),
        );
        // The per-provider identity faces the VERSION TAGS fold, agreeing with
        // the inventory the MENU reads — as they do on a real host, where both
        // report the provider's configured model. (They are still two
        // resources: a discovering provider can report null to the cheap
        // inventory and a probed id to `:model`. The menu is allowed to know
        // less; it may never claim more.)
        let default = inventory["default"].as_str().unwrap().to_string();
        for (name, entry) in inventory["models"].as_object().unwrap() {
            let Some(model) = entry["model"].as_str().map(str::to_string) else {
                continue;
            };
            let mut iris = vec![format!("urn:llm:{name}:model")];
            if *name == default {
                iris.push("urn:llm:model".to_string());
            }
            for iri in iris {
                let model = model.clone();
                space = space.bind(
                    Exact::new(iri),
                    FnEndpoint::new("fake-llm-model", move |_inv: &Invocation<'_>| {
                        Ok(repr_utf8("text/plain", model.clone()))
                    })
                    .with_description(Description::new("fake-llm-model").verb(Verb::Source)),
                );
            }
        }
        let config = serde_json::json!({ "default": default }).to_string();
        space.bind(
            Exact::new("urn:llm:config"),
            FnEndpoint::new("fake-llm-config", move |_inv: &Invocation<'_>| {
                Ok(repr("application/json", config.clone()))
            })
            .with_description(Description::new("fake-llm-config").verb(Verb::Source)),
        )
    }

    /// Two backends on ONE model (coder and batch both serve `same:1b`), a
    /// third on its own (`gp`, which the bare `urn:llm:ask` facade routes to).
    const INVENTORY: &str = r#"{
        "default": "gp",
        "models": {
            "coder": {"backend": "urn:llm:coder:ask", "model": "same:1b"},
            "batch": {"backend": "urn:llm:batch:ask", "model": "same:1b"},
            "gp":    {"backend": "urn:llm:gp:ask",    "model": "big:70b"}
        }
    }"#;

    fn kernel_with_inventory(
        root: &Path,
        store: &Arc<Store>,
        log: &Arc<Log>,
        inventory: &str,
        calls: &Arc<AtomicU32>,
        config: impl FnOnce(ExplainConfig) -> ExplainConfig,
    ) -> Kernel {
        let cfg = config(ExplainConfig::new(Arc::clone(store)));
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_models_space(inventory, calls)),
            Arc::new(fake_llm_space(log)),
        ])))
    }

    fn menu(kernel: &Kernel, iri: &str) -> String {
        body(&source(kernel, iri, &[("as", "text/html")], &cap()).unwrap())
    }

    /// Every `provider=` the menu emits, in order.
    fn offered_providers(html: &str) -> Vec<String> {
        html.match_indices("provider=")
            .map(|(i, _)| {
                html[i + "provider=".len()..]
                    .split(['"', ' '])
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// Every `version=` the menu emits as a reopen button, in order.
    fn offered_versions(html: &str) -> Vec<String> {
        html.match_indices("version=")
            .map(|(i, _)| {
                html[i + "version=".len()..]
                    .split(['"', ' '])
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn the_menu_offers_exactly_the_selectable_set_and_never_more() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| {
            c.allow_provider(BATCH_PROVIDER)
        });

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        let offered = offered_providers(&html);
        // The selectable set is {ask, coder, batch}; coder and batch serve one
        // model, so the menu is TWO rows over THREE selectable providers.
        assert_eq!(offered.len(), 2, "{html}");

        // Every offered provider is one `provider=` accepts — the menu can
        // never render a click that comes back Denied.
        for provider in &offered {
            assert!(
                source(
                    &k,
                    "urn:repo:demo:explain:a.rs",
                    &[("provider", provider)],
                    &cap()
                )
                .is_ok(),
                "menu offered `{provider}`, which explain refused"
            );
        }
        // And a bound backend the operator did NOT make selectable is absent
        // (urn:llm:gp:ask is in the inventory but not in the allowlist).
        assert!(!html.contains("urn:llm:gp:ask"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_providers_serving_one_model_are_one_row_naming_the_configured_one() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| {
            c.allow_provider(BATCH_PROVIDER)
        });

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        // ONE row for `same:1b`, not two — the archive would key them the same,
        // so a second row would promise an explanation it cannot produce.
        assert_eq!(html.matches("same:1b").count(), 1, "{html}");
        // The backends are stated as a fact beside it, never as a second button.
        assert!(html.contains("served by coder, batch"), "{html}");
        // And the button names the CONFIGURED tier, not the alphabetically
        // first of the pair — a miss must derive where the operator said.
        assert_eq!(
            offered_providers(&html),
            vec![DIR_PROVIDER.to_string(), FILE_PROVIDER.to_string()],
            "{html}"
        );
        // The markup says why there is one row, in the panel itself.
        assert!(
            html.contains("One row per model, not per backend"),
            "{html}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unknown_model_never_collapses_two_providers() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        // Since ikigai-llm 0.12 a provider may pin no model and discover it —
        // an unreachable backend then reports `null`. Two such providers are
        // two rows: they may well be two models, and the menu's own ignorance
        // must not read as a claim that they are the same.
        let unknown = r#"{"default":"gp","models":{
            "coder":{"backend":"urn:llm:coder:ask","model":null},
            "batch":{"backend":"urn:llm:batch:ask","model":null},
            "gp":{"backend":"urn:llm:gp:ask","model":"big:70b"}}}"#;
        let k = kernel_with_inventory(&root, &store, &log, unknown, &calls, |c| {
            c.allow_provider(BATCH_PROVIDER)
        });

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        let offered = offered_providers(&html);
        assert_eq!(offered.len(), 3, "unknown models must not group: {html}");
        assert!(offered.contains(&FILE_PROVIDER.to_string()), "{html}");
        assert!(offered.contains(&BATCH_PROVIDER.to_string()), "{html}");
        // Each unknown row says so rather than inventing an identity, and is
        // labelled by the same heuristic its version tag will carry.
        assert_eq!(
            html.matches("backend reports no model id").count(),
            2,
            "{html}"
        );
        assert!(html.contains(">coder</button>"), "{html}");
        assert!(html.contains(">batch</button>"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_archived_explanation_reopens_from_the_menu_without_deriving() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| c);

        let first = body(&source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap());
        assert_eq!(log.count(FILE_PROVIDER), 1);

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        let versions = offered_versions(&html);
        assert_eq!(versions, vec!["code-v1@same:1b".to_string()], "{html}");
        // The tag the menu offers reopens the archived text, and pays nothing.
        let again = body(
            &source(
                &k,
                "urn:repo:demo:explain:a.rs",
                &[("version", &versions[0])],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(again, first);
        assert_eq!(log.count(FILE_PROVIDER), 1, "reopening must not derive");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn entries_under_earlier_content_are_listed_but_never_offered() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| c);
        source(&k, "urn:repo:demo:explain:a.rs", &[], &cap()).unwrap();

        // Edit: the content re-keys, and the archived entry is no longer
        // addressable by `version=` (which resolves against the CURRENT hash).
        std::fs::write(root.join("a.rs"), "// a changed\n").unwrap();
        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        assert!(offered_versions(&html).is_empty(), "{html}");
        assert!(html.contains("archived under earlier"), "{html}");
        // The text face still lists it — the archive outlives the content.
        let listing =
            body(&source(&k, "urn:repo:demo:explain-versions:a.rs", &[], &cap()).unwrap());
        assert!(listing.contains("code-v1@same:1b"), "{listing}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_grain_with_no_versions_still_renders_its_choices() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| c);

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        assert!(html.contains("nothing yet"), "{html}");
        assert!(offered_versions(&html).is_empty(), "{html}");
        // The derive half is unaffected: an empty archive is the ordinary
        // first-visit state, not a degraded one.
        assert_eq!(offered_providers(&html).len(), 2, "{html}");
        assert_eq!(log.count(FILE_PROVIDER), 0, "a menu never derives");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_menu_renders_with_no_llm_provider_bound_at_all() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        // Browse alone: no urn:llm:models, no backends. The hosts that mount
        // this crate must degrade, not fail.
        let browse = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            ExplainConfig::new(Arc::clone(&store)),
        );
        let k = Kernel::new(Arc::new(browse));

        let html = menu(&k, "urn:repo:demo:explain-versions:a.rs");
        // Both configured tiers are still offered, labelled by the same
        // provider heuristic their version tags fall back to.
        assert_eq!(
            offered_providers(&html),
            vec![DIR_PROVIDER.to_string(), FILE_PROVIDER.to_string()],
            "{html}"
        );
        assert!(html.contains(">coder</button>"), "{html}");
        assert!(html.contains(">ask</button>"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_listing_renders_one_menu_and_reads_the_inventory_only_when_opened() {
        let root = temp_dir();
        for n in 0..12 {
            std::fs::write(root.join(format!("f{n}.rs")), "// x\n").unwrap();
        }
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| c);

        // Rendering the directory: twelve rows, ONE menu, and not a single
        // inventory read — the panel is a separate resolution, fetched on the
        // disclosure's toggle.
        let tree = body(&source(&k, "urn:repo:demo:tree", &[("as", "text/html")], &cap()).unwrap());
        assert_eq!(tree.matches("browse-explain-menu\"").count(), 1, "{tree}");
        assert_eq!(tree.matches("browse-explain-link").count(), 13); // header + 12 rows
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "a listing must not fan out"
        );

        // Opening it: exactly one, however many entries the directory holds.
        menu(&k, "urn:repo:demo:explain-versions");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        menu(&k, "urn:repo:demo:explain-versions");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "one per opening, not per row"
        );
        assert_eq!(log.count(FILE_PROVIDER), 0, "a menu never derives");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_explain_page_carries_the_menu_and_the_versions_face_declares_it() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_inventory(&root, &store, &log, INVENTORY, &calls, |c| c);

        let page = body(
            &source(
                &k,
                "urn:repo:demo:explain:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        // The disclosure is keyboard-operable by construction (details/summary)
        // and lazy: it names the versions resource, it does not embed it.
        assert!(
            page.contains("<details class=\"browse-explain-menu\">"),
            "{page}"
        );
        assert!(page.contains("<summary>explain with…</summary>"), "{page}");
        assert!(
            page.contains("urn:repo:demo:explain-versions:a.rs"),
            "{page}"
        );
        assert!(
            page.contains("hx-trigger=\"toggle once from:closest details\""),
            "{page}"
        );
        // Only one inventory read happened, and it was NOT this page's.
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "the page itself reads no inventory"
        );

        // The html face is declared, so a client can find it without guessing.
        let as_spec = VersionsEndpoint {
            roots: Arc::new(std::collections::BTreeMap::from([(
                "demo".to_string(),
                root.clone(),
            )])),
            config: Arc::new(ExplainConfig::new(Arc::clone(&store))),
        }
        .describe();
        let face = as_spec.inputs.iter().find(|i| i.name == "as").unwrap();
        assert!(
            face.one_of.contains(&"text/html".to_string()),
            "{:?}",
            face.one_of
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn iso8601_renders_epoch_millis() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(86_400_123), "1970-01-02T00:00:00.123Z");
        assert_eq!(iso8601(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(iso8601(4_102_444_800_000), "2100-01-01T00:00:00.000Z");
    }
}
