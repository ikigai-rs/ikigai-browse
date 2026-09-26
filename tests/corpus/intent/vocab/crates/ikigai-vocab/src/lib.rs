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
/// (`ik:Endpoint`, `ik:Transreptor ⊏ ik:Endpoint`, …) and properties. Served by
/// [`space`] at `urn:ikigai:vocab`, and eventually at the external `ns#` URL.
pub const VOCABULARY: &str = include_str!("vocabulary.ttl");

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
        format!("<{scope}>")
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
    ik:class <{class}>"
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
pub fn to_turtle(description: &Description) -> String {
    // `id` is a resource identifier (no Turtle-significant characters).
    let subject = format!("<urn:ikigai:endpoint:{}>", description.id);

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
    let endpoint_iri = format!("urn:ikigai:endpoint:{}", description.id);
    let mut extra_nodes: Vec<String> = Vec::new();
    for input in &description.inputs {
        let node_iri = format!("{endpoint_iri}:input:{}", input.name);
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
            let node_iri = if synthesized {
                // the flat input node already emitted above — reference it
                format!("{endpoint_iri}:input:{}", input.name)
            } else {
                let iri = format!("{action_iri}:input:{}", input.name);
                extra_nodes.push(format!("<{iri}> {} .", input_predicates(input)));
                iri
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
