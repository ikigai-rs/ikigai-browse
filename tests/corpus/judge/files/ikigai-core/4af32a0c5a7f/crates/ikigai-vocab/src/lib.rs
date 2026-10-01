//! The ikigai self-description vocabulary and its RDF (Turtle) projection.
//!
//! Endpoints describe themselves with a neutral [`Description`] (in
//! `ikigai-core`); this crate renders that to RDF so a `Meta` request can return
//! a machine-readable description of an endpoint.
//!
//! The Turtle is hand-rolled, which keeps this crate dependency-free and
//! WASM-trivial — the emitted graph is small and fully controlled. (A future
//! revision may project to Hydra / OpenAPI from the same vocabulary.)

use ikigai_core::{
    Description, EndpointSpace, Error, Exact, FnEndpoint, InputSource, Invocation, MetaRenderer,
    ReprType, Representation, Result, Verb,
};

/// The ikigai vocabulary namespace. Provisional — the canonical namespace IRI
/// is a project decision; it is used here purely as a stable identifier.
pub const NS: &str = "https://ikigai-rs.dev/ns#";

/// The ikigai vocabulary itself, as a Turtle ontology — hand-maintained in
/// `src/vocabulary.ttl` and bundled at compile time. Defines the `ns#` classes
/// (`ik:Endpoint`, `ik:Transreptor ⊏ ik:Endpoint`, the process classes `ik:Process` /
/// `ik:Step` / `ik:Argument` / `ik:Fork`, …) and properties. Served by [`space`] at
/// `urn:ikigai:vocab`, and eventually at the external `ns#` URL. Its `owl:versionInfo`
/// is this crate's version (a test enforces it), so a deployed copy can be dated.
pub const VOCABULARY: &str = include_str!("vocabulary.ttl");

/// SHACL shapes for the process vocabulary — what a plan must look like before any
/// step runs: one verb from the five per step, one target, one result, an argument by
/// value or by reference but not both, every edge and `@name` reference inside the plan,
/// each name bound once. Hand-maintained in `src/shapes.ttl`, bundled at compile time,
/// parsed for syntax by a test. **Data only**: this crate has no SHACL engine and must
/// not gain one; `ikigai-shacl` runs them. What they cannot say (full acyclicity — a
/// reference goes through a name, a hop a property path cannot take; catalog binding;
/// ArgSpec fit; transreption routes; capability) is stated in the file's header and
/// belongs to the plan validator.
pub const SHAPES: &str = include_str!("shapes.ttl");

/// A JSON-LD `@context` for the whole vocabulary — every `ns#` term mapped to its
/// short name, with datatype/`@id` coercions (integers, booleans, and IRI-valued
/// properties like `ik:cors`/`ik:shape` typed correctly). **Generated from
/// [`VOCABULARY`]**, so it never drifts from the terms. Serve it at the external
/// `ns#` URL under content negotiation (`application/ld+json`) alongside the Turtle,
/// so a document's `"@context": "https://ikigai-rs.dev/ns"` resolves — letting
/// config surfaces (e.g. the `urn:web:routes` route table) be authored in plain
/// JSON/YAML that lifts to the same RDF.
pub const CONTEXT: &str = include_str!("context.jsonld");

/// The conventional IRI the vocabulary is bound to by [`space`].
pub const VOCAB_IRI: &str = "urn:ikigai:vocab";

/// The plan IRI scheme — the one place it is coded, so the engine that emits a plan, the
/// validator that reads one, and the fixtures that pin the shape agree on every IRI.
///
/// A plan and everything in it is skolemized under `urn:plan:{plan-id}` (no blank nodes:
/// the graph diffs, signs, and answers SPARQL). `{plan-id}` is the program's name when the
/// plan is stored — `urn:program:{name}` is then the resolvable face that RUNS it, and
/// `urn:plan:{name}` the graph that DESCRIBES it — and the plan's content address when it
/// is anonymous. Every author-supplied segment passes through
/// [`ikigai_core::escape_iri_fragment`], so a hostile name cannot close the IRI early.
///
/// ```
/// use ikigai_vocab::plan;
///
/// assert_eq!(plan::process_iri("linkcheck"), "urn:plan:linkcheck");
/// assert_eq!(plan::step_iri("linkcheck", 2), "urn:plan:linkcheck:step:2");
/// assert_eq!(plan::var_iri("linkcheck", "checks"), "urn:plan:linkcheck:var:checks");
/// assert_eq!(plan::fork_iri("linkcheck", 1), "urn:plan:linkcheck:fork:1");
/// assert_eq!(plan::input_iri("linkcheck", "ttl"), "urn:plan:linkcheck:input:ttl");
/// assert_eq!(
///     plan::argument_iri("linkcheck", 4, "content"),
///     "urn:plan:linkcheck:step:4:arg:content"
/// );
/// // A content-addressed (anonymous) plan keeps its colons; a hostile name does not
/// // escape the IRI.
/// assert_eq!(plan::step_iri("sha256:9f2c", 1), "urn:plan:sha256:9f2c:step:1");
/// assert_eq!(plan::var_iri("p", "x> . <urn:y"), "urn:plan:p:var:x%3E%20.%20%3Curn:y");
/// ```
pub mod plan {
    use ikigai_core::escape_iri_fragment;

    /// The IRI of the plan itself: `urn:plan:{plan-id}`.
    pub fn process_iri(plan_id: &str) -> String {
        format!("urn:plan:{}", escape_iri_fragment(plan_id))
    }

    /// One step, by its 1-based position in the text face: `urn:plan:{plan-id}:step:{n}`.
    pub fn step_iri(plan_id: &str, n: usize) -> String {
        format!("{}:step:{n}", process_iri(plan_id))
    }

    /// A bound name — a step's `ik:binds` or a parameter's `ik:inputName` — which is what
    /// an `ik:ref` cites: `urn:plan:{plan-id}:var:{name}`.
    pub fn var_iri(plan_id: &str, name: &str) -> String {
        format!("{}:var:{}", process_iri(plan_id), escape_iri_fragment(name))
    }

    /// One fork: `urn:plan:{plan-id}:fork:{n}`.
    pub fn fork_iri(plan_id: &str, n: usize) -> String {
        format!("{}:fork:{n}", process_iri(plan_id))
    }

    /// A declared parameter's ArgSpec node: `urn:plan:{plan-id}:input:{name}` — the same
    /// `:input:{name}` suffix an endpoint's inputs carry under `urn:ikigai:endpoint:{id}`.
    pub fn input_iri(plan_id: &str, name: &str) -> String {
        format!(
            "{}:input:{}",
            process_iri(plan_id),
            escape_iri_fragment(name)
        )
    }

    /// One named argument of a step: `urn:plan:{plan-id}:step:{n}:arg:{name}`.
    pub fn argument_iri(plan_id: &str, n: usize, name: &str) -> String {
        format!("{}:arg:{}", step_iri(plan_id, n), escape_iri_fragment(name))
    }
}

/// Escape a string for use inside a Turtle double-quoted literal.
fn escape_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn lit(s: &str) -> String {
    format!("\"{}\"", escape_literal(s))
}

/// Emit an author-supplied value that is *meant* to be an IRI (a declared `class`, a
/// capability scope) — as a resource when it really is one, else as a literal.
///
/// The counterpart to [`lit`], and it exists for the same reason: an emitter must never
/// trust that the strings it interpolates are shaped the way a doc comment says.
/// `Description` validates nothing at construction, so a `>` in any of these would close
/// the IRI early and everything after it would parse as Turtle.
///
/// **Two defenses, because escaping alone is not enough.** Percent-encoding
/// ([`ikigai_core::escape_iri_fragment`]) makes the value a legal Turtle `IRIREF`, but a
/// legal `IRIREF` is not necessarily a legal IRI: encoding `http://a> . <b` yields
/// `http://a%3E%20.%20%3Cb`, whose authority has a non-numeric "port", and an RDF parser
/// rejects the whole document. So the escaped form is parsed, and anything that still
/// isn't an IRI degrades to a literal — wrong *type* for the predicate, but visible in
/// the graph and never a parse failure for consumers.
///
/// Every `class` and `requires` value in the ecosystem is already a valid IRI, so this
/// changes no existing output.
fn iri_term(s: &str) -> String {
    let escaped = ikigai_core::escape_iri_fragment(s);
    match ikigai_core::Iri::parse(escaped.as_ref()) {
        Ok(_) => format!("<{escaped}>"),
        Err(_) => lit(s),
    }
}

fn verb_name(verb: Verb) -> &'static str {
    match verb {
        Verb::Source => "Source",
        Verb::Sink => "Sink",
        Verb::Exists => "Exists",
        Verb::Delete => "Delete",
        Verb::Meta => "Meta",
    }
}

fn source_name(source: InputSource) -> &'static str {
    match source {
        InputSource::Argument => "argument",
        InputSource::Binding => "binding",
    }
}

/// A capability scope is an IRI when it looks like one (`urn:…`, `http(s)://…`) — emitted
/// as a resource so selection can join on it — else a legacy descriptive label (literal).
fn cap_term(scope: &str) -> String {
    if scope.starts_with("urn:") || scope.starts_with("http://") || scope.starts_with("https://") {
        iri_term(scope)
    } else {
        lit(scope)
    }
}

/// The predicates of one input node (shared by the endpoint-level and action-level forms).
fn input_predicates(input: &ikigai_core::ArgSpec) -> String {
    let mut node = format!(
        "ik:inputName {} ;
    ik:source {} ;
    ik:required {}",
        lit(&input.name),
        lit(source_name(input.source)),
        input.required
    );
    if !input.summary.is_empty() {
        node.push_str(&format!(
            " ;
    ik:summary {}",
            lit(&input.summary)
        ));
    }
    if let Some(class) = &input.class {
        // The declared class/datatype is an IRI — emit it as a resource, not a literal.
        node.push_str(&format!(
            " ;
    ik:class {}",
            iri_term(class)
        ));
    }
    if let Some(default) = &input.default {
        node.push_str(&format!(
            " ;
    ik:default {}",
            lit(default)
        ));
    }
    for value in &input.one_of {
        node.push_str(&format!(
            " ;
    ik:oneOf {}",
            lit(value)
        ));
    }
    node
}

/// Render a [`Description`] as a Turtle document using the ikigai vocabulary.
///
/// Every identifier that lands in an IRI position — the endpoint id, each input name,
/// each declared class, each capability scope — is percent-encoded on the way out (see
/// [`ikigai_core::escape_iri_fragment`], and the private `iri_term` for the author-supplied IRIs);
/// `Description` enforces nothing at construction, so this function must not assume
/// anything about their shape. The output always parses. Call
/// [`Description::validate`] if you want the *description* refused instead.
pub fn to_turtle(description: &Description) -> String {
    // The endpoint's node, computed ONCE so the subject and every IRI hung off it
    // (inputs, actions) cannot disagree about how the id was encoded.
    let endpoint_iri = format!(
        "urn:ikigai:endpoint:{}",
        ikigai_core::escape_iri_fragment(&description.id)
    );
    let subject = format!("<{endpoint_iri}>");

    // A transreptor is typed as both classes explicitly (`ik:Transreptor ⊏ ik:Endpoint`),
    // so consumers that don't reason over the subclass axiom still see `ik:Endpoint`.
    let rdf_type = if description.transreption().is_some() {
        "a ik:Endpoint, ik:Transreptor"
    } else {
        "a ik:Endpoint"
    };
    let mut predicates: Vec<String> = vec![
        rdf_type.to_string(),
        format!("ik:id {}", lit(&description.id)),
    ];
    if !description.title.is_empty() {
        predicates.push(format!("ik:title {}", lit(&description.title)));
    }
    if !description.summary.is_empty() {
        predicates.push(format!("ik:summary {}", lit(&description.summary)));
    }
    if !description.verbs.is_empty() {
        let verbs = description
            .verbs
            .iter()
            .map(|v| lit(verb_name(*v)))
            .collect::<Vec<_>>()
            .join(", ");
        predicates.push(format!("ik:verb {verbs}"));
    }
    if !description.outputs.is_empty() {
        let outputs = description
            .outputs
            .iter()
            .map(|o| lit(o))
            .collect::<Vec<_>>()
            .join(", ");
        predicates.push(format!("ik:output {outputs}"));
    }
    if !description.requires.is_empty() {
        let reqs = description
            .requires
            .iter()
            .map(|c| cap_term(c))
            .collect::<Vec<_>>()
            .join(", ");
        predicates.push(format!("ik:requires {reqs}"));
    }
    if let Some(t) = description.transreption() {
        if !t.from.is_empty() {
            let from = t.from.iter().map(|m| lit(m)).collect::<Vec<_>>().join(", ");
            predicates.push(format!("ik:transreptsFrom {from}"));
        }
        if !t.to.is_empty() {
            let to = t.to.iter().map(|m| lit(m)).collect::<Vec<_>>().join(", ");
            predicates.push(format!("ik:transreptsTo {to}"));
        }
    }
    // Flat inputs: skolemized under the endpoint (stable IRIs — catalogs SPARQL and
    // diff cleanly; no blank nodes). Actions synthesized from the flat form REFERENCE
    // these same nodes, so the spec body is stated once.
    let mut extra_nodes: Vec<String> = Vec::new();
    for input in &description.inputs {
        let node_iri = format!(
            "{endpoint_iri}:input:{}",
            ikigai_core::escape_iri_fragment(&input.name)
        );
        predicates.push(format!("ik:input <{node_iri}>"));
        extra_nodes.push(format!("<{node_iri}> {} .", input_predicates(input)));
    }

    // The per-verb ACTION view — the unit of selection. One ik:Action node per
    // non-Meta verb, normalized from either authoring form (explicit ActionSpec
    // wins; flat fields synthesize the rest), so catalog consumers never know
    // which form authored an endpoint.
    for action in description.action_specs() {
        let verb = verb_name(action.verb);
        let action_iri = format!("{endpoint_iri}:action:{}", verb.to_lowercase());
        predicates.push(format!("ik:action <{action_iri}>"));
        let mut preds: Vec<String> =
            vec!["a ik:Action".to_string(), format!("ik:verb {}", lit(verb))];
        if !action.summary.is_empty() {
            preds.push(format!("ik:summary {}", lit(&action.summary)));
        }
        for output in &action.outputs {
            preds.push(format!("ik:output {}", lit(output)));
        }
        for cap in &action.requires {
            preds.push(format!("ik:requires {}", cap_term(cap)));
        }
        let synthesized = !description.actions.iter().any(|a| a.verb == action.verb);
        for input in &action.inputs {
            let name = ikigai_core::escape_iri_fragment(&input.name);
            let node_iri = if synthesized {
                // the flat input node already emitted above — reference it
                format!("{endpoint_iri}:input:{name}")
            } else {
                let scoped = format!("{action_iri}:input:{name}");
                extra_nodes.push(format!("<{scoped}> {} .", input_predicates(input)));
                scoped
            };
            preds.push(format!("ik:input <{node_iri}>"));
        }
        extra_nodes.push(format!("<{action_iri}> {} .", preds.join(" ;\n    ")));
    }

    let mut ttl = format!(
        "@prefix ik: <{NS}> .\n\n{subject} {} .\n",
        predicates.join(" ;\n    ")
    );
    for node in extra_nodes {
        ttl.push_str(&format!("\n{node}\n"));
    }
    ttl
}

/// Render a [`Description`] as a `text/turtle` [`Representation`].
pub fn describe_turtle(description: &Description) -> Representation {
    Representation::new(
        ReprType::new("text/turtle"),
        to_turtle(description).into_bytes(),
    )
}

/// A space binding [`VOCAB_IRI`] (`urn:ikigai:vocab`) to the [`VOCABULARY`] Turtle. Mount it
/// in a kernel's root so the vocabulary is `source`-able as a resource — and, via the http
/// arc, servable at the external `ns#` URL. (Cacheable; lists in the catalog as a plain
/// endpoint.)
pub fn space() -> EndpointSpace {
    EndpointSpace::new().bind(
        Exact::new(VOCAB_IRI),
        FnEndpoint::new("ikigai-vocab", |_inv: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/turtle").with_param("charset", "utf-8"),
                VOCABULARY.as_bytes().to_vec(),
            )
            .cacheable())
        })
        .with_description(
            Description::new("ikigai-vocab")
                .title("ikigai vocabulary")
                .summary(
                    "The ikigai self-description vocabulary (the ns# ontology): the endpoint and \
                     transreptor classes and their properties.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .output("text/turtle;charset=utf-8"),
        ),
    )
}

/// Render a [`Description`] as human-readable plain text (for the CLI `describe`).
pub fn to_text(description: &Description) -> String {
    let mut s = format!("{} — {}\n", description.id, description.title);
    if !description.summary.is_empty() {
        s.push_str(&format!("{}\n", description.summary));
    }
    if !description.verbs.is_empty() {
        let verbs: Vec<&str> = description.verbs.iter().map(|v| verb_name(*v)).collect();
        s.push_str(&format!("verbs: {}\n", verbs.join(", ")));
    }
    for input in &description.inputs {
        let opt = if input.required { "" } else { " (optional)" };
        s.push_str(&format!(
            "  input {} [{}]{}: {}\n",
            input.name,
            source_name(input.source),
            opt,
            input.summary
        ));
    }
    if !description.outputs.is_empty() {
        s.push_str(&format!("outputs: {}\n", description.outputs.join(", ")));
    }
    if let Some(t) = description.transreption() {
        s.push_str(&format!(
            "transrepts: {} → {}\n",
            t.from.join(", "),
            t.to.join(", ")
        ));
    }
    s
}

/// A [`MetaRenderer`] projecting descriptions to `text/turtle` (the default) or
/// `text/plain`. Inject it into a kernel via `Kernel::with_meta_renderer`.
pub struct TurtleRenderer;

impl MetaRenderer for TurtleRenderer {
    fn render(&self, description: &Description, target: &ReprType) -> Result<Representation> {
        match target.media_type.as_str() {
            "text/turtle" | "*/*" | "" => Ok(describe_turtle(description)),
            "text/plain" => Ok(Representation::new(
                ReprType::new("text/plain").with_param("charset", "utf-8"),
                to_text(description).into_bytes(),
            )),
            // The JSON Meta face: the Description via its serde derive. A client
            // engine fetches this to learn an endpoint's declared arguments and
            // route `key=value` — over a socket as much as in-process — so every
            // server that mounts this renderer supports remote argument routing.
            "application/json" => serde_json::to_vec(description)
                .map(|bytes| Representation::new(ReprType::new("application/json"), bytes))
                .map_err(|e| Error::Endpoint(format!("describe as json: {e}"))),
            other => Err(Error::Endpoint(format!(
                "meta renderer does not support target `{other}`"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::{ArgSpec, Space};

    fn sample() -> Description {
        Description::new("toUpper")
            .title("Upper-case")
            .summary("Upper-cases its input.")
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(ArgSpec::new("in").summary("the string"))
            .output("text/plain;charset=utf-8")
    }

    #[test]
    fn renders_the_json_meta_face() {
        use ikigai_core::{ArgSpec, Verb};
        let d = Description::new("wc")
            .verb(Verb::Source)
            .input(ArgSpec::new("count").one_of(["lines", "words"]));
        let repr = TurtleRenderer
            .render(&d, &ReprType::new("application/json"))
            .unwrap();
        assert_eq!(repr.repr_type.media_type, "application/json");
        // round-trips back into the same Description (what a client engine does)
        let back: Description = serde_json::from_slice(&repr.bytes).unwrap();
        assert_eq!(back.id, "wc");
        assert_eq!(back.inputs[0].name, "count");
        assert_eq!(back.inputs[0].one_of, vec!["lines", "words"]);
    }

    #[test]
    fn renders_expected_triples() {
        let ttl = to_turtle(&sample());
        assert!(ttl.starts_with("@prefix ik: <https://ikigai-rs.dev/ns#> ."));
        assert!(ttl.contains("a ik:Endpoint"));
        assert!(ttl.contains("ik:id \"toUpper\""));
        assert!(ttl.contains("ik:verb \"Source\", \"Meta\""));
        assert!(ttl.contains("ik:output \"text/plain;charset=utf-8\""));
        assert!(ttl.contains("ik:inputName \"in\""));
        assert!(ttl.contains("ik:source \"argument\""));
        assert!(ttl.contains("ik:required true"));
        assert!(ttl.trim_end().ends_with('.'));
    }

    #[test]
    fn a_multi_verb_endpoint_projects_per_verb_actions() {
        // The design's worked example: a calendar whose Source (read cap, read args)
        // and Sink (write cap, write args) are DIFFERENT actions.
        use ikigai_core::ActionSpec;
        let d = Description::new("personal-calendar")
            .action(
                ActionSpec::new(Verb::Source)
                    .requires("urn:cap:personal:calendar:read:detail")
                    .output("text/turtle")
                    .input(ArgSpec::new("calendar").optional()),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .requires("urn:cap:personal:calendar:write")
                    .input(
                        ArgSpec::new("start")
                            .class("http://www.w3.org/2001/XMLSchema#dateTime")
                            .summary("event start"),
                    )
                    .input(ArgSpec::new("alert").optional()),
            );
        let ttl = to_turtle(&d);
        // skolemized action nodes, typed and linked
        assert!(ttl.contains("ik:action <urn:ikigai:endpoint:personal-calendar:action:source>"));
        assert!(
            ttl.contains("<urn:ikigai:endpoint:personal-calendar:action:sink> a ik:Action"),
            "{ttl}"
        );
        // capability IRIs are resources, not literals
        assert!(
            ttl.contains("ik:requires <urn:cap:personal:calendar:write>"),
            "{ttl}"
        );
        assert!(
            !ttl.contains("\"urn:cap:"),
            "cap IRIs must not be literals: {ttl}"
        );
        // action-scoped skolemized inputs with datatype
        assert!(ttl.contains(
            "<urn:ikigai:endpoint:personal-calendar:action:sink:input:start> ik:inputName \"start\""
        ));
        assert!(ttl.contains("ik:class <http://www.w3.org/2001/XMLSchema#dateTime>"));
        // the Sink action does not carry the Source's args or cap
        let sink = ttl
            .split("action:sink> ")
            .nth(1)
            .unwrap()
            .split(" .")
            .next()
            .unwrap();
        assert!(
            !sink.contains("calendar:read"),
            "sink action carries only its own cap"
        );
    }

    #[test]
    fn a_single_verb_endpoint_synthesizes_its_action_from_flat_fields() {
        // The 93% case: flat authoring IS the action spec — one ik:Action node,
        // referencing the endpoint-level input nodes (no duplicated bodies).
        let d = Description::new("toUpper")
            .verb(Verb::Source)
            .input(ArgSpec::new("in").summary("the string"))
            .output("text/plain");
        let ttl = to_turtle(&d);
        assert!(ttl.contains("<urn:ikigai:endpoint:toUpper:action:source> a ik:Action"));
        // the synthesized action references the endpoint-level input node
        assert!(
            ttl.contains("ik:input <urn:ikigai:endpoint:toUpper:input:in>"),
            "{ttl}"
        );
        // the input body is stated exactly once
        assert_eq!(ttl.matches("ik:inputName \"in\"").count(), 1, "{ttl}");
        // Meta is never a selectable action
        assert!(!ttl.contains("action:meta"), "{ttl}");
    }

    #[test]
    fn enumerated_inputs_project_default_and_one_of() {
        let d = Description::new("diff").verb(Verb::Source).input(
            ArgSpec::new("mode")
                .one_of(["added", "removed"])
                .default_value("added"),
        );
        let ttl = to_turtle(&d);
        assert!(ttl.contains("ik:default \"added\""), "{ttl}");
        assert!(ttl.contains("ik:oneOf \"added\""), "{ttl}");
        assert!(ttl.contains("ik:oneOf \"removed\""), "{ttl}");
    }

    #[test]
    fn projects_typed_inputs_and_required_capabilities() {
        let d = Description::new("schedule")
            .verb(Verb::Source)
            .requires("cap:net")
            .input(ArgSpec::new("who").class("https://schema.org/Person"))
            .input(ArgSpec::new("content")); // untyped → no ik:class
        let ttl = to_turtle(&d);
        // Endpoint-level required capability scope.
        assert!(ttl.contains("ik:requires \"cap:net\""), "{ttl}");
        // The typed input carries its RDF class as an IRI (resource, not a literal).
        assert!(
            ttl.contains("ik:class <https://schema.org/Person>"),
            "{ttl}"
        );
        // The untyped input has no ik:class.
        assert_eq!(ttl.matches("ik:class").count(), 1, "{ttl}");
        // No `requires` triple when none declared.
        assert!(!to_turtle(&sample()).contains("ik:requires"));
    }

    #[test]
    fn renders_binding_inputs() {
        let d = Description::new("echo").verb(Verb::Source).input(
            ArgSpec::new("message")
                .summary("captured from the path")
                .binding(),
        );
        let ttl = to_turtle(&d);
        assert!(ttl.contains("ik:inputName \"message\""));
        assert!(ttl.contains("ik:source \"binding\""));
        let text = to_text(&d);
        assert!(text.contains("input message [binding]"));
    }

    #[test]
    fn escapes_literals() {
        let d = Description::new("x").title("a \"quote\" and \\ slash");
        let ttl = to_turtle(&d);
        assert!(ttl.contains(r#"ik:title "a \"quote\" and \\ slash""#));
    }

    #[test]
    fn describe_turtle_is_text_turtle() {
        let rep = describe_turtle(&sample());
        assert_eq!(rep.repr_type.media_type, "text/turtle");
    }

    /// A description exercising every IRI position at once: the id, an input name, a
    /// declared class, and a capability scope.
    fn every_iri_position(id: &str, input: &str, class: &str, cap: &str) -> Description {
        Description::new(id)
            .verb(Verb::Source)
            .input(ArgSpec::new(input).class(class))
            .requires(cap)
    }

    #[test]
    fn escaping_the_iri_positions_changes_no_real_endpoint() {
        // The claim the guard rail rests on, asserted rather than assumed: for every id
        // shape actually in the ecosystem — kebab, lowerCamel, and the 27 full-IRI ids —
        // the emitted Turtle is byte-for-byte what raw interpolation produced.
        for id in [
            "camel-case",
            "toUpper",
            "urn:cms:graph",
            "urn:meeting:zoom:schedule",
            "urn:secret",
        ] {
            let ttl = to_turtle(&every_iri_position(
                id,
                "path",
                "http://www.w3.org/2001/XMLSchema#string",
                "urn:cap:fs:read",
            ));
            assert!(
                ttl.contains(&format!("<urn:ikigai:endpoint:{id}> a ik:Endpoint")),
                "{ttl}"
            );
            assert!(
                ttl.contains(&format!("<urn:ikigai:endpoint:{id}:input:path>")),
                "{ttl}"
            );
            assert!(
                ttl.contains(&format!("<urn:ikigai:endpoint:{id}:action:source>")),
                "{ttl}"
            );
            assert!(
                ttl.contains("ik:class <http://www.w3.org/2001/XMLSchema#string>"),
                "{ttl}"
            );
            assert!(ttl.contains("ik:requires <urn:cap:fs:read>"), "{ttl}");
            assert!(
                !ttl.contains('%'),
                "something was encoded that need not be: {ttl}"
            );
        }
    }

    #[test]
    fn a_hostile_identifier_cannot_inject_triples() {
        // `Description::new` validates nothing, so this is constructible today. Before the
        // IRI positions were escaped, the `>` closed the subject IRI and everything after
        // it parsed as Turtle — arbitrary triples in every Meta response.
        let ttl = to_turtle(&every_iri_position(
            "evil> ; a <urn:x> . <urn:y",
            "a b",
            "http://ok> . <urn:z> a <urn:w",
            "urn:cap:x> . <urn:everything",
        ));
        // It still parses (the bound encodes; it does not truncate or emit garbage) …
        let triples: Vec<_> = oxttl::TurtleParser::new()
            .for_reader(ttl.as_bytes())
            .map(|t| t.expect("hostile ids must still emit parseable Turtle"))
            .collect();
        // … and none of the injected subjects made it into the graph.
        for t in &triples {
            let s = t.subject.to_string();
            assert!(
                s.starts_with("<urn:ikigai:endpoint:evil%3E"),
                "injected subject {s}"
            );
            let o = t.object.to_string();
            assert!(
                !matches!(
                    o.as_str(),
                    "<urn:x>" | "<urn:z>" | "<urn:w>" | "<urn:everything>"
                ),
                "injected object {o}"
            );
        }
        // A urn: scope survives encoding as a real IRI …
        assert!(
            ttl.contains("ik:requires <urn:cap:x%3E%20.%20%3Curn:everything>"),
            "{ttl}"
        );
        // … while the mangled http:// class does not (its authority gains a non-numeric
        // "port"), so it degrades to a literal rather than breaking the document.
        assert!(
            ttl.contains(r#"ik:class "http://ok> . <urn:z> a <urn:w""#),
            "{ttl}"
        );
        // The literal face is unaffected: `ik:id` still carries the id verbatim, because
        // literal escaping was never the hole.
        assert!(
            ttl.contains(r#"ik:id "evil> ; a <urn:x> . <urn:y""#),
            "{ttl}"
        );
    }

    #[test]
    fn validate_rejects_what_the_emitter_has_to_encode() {
        // The opt-in predicate and the emitter must agree on the same character set: a
        // description that validates is one whose Turtle needs no encoding at all.
        let ok = every_iri_position(
            "tag-suggest",
            "book",
            "https://schema.org/Person",
            "urn:cap:llm",
        );
        ok.validate().expect("a real endpoint validates");
        assert!(!to_turtle(&ok).contains('%'));

        for bad in [
            every_iri_position("ev>il", "book", "https://schema.org/Person", "urn:cap:llm"),
            every_iri_position("ok", "bo ok", "https://schema.org/Person", "urn:cap:llm"),
            every_iri_position("ok", "book", "https://schema.org/Pe rson", "urn:cap:llm"),
            every_iri_position("ok", "book", "https://schema.org/Person", "urn:cap:l>lm"),
        ] {
            bad.validate().expect_err("must be refused");
            assert!(to_turtle(&bad).contains('%'), "emitter did not encode it");
        }
    }

    /// Parse Turtle and return the triple count, panicking on any syntax error.
    fn parse_count(ttl: &str) -> usize {
        let mut count = 0;
        for triple in oxttl::TurtleParser::new().for_reader(ttl.as_bytes()) {
            triple.expect("valid turtle");
            count += 1;
        }
        count
    }

    #[test]
    fn renders_a_transreptor() {
        let d = Description::new("rdf-transrept")
            .verb(Verb::Source)
            .transreptor(
                ["text/turtle", "application/rdf+xml"],
                ["text/turtle", "text/html"],
            );
        let ttl = to_turtle(&d);
        assert!(ttl.contains("a ik:Endpoint, ik:Transreptor"), "{ttl}");
        assert!(
            ttl.contains("ik:transreptsFrom \"text/turtle\", \"application/rdf+xml\""),
            "{ttl}"
        );
        assert!(
            ttl.contains("ik:transreptsTo \"text/turtle\", \"text/html\""),
            "{ttl}"
        );
        assert!(parse_count(&ttl) > 0, "emitted transreptor turtle parses");
        // A plain endpoint stays just ik:Endpoint.
        let plain = to_turtle(&sample());
        assert!(
            plain.contains("a ik:Endpoint") && !plain.contains("ik:Transreptor"),
            "{plain}"
        );
    }

    #[test]
    fn bundled_vocabulary_is_valid_turtle_with_the_subclass_axiom() {
        assert!(parse_count(VOCABULARY) > 0, "vocabulary.ttl parses");
        assert!(VOCABULARY.contains("ik:Transreptor"));
        assert!(VOCABULARY.contains("rdfs:subClassOf ik:Endpoint"));
        assert!(VOCABULARY.contains("ik:transreptsFrom") && VOCABULARY.contains("ik:transreptsTo"));
    }

    #[test]
    fn vocab_space_binds_the_vocab_iri() {
        let entries = space().entries().expect("space enumerates");
        assert!(
            entries.iter().any(|e| e.pattern == VOCAB_IRI),
            "{entries:?}"
        );
    }

    #[test]
    fn bundled_context_is_valid_jsonld_covering_the_whole_vocab() {
        let v: serde_json::Value = serde_json::from_str(CONTEXT).expect("CONTEXT is valid JSON");
        let ctx = &v["@context"];
        assert_eq!(ctx["ik"], NS);
        // classes map to their ik: term
        assert_eq!(ctx["Route"], "ik:Route");
        assert_eq!(ctx["Endpoint"], "ik:Endpoint");
        // datatype coercions
        assert_eq!(ctx["order"]["@type"], "xsd:integer");
        assert_eq!(ctx["corsCredentials"]["@type"], "xsd:boolean");
        // IRI-valued property coerced to @id (so a string value is an IRI ref)
        assert_eq!(ctx["cors"]["@type"], "@id");
        assert_eq!(ctx["shape"]["@type"], "@id");
        // … including the ones that used to be special-cased in the generator, now
        // carried by a declared rdfs:range: a range change is a context change.
        for iri_valued in ["requires", "input", "endpoint", "class", "resolves", "ref"] {
            assert_eq!(ctx[iri_valued]["@type"], "@id", "{iri_valued}");
        }
        // a literal-valued term stays plain
        assert_eq!(ctx["value"], "ik:value");
        assert_eq!(ctx["binds"], "ik:binds");
        // plain string term
        assert_eq!(ctx["match"], "ik:match");
        // whole-vocab, not just routes — an unrelated term is present
        assert_eq!(ctx["verb"], "ik:verb");
    }

    // Drift guard: the JSON-LD context must map exactly the vocabulary's terms — no
    // more, no fewer. Fails if a term was added/removed in vocabulary.ttl without
    // regenerating context.jsonld (`python3 crates/ikigai-vocab/context.gen.py`).
    #[test]
    fn context_covers_every_vocabulary_term() {
        let vocab_terms: std::collections::BTreeSet<String> = VOCABULARY
            .lines()
            .filter_map(|l| {
                let rest = l.trim_start().strip_prefix("ik:")?;
                let (name, tail) = rest.split_once(' ')?;
                (tail.starts_with("a rdf:Property") || tail.starts_with("a rdfs:Class"))
                    .then(|| name.to_string())
            })
            .collect();

        let ctx: serde_json::Value = serde_json::from_str(CONTEXT).unwrap();
        let context_terms: std::collections::BTreeSet<String> = ctx["@context"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "ik" && *k != "xsd")
            .cloned()
            .collect();

        assert_eq!(
            vocab_terms, context_terms,
            "context.jsonld is out of sync with vocabulary.ttl — regenerate it: \
             `python3 crates/ikigai-vocab/context.gen.py`"
        );
    }

    // The name-set test above cannot see a changed COERCION (a term that moved from a
    // string to an @id when its rdfs:range was declared). `--check` regenerates to a
    // string and compares bytes, so this is the test that keeps a range edit honest.
    #[test]
    fn context_generator_sees_no_drift() {
        let out = std::process::Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/context.gen.py"))
            .arg("--check")
            .output()
            .expect("python3 runs the generator");
        assert!(
            out.status.success(),
            "context.jsonld is stale — regenerate it: `python3 crates/ikigai-vocab/context.gen.py`\n\
             stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The deployed /ns is a copy of VOCABULARY; owl:versionInfo is how one deploy is told
    // apart from the last. It is the crate version, and the release bump must move it.
    #[test]
    fn ontology_version_is_the_crate_version() {
        let declared = VOCABULARY
            .lines()
            .find_map(|l| l.trim().strip_prefix("owl:versionInfo \""))
            .and_then(|rest| rest.split('"').next())
            .expect("the ontology header declares owl:versionInfo");
        assert_eq!(
            declared,
            env!("CARGO_PKG_VERSION"),
            "vocabulary.ttl's owl:versionInfo must be bumped with the crate version"
        );
    }

    // The endpoint-contract properties are emitted on ik:Action and input nodes too; a
    // `rdfs:domain ik:Endpoint` on them made every action an endpoint under RDFS
    // entailment. Keep them domain-free.
    #[test]
    fn shared_contract_properties_declare_no_domain() {
        for term in ["summary", "verb", "output", "input", "requires", "order"] {
            let block = VOCABULARY
                .split("\n\n")
                .find(|b| {
                    b.trim_start()
                        .starts_with(&format!("ik:{term} a rdf:Property"))
                })
                .unwrap_or_else(|| panic!("ik:{term} is declared"));
            assert!(
                !block.contains("rdfs:domain"),
                "ik:{term} is shared across node kinds and must not declare a domain:\n{block}"
            );
        }
    }

    // Every term carries a label and a comment (the /ns page and the catalog UI read
    // them); ik:missingOptional once shipped without a label.
    #[test]
    fn every_term_has_a_label_and_a_comment() {
        for block in VOCABULARY.split("\n\n") {
            let head = block.trim_start();
            if !(head.starts_with("ik:")
                && (head.contains(" a rdf:Property") || head.contains(" a rdfs:Class")))
            {
                continue;
            }
            let name = head.split_whitespace().next().unwrap();
            assert!(block.contains("rdfs:label"), "{name} has no rdfs:label");
            assert!(block.contains("rdfs:comment"), "{name} has no rdfs:comment");
        }
    }

    /// The six terms the conformance wave found: every `ik:` name a shipped module
    /// SERVES in its Turtle face that this vocabulary did not define. Each was pinned
    /// as an exact finding in its own module's conformance test, so this list is the
    /// other half of three pins in three repos — `ikigai-llm`'s
    /// `batch_at_is_the_one_term_the_vocabulary_lacks`, `ikigai-meeting`'s `conforms`,
    /// and `ikigai-browse`'s `the_review_graph_uses_four_terms_the_vocabulary_owes_it`
    /// (whose `browse-review` opt-out exists ONLY because of the four).
    ///
    /// Those pins go red on the first CI run that resolves the version carrying these,
    /// by design: red there means the term arrived. Asserted here so the day someone
    /// deletes one from `vocabulary.ttl`, this crate says which face it breaks rather
    /// than three other repos discovering it separately.
    #[test]
    fn the_terms_three_modules_serve_are_defined() {
        // Asserted the way `ikigai_conformance::rdf::is_defined` asks it — over PARSED
        // subjects under the namespace, not a substring of the file. A term declared in
        // syntax the parser does not accept as a subject reads as present to `contains`
        // and as missing to every module's VOCABULARY check.
        let subjects: std::collections::BTreeSet<String> = oxttl::TurtleParser::new()
            .for_reader(VOCABULARY.as_bytes())
            .map(|t| t.expect("valid turtle").subject.to_string())
            .collect();
        for (term, served_by) in [
            (
                "batchAt",
                "ikigai-llm, on an ik:LlmBackend in urn:llm:models",
            ),
            (
                "passcode",
                "ikigai-meeting, on the ical:Vevent of urn:meeting:zoom:*",
            ),
            ("Review", "ikigai-browse, the review pass's archive entry"),
            (
                "orphanedItems",
                "ikigai-browse, unanchorable findings of a pass",
            ),
            (
                "reviewedBytes",
                "ikigai-browse, prompt-truncation accounting",
            ),
            ("totalBytes", "ikigai-browse, prompt-truncation accounting"),
        ] {
            assert!(
                subjects.contains(&format!("<{NS}{term}>")),
                "ik:{term} is served today by {served_by} — without it that face \
                 emits an undefined term again"
            );
        }
    }

    /// The archive keys and stamps are shared by `ik:Explanation` and `ik:Review`, and
    /// a `rdfs:domain ik:Explanation` on them typed every review entry an explanation
    /// under RDFS entailment — the V-1 mistake (`rdfs:domain ik:Endpoint` on the four
    /// contract properties) repeated one archive over. The review face has emitted
    /// `ik:versionTag` and `ik:derivedAt` on `ik:Review` nodes since it shipped; the
    /// domains came off when `ik:Review` was declared. Keep them off.
    #[test]
    fn archive_properties_shared_with_review_declare_no_domain() {
        for term in [
            "versionTag",
            "derivedAt",
            "contentHash",
            "model",
            "path",
            "repo",
        ] {
            let block = VOCABULARY
                .split("\n\n")
                .find(|b| {
                    b.trim_start()
                        .starts_with(&format!("ik:{term} a rdf:Property"))
                })
                .unwrap_or_else(|| panic!("ik:{term} is declared"));
            // A DECLARATION line, not a mention: these comments say in prose which
            // domain came off, and a substring match on the whole block would read
            // that explanation as the axiom it warns about.
            assert!(
                !block
                    .lines()
                    .any(|l| l.trim_start().starts_with("rdfs:domain")),
                "ik:{term} is stamped on ik:Explanation AND ik:Review entries, so a \
                 domain types one as the other:\n{block}"
            );
        }
    }

    /// The finding family: the six terms a review pass and the pending queue it
    /// writes into need in order to stop borrowing. Asserted here the same way and
    /// for the same reason as [`the_terms_three_modules_serve_are_defined`] — over
    /// PARSED subjects, so a term declared in syntax the parser rejects reads as
    /// missing here rather than as present to `contains`.
    ///
    /// Each replaces a borrowed term or an absence:
    /// `ik:Finding` a bare `prov:Entity`, `ik:severity` a `sh:resultSeverity`,
    /// `ik:outcome` a `dcterms:type`, and the three counts nothing at all — a fate
    /// no triple recorded.
    #[test]
    fn the_finding_family_is_defined() {
        let subjects: std::collections::BTreeSet<String> = oxttl::TurtleParser::new()
            .for_reader(VOCABULARY.as_bytes())
            .map(|t| t.expect("valid turtle").subject.to_string())
            .collect();
        for (term, replaces) in [
            ("Finding", "a pending finding typed only `a prov:Entity`"),
            ("severity", "the borrowed sh:resultSeverity"),
            ("outcome", "a dcterms:type carrying an outcome IRI"),
            (
                "findingCount",
                "a clean pass being an ABSENCE of prov:generated",
            ),
            (
                "droppedItems",
                "a suggestion over budget, discarded uncounted",
            ),
            (
                "suppressedItems",
                "a claim withheld as already declined, uncounted",
            ),
        ] {
            assert!(
                subjects.contains(&format!("<{NS}{term}>")),
                "ik:{term} exists to replace {replaces} — removing it sends that face \
                 back to borrowing a term whose domain types it into another family"
            );
        }
    }

    /// ⚠ The finding family's domains, pinned OFF — the axiom this neighbourhood has
    /// already paid for twice.
    ///
    /// `ik:severity` is the sharpest: three node classes carry it one hop apart — the
    /// `ik:Finding` (the model's proposal), the `prov:Activity` decision node (the
    /// human's final rating), and the `oa:Annotation` a publication mints. A domain
    /// would type two of those as the third and destroy the only signal that can
    /// answer whether a reviewer is calibrated. `ik:outcome`'s subject is an external
    /// class (`prov:Activity`), and this vocabulary declares only its own. The three
    /// counts state "I checked and found n", which any checker claims.
    ///
    /// ★ `ik:orphanedItems` is NOT in this list and keeps its `rdfs:domain ik:Review`
    /// deliberately: it names a mechanism — a quote that did not anchor
    /// character-for-character — that only an anchored-quote LLM pass has. That is
    /// the line between the two halves of this family, and it is the whole test.
    #[test]
    fn the_finding_family_declares_no_domain() {
        for (term, kind, why) in [
            (
                "severity",
                "rdf:Property",
                "a finding, a decision activity and an annotation all carry it",
            ),
            (
                "outcome",
                "rdf:Property",
                "its subject is a prov:Activity, an external class",
            ),
            (
                "findingCount",
                "rdf:Property",
                "any checker can say it found nothing",
            ),
            ("droppedItems", "rdf:Property", "ditto, one fate over"),
            ("suppressedItems", "rdf:Property", "ditto, one fate over"),
            (
                "Finding",
                "rdfs:Class",
                "a class carries no domain; here to keep the family in one list",
            ),
        ] {
            let block = VOCABULARY
                .split("\n\n")
                .find(|b| b.trim_start().starts_with(&format!("ik:{term} a {kind}")))
                .unwrap_or_else(|| panic!("ik:{term} is declared"));
            // A DECLARATION line, not a mention: the comments argue in prose about
            // domains, and a substring match would read the argument as the axiom.
            assert!(
                !block
                    .lines()
                    .any(|l| l.trim_start().starts_with("rdfs:domain")),
                "ik:{term} must declare no domain — {why}:\n{block}"
            );
        }
    }

    /// The coercion is a function of the declared range, so a range the generator has
    /// never seen must still be carried — `xsd:nonNegativeInteger` and
    /// `xsd:positiveInteger` arrived with the review and LLM terms and neither was
    /// enumerated in `context.gen.py`. (`ikigai-browse` emits its counts with the
    /// `^^xsd:nonNegativeInteger` tag already, so the two faces agree term-for-term.)
    #[test]
    fn a_range_the_generator_never_enumerated_still_coerces() {
        let ctx: serde_json::Value = serde_json::from_str(CONTEXT).unwrap();
        let ctx = &ctx["@context"];
        assert_eq!(ctx["batchAt"]["@type"], "xsd:positiveInteger");
        for counted in [
            "orphanedItems",
            "reviewedBytes",
            "totalBytes",
            "findingCount",
            "droppedItems",
            "suppressedItems",
        ] {
            assert_eq!(ctx[counted]["@type"], "xsd:nonNegativeInteger", "{counted}");
        }
        // An IRI-valued property declares `rdfs:range rdfs:Resource` and gets `@id`
        // from that alone. Both of these name a value from a closed set that lives
        // in the serving code, so a range slipped to xsd:string would quietly turn
        // every severity and every outcome into a JSON string in the same edit.
        for iri_valued in ["severity", "outcome"] {
            assert_eq!(ctx[iri_valued]["@type"], "@id", "{iri_valued}");
        }
        // xsd:string stays a plain term, and a class is still just its ik: name.
        assert_eq!(ctx["passcode"], "ik:passcode");
        assert_eq!(ctx["Review"], "ik:Review");
        assert_eq!(ctx["Finding"], "ik:Finding");
    }

    #[test]
    fn bundled_shapes_are_valid_turtle_over_declared_terms() {
        assert!(parse_count(SHAPES) > 0, "shapes.ttl parses");
        // Every ik: term a shape constrains (sh:path / sh:targetClass / sh:class) is a
        // declared vocabulary term — a shape over a term that does not exist would
        // silently constrain nothing.
        let declared: std::collections::BTreeSet<&str> = VOCABULARY
            .lines()
            .filter_map(|l| {
                let rest = l.trim_start().strip_prefix("ik:")?;
                let (name, tail) = rest.split_once(' ')?;
                (tail.starts_with("a rdf:Property") || tail.starts_with("a rdfs:Class"))
                    .then_some(name)
            })
            .collect();
        for line in SHAPES.lines() {
            let line = line.trim();
            for key in ["sh:path ik:", "sh:targetClass ik:", "sh:class ik:"] {
                if let Some(rest) = line.strip_prefix(key) {
                    let term = rest.trim_end_matches([' ', ';', '.']);
                    assert!(
                        declared.contains(term),
                        "shapes.ttl constrains undeclared ik:{term}"
                    );
                }
            }
        }
    }

    // The generator must fail loudly when vocabulary.ttl declares a term twice —
    // a duplicate once last-won silently and flipped a term's mapping to an @id
    // coercion. Runs the real script against a fixture tree with a duplicate.
    #[test]
    fn context_generator_rejects_duplicate_term_declarations() {
        let dir = std::env::temp_dir().join(format!("ikigai-vocab-dupe-{}", std::process::id()));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("fixture tree");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/context.gen.py"),
            dir.join("context.gen.py"),
        )
        .expect("copy generator");
        std::fs::write(
            src.join("vocabulary.ttl"),
            "ik:target a rdf:Property ;\n    rdfs:range rdfs:Resource .\n\n\
             ik:other a rdf:Property .\n\n\
             ik:target a rdf:Property ;\n    rdfs:range xsd:string .\n",
        )
        .expect("write fixture ttl");

        let out = std::process::Command::new("python3")
            .arg(dir.join("context.gen.py"))
            .output()
            .expect("python3 runs the generator");
        let wrote_context = src.join("context.jsonld").exists();
        std::fs::remove_dir_all(&dir).ok();

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "generator must fail on a duplicate declaration; stderr: {stderr}"
        );
        assert!(stderr.contains("ik:target"), "names the term: {stderr}");
        assert!(
            stderr.contains("lines 1 and 6"),
            "names both declaration lines: {stderr}"
        );
        assert!(!wrote_context, "must not write context.jsonld on failure");
    }
}
