use serde::{Deserialize, Serialize};

use crate::verb::Verb;

/// Where an input's value comes from — the two channels an endpoint can read a
/// parameter through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSource {
    /// A by-value argument supplied alongside the request and content-addressed
    /// into its identity (`ArgRef::Inline` / `Content`). The default.
    #[default]
    Argument,
    /// A variable captured from the resource identifier by the resolving grammar
    /// (e.g. a `{var}` in a [`UriTemplate`](crate::UriTemplate)). Its value lives
    /// in the IRI, so it is part of the resource's identity directly.
    Binding,
}

/// A specification of one named input an endpoint accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// The argument name.
    pub name: String,
    /// A short human description.
    pub summary: String,
    /// Whether the argument is required.
    pub required: bool,
    /// Whether the value arrives as a by-value argument or a grammar binding.
    #[serde(default)]
    pub source: InputSource,
    /// The RDF class OR datatype this input's value is expected to be, as an IRI: an
    /// `rdfs:Class` for entity-valued inputs (e.g. `https://schema.org/Person`), an XSD
    /// datatype for scalars (e.g. `xsd:dateTime`). Optional; when present it lets selection
    /// match an endpoint to the *types* available in a context — "what can I do with these
    /// entities?" (`select_action`) — the same way `transreptsFrom`/`To` drives transreptor
    /// selection. Omitted from serialized output when absent, so existing JSON contracts are
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The value assumed when the argument is omitted. Omitted from serialized output when
    /// absent, so existing JSON contracts are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The closed set of accepted values, when the argument is an enumeration (e.g.
    /// `mode=added|removed`). Empty = open-valued. Omitted from serialized output when
    /// empty, so existing JSON contracts are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<String>,
}

impl ArgSpec {
    /// A required by-value argument with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        ArgSpec {
            name: name.into(),
            summary: String::new(),
            required: true,
            source: InputSource::Argument,
            class: None,
            default: None,
            one_of: Vec::new(),
        }
    }

    /// Add a human summary (builder).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Declare the RDF class (IRI) this input's value is expected to be (builder) — e.g.
    /// `https://schema.org/Person`. Drives type-based selection (`select_action`).
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = Some(class.into());
        self
    }

    /// Declare the value assumed when the argument is omitted (builder). Implies the
    /// argument is optional.
    pub fn default_value(mut self, value: impl Into<String>) -> Self {
        self.default = Some(value.into());
        self.required = false;
        self
    }

    /// Declare the closed set of accepted values (builder) — e.g. `["added", "removed"]`
    /// for a `mode=` argument.
    pub fn one_of(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.one_of = values.into_iter().map(Into::into).collect();
        self
    }

    /// Mark the argument optional (builder).
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Declare that this input is captured from the resource identifier by the
    /// resolving grammar rather than passed as a by-value argument (builder).
    pub fn binding(mut self) -> Self {
        self.source = InputSource::Binding;
        self
    }

    /// Every string in this spec that an emitter puts in an IRI position, paired with
    /// the field name to blame. `name` becomes the input node's IRI segment; `class` is
    /// emitted as a resource (`ik:class <…>`).
    fn iri_positions(&self) -> impl Iterator<Item = (&'static str, &str)> {
        std::iter::once(("input name", self.name.as_str()))
            .chain(self.class.as_deref().map(|c| ("input class", c)))
    }
}

/// One verb's contract on an endpoint: the inputs it reads, the outputs it can produce,
/// and the capability scopes invoking it requires. The **action** — an (endpoint, verb)
/// pair — is the unit of selection: a calendar endpoint's `Source` (read, under a read
/// capability) and `Sink` (write, different arguments, write capability) are different
/// actions with different contracts.
///
/// Most endpoints never construct one: a single-verb endpoint's flat
/// [`Description`] fields ARE its action spec, and [`Description::action_specs`]
/// synthesizes the per-verb view. Declare explicit `ActionSpec`s only when verbs
/// genuinely differ.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSpec {
    /// The verb this contract applies to.
    pub verb: Verb,
    /// A short human summary of what this verb does on this endpoint (optional).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// The named inputs this verb reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ArgSpec>,
    /// The representation types this verb can produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    /// The capability scopes invoking this verb requires (IRIs like
    /// `urn:cap:personal:calendar:write`, or legacy descriptive labels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

impl ActionSpec {
    /// An action spec for the given verb.
    pub fn new(verb: Verb) -> Self {
        ActionSpec {
            verb,
            summary: String::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// Set the summary (builder).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Declare an input (builder).
    pub fn input(mut self, input: ArgSpec) -> Self {
        self.inputs.push(input);
        self
    }

    /// Declare an output media type (builder).
    pub fn output(mut self, media_type: impl Into<String>) -> Self {
        self.outputs.push(media_type.into());
        self
    }

    /// Declare a required capability scope (builder).
    pub fn requires(mut self, capability: impl Into<String>) -> Self {
        self.requires.push(capability.into());
        self
    }

    /// Every string in this spec that an emitter puts in an IRI position, paired with
    /// the field name to blame. Shared by [`Description::validate`] so the predicate can
    /// never fall behind what the RDF projection actually emits.
    fn iri_positions(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.inputs
            .iter()
            .flat_map(ArgSpec::iri_positions)
            .chain(self.requires.iter().map(|c| ("requires", c.as_str())))
    }
}

/// A structured, RDF-agnostic self-description of an endpoint.
///
/// `ikigai-core` keeps this free of any RDF dependency; `ikigai-vocab` projects
/// it to RDF (e.g. Turtle) so a `Meta` request can return a machine-readable
/// description of an endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Description {
    /// The endpoint identifier.
    ///
    /// **What is enforced**: nothing at construction — [`Description::new`] takes any
    /// string. The one hard requirement is that the id must be safe in an RDF `IRIREF`
    /// ([`is_iri_safe`](crate::is_iri_safe)), because it is projected into IRI positions
    /// (`urn:ikigai:endpoint:{id}`, and the action and input nodes hung off it). Emitters
    /// percent-encode rather than trust it, and [`validate`](Self::validate) reports it —
    /// call that if you would rather fail than be encoded.
    ///
    /// **What is convention**: a short noun in `kebab-case` (`camel-case`, `tag-suggest`).
    /// Not enforced, and not universal — 27 ids in the ecosystem are full IRIs
    /// (`urn:cms:graph`, `urn:secret`), which nest oddly as
    /// `<urn:ikigai:endpoint:urn:cms:graph>` and are load-bearing anyway: the MCP
    /// projection derives tool names from this field, so an id is a published name.
    pub id: String,
    /// A short human title.
    pub title: String,
    /// A longer human summary.
    pub summary: String,
    /// The verbs this endpoint answers.
    pub verbs: Vec<Verb>,
    /// The named inputs it accepts.
    pub inputs: Vec<ArgSpec>,
    /// The representation types it can produce (canonical media-type strings).
    pub outputs: Vec<String>,
    /// What kind of endpoint this is. Defaults to [`EndpointKind::Endpoint`]; omitted from
    /// serialized output for plain endpoints, so existing JSON contracts are unchanged.
    #[serde(default, skip_serializing_if = "EndpointKind::is_endpoint")]
    pub kind: EndpointKind,
    /// The capability scopes invoking this endpoint requires (descriptive labels, e.g.
    /// `cap:net`, `cap:fs:read`) — what *authority* it demands, not a runtime
    /// [`Capability`](crate::Capability). Lets a host project a capability-scoped catalog
    /// (show an agent only what it may invoke) and lets a caller pre-check feasibility.
    /// Empty for endpoints needing no special authority; omitted from serialized output when
    /// empty, so existing JSON contracts are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Per-verb contracts, for endpoints whose verbs genuinely differ (a calendar's Source
    /// vs Sink). An explicit action WINS over the flat fields for its verb; verbs with no
    /// explicit action get one synthesized from the flat fields
    /// ([`action_specs`](Self::action_specs)). Empty for the common single-verb endpoint;
    /// omitted from serialized output when empty, so existing JSON contracts are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionSpec>,
}

/// What *kind* of endpoint this is — a first-class type that projects to an RDF class
/// (`ik:Endpoint`, `ik:Transreptor`, …) and lets the kernel select endpoints by role.
///
/// The default, [`Endpoint`](EndpointKind::Endpoint), is a plain endpoint (today's
/// behaviour). [`Transreptor`](EndpointKind::Transreptor) marks an endpoint that converts a
/// representation between media types and carries the `from`/`to` types it handles, so a host
/// can find "a transreptor from A to B." More kinds may follow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    /// A plain endpoint (`ik:Endpoint`).
    #[default]
    Endpoint,
    /// A transreptor (`ik:Transreptor ⊏ ik:Endpoint`) — converts a representation from one
    /// media type to another, carrying the conversions it supports.
    Transreptor(Transreption),
}

impl EndpointKind {
    /// Whether this is the default plain [`Endpoint`](EndpointKind::Endpoint) kind.
    pub fn is_endpoint(&self) -> bool {
        matches!(self, EndpointKind::Endpoint)
    }
}

/// The media-type conversions a transreptor supports: it can accept any of `from` and
/// produce any of `to`. Used both to type the endpoint in RDF (`ik:transreptsFrom`/`To`) and
/// to select a transreptor for a needed `from → to` conversion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Transreption {
    /// The media types it can read.
    pub from: Vec<String>,
    /// The media types it can produce.
    pub to: Vec<String>,
}

impl Description {
    /// A description for the endpoint with the given identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Description {
            id: id.into(),
            ..Default::default()
        }
    }

    /// Set the title (builder).
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the summary (builder).
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Declare a supported verb (builder).
    pub fn verb(mut self, verb: Verb) -> Self {
        self.verbs.push(verb);
        self
    }

    /// Declare an input (builder).
    pub fn input(mut self, input: ArgSpec) -> Self {
        self.inputs.push(input);
        self
    }

    /// Declare an output media type (builder).
    pub fn output(mut self, media_type: impl Into<String>) -> Self {
        self.outputs.push(media_type.into());
        self
    }

    /// Declare a capability scope this endpoint requires to be invoked (builder), e.g.
    /// `cap:net`. Callable multiple times; descriptive only (drives capability-scoped
    /// catalog projection and feasibility pre-checks), not runtime enforcement.
    pub fn requires(mut self, capability: impl Into<String>) -> Self {
        self.requires.push(capability.into());
        self
    }

    /// Set the endpoint kind (builder).
    pub fn kind(mut self, kind: EndpointKind) -> Self {
        self.kind = kind;
        self
    }

    /// Declare a per-verb contract (builder). The verb is added to
    /// [`verbs`](Self::verbs) if not already declared.
    pub fn action(mut self, action: ActionSpec) -> Self {
        if !self.verbs.contains(&action.verb) {
            self.verbs.push(action.verb);
        }
        self.actions.push(action);
        self
    }

    /// The normalized per-verb view: one [`ActionSpec`] per declared verb (Meta excluded —
    /// every endpoint answers Meta with this description; it is not a selectable action).
    /// An explicitly declared action wins for its verb; any other verb gets a spec
    /// synthesized from the flat `inputs`/`outputs`/`requires` fields — so the two
    /// authoring forms normalize to the same view, and catalog consumers never know which
    /// form authored an endpoint.
    pub fn action_specs(&self) -> Vec<ActionSpec> {
        self.verbs
            .iter()
            .filter(|v| **v != Verb::Meta)
            .map(|verb| {
                self.actions
                    .iter()
                    .find(|a| a.verb == *verb)
                    .cloned()
                    .unwrap_or_else(|| ActionSpec {
                        verb: *verb,
                        summary: String::new(),
                        inputs: self.inputs.clone(),
                        outputs: self.outputs.clone(),
                        requires: self.requires.clone(),
                    })
            })
            .collect()
    }

    /// Mark this endpoint a **transreptor** that converts representations from any of `from`
    /// to any of `to` (media-type strings) — builder. Sets [`EndpointKind::Transreptor`].
    pub fn transreptor(
        mut self,
        from: impl IntoIterator<Item = impl Into<String>>,
        to: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.kind = EndpointKind::Transreptor(Transreption {
            from: from.into_iter().map(Into::into).collect(),
            to: to.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// The transreption this endpoint supports, if it is a transreptor — for the vocabulary
    /// projection and for transreptor selection.
    pub fn transreption(&self) -> Option<&Transreption> {
        match &self.kind {
            EndpointKind::Transreptor(t) => Some(t),
            EndpointKind::Endpoint => None,
        }
    }

    /// Check that every identifier this description projects into an RDF IRI position is
    /// safe to write there — the [`id`](Self::id), each input `name` and `class`, and each
    /// `requires` scope, across the flat fields and every explicit [`ActionSpec`].
    ///
    /// **Opt-in.** [`Description::new`] validates nothing and never will: it has hundreds
    /// of call sites and a fallible constructor would be a flag day. The RDF projection
    /// percent-encodes ([`escape_iri_fragment`](crate::escape_iri_fragment)) so a bad name
    /// can never break the emitted graph; this is for a host that would rather refuse the
    /// endpoint at bind time than serve it under an encoded name.
    ///
    /// ```
    /// use ikigai_core::{ArgSpec, Description};
    ///
    /// assert!(Description::new("tag-suggest").input(ArgSpec::new("book")).validate().is_ok());
    ///
    /// // An id that would close the IRI early and inject triples into every Meta response.
    /// let err = Description::new("evil> ; a <urn:x> . <urn:y").validate().unwrap_err();
    /// assert!(err.to_string().contains("id"), "{err}");
    ///
    /// // Nested fields are checked too — an input name is an IRI segment as well.
    /// assert!(Description::new("ok").input(ArgSpec::new("a b")).validate().is_err());
    /// ```
    pub fn validate(&self) -> crate::Result<()> {
        let flat = self
            .inputs
            .iter()
            .flat_map(ArgSpec::iri_positions)
            .chain(self.requires.iter().map(|c| ("requires", c.as_str())));
        let positions = std::iter::once(("id", self.id.as_str()))
            .chain(flat)
            .chain(self.actions.iter().flat_map(ActionSpec::iri_positions));
        for (field, value) in positions {
            if !crate::is_iri_safe(value) {
                return Err(crate::Error::Endpoint(format!(
                    "endpoint `{}`: {field} `{value}` is not safe in an IRI position \
                     (an RDF IRIREF cannot contain <>\"{{}}|^`\\, space or a control character)",
                    self.id
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_specs_normalize_both_authoring_forms() {
        // flat single-verb: the flat fields ARE the (synthesized) action
        let flat = Description::new("toUpper")
            .verb(Verb::Source)
            .verb(Verb::Meta) // Meta is universal, never a selectable action
            .input(ArgSpec::new("in"))
            .output("text/plain")
            .requires("cap:demo");
        let specs = flat.action_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].verb, Verb::Source);
        assert_eq!(specs[0].inputs[0].name, "in");
        assert_eq!(specs[0].requires, vec!["cap:demo".to_string()]);

        // explicit action WINS for its verb; other declared verbs synthesize
        let mixed = Description::new("calendar")
            .verb(Verb::Source)
            .input(ArgSpec::new("calendar").optional()) // flat = Source's shape
            .action(
                ActionSpec::new(Verb::Sink)
                    .requires("urn:cap:personal:calendar:write")
                    .input(ArgSpec::new("start")),
            );
        assert!(
            mixed.verbs.contains(&Verb::Sink),
            ".action() declares its verb"
        );
        let specs = mixed.action_specs();
        assert_eq!(specs.len(), 2);
        let sink = specs.iter().find(|a| a.verb == Verb::Sink).unwrap();
        assert_eq!(sink.inputs[0].name, "start", "explicit action wins");
        let source = specs.iter().find(|a| a.verb == Verb::Source).unwrap();
        assert_eq!(source.inputs[0].name, "calendar", "synthesized from flat");
    }

    #[test]
    fn builds_a_description() {
        let d = Description::new("toUpper")
            .title("Upper-case")
            .verb(Verb::Source)
            .input(ArgSpec::new("in").summary("the string").optional())
            .output("text/plain");
        assert_eq!(d.id, "toUpper");
        assert_eq!(d.verbs, vec![Verb::Source]);
        assert_eq!(d.inputs[0].name, "in");
        assert!(!d.inputs[0].required);
    }

    #[test]
    fn inputs_default_to_arguments_and_can_be_bindings() {
        let arg = ArgSpec::new("in");
        assert_eq!(arg.source, InputSource::Argument);
        let bound = ArgSpec::new("message").binding();
        assert_eq!(bound.source, InputSource::Binding);
    }

    #[test]
    fn serde_round_trip() {
        let d = Description::new("x")
            .verb(Verb::Meta)
            .input(ArgSpec::new("message").binding());
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Description>(&json).unwrap(), d);
    }

    #[test]
    fn kind_defaults_to_endpoint_and_is_omitted_from_json() {
        let d = Description::new("toUpper").output("text/plain");
        assert_eq!(d.kind, EndpointKind::Endpoint);
        assert!(d.transreption().is_none());
        // A plain endpoint's JSON carries no `kind` key — the engine's contract is unchanged.
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("kind"), "{json}");
        // …and JSON without a `kind` still deserializes (back-compat).
        let d2: Description = serde_json::from_str(&json).unwrap();
        assert_eq!(d2.kind, EndpointKind::Endpoint);
    }

    #[test]
    fn transreptor_builder_records_its_conversions() {
        let d = Description::new("rdf-transrept")
            .verb(Verb::Source)
            .transreptor(
                ["text/turtle", "application/rdf+xml"],
                ["text/turtle", "text/html"],
            );
        let t = d.transreption().expect("is a transreptor");
        assert_eq!(t.from, vec!["text/turtle", "application/rdf+xml"]);
        assert_eq!(t.to, vec!["text/turtle", "text/html"]);
        assert!(!d.kind.is_endpoint());
        // The transreptor kind round-trips through serde.
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("transreptor"), "{json}");
        assert_eq!(serde_json::from_str::<Description>(&json).unwrap(), d);
    }

    #[test]
    fn input_class_is_optional_and_omitted_when_absent() {
        // No class → no `class` key in the input's JSON (existing contract unchanged).
        let plain = ArgSpec::new("content");
        assert_eq!(plain.class, None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("class"), "{json}");

        // With a class → recorded and round-trips.
        let typed = ArgSpec::new("who").class("https://schema.org/Person");
        assert_eq!(typed.class.as_deref(), Some("https://schema.org/Person"));
        assert_eq!(
            serde_json::from_str::<ArgSpec>(&serde_json::to_string(&typed).unwrap()).unwrap(),
            typed
        );
    }

    #[test]
    fn requires_is_empty_by_default_and_omitted_from_json() {
        let d = Description::new("toUpper").output("text/plain");
        assert!(d.requires.is_empty());
        // A no-authority endpoint carries no `requires` key — JSON contract unchanged.
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("requires"), "{json}");

        // Declared capability scopes accumulate and round-trip.
        let gated = Description::new("httpGet")
            .verb(Verb::Source)
            .requires("cap:net")
            .requires("cap:fs:read");
        assert_eq!(gated.requires, vec!["cap:net", "cap:fs:read"]);
        assert_eq!(
            serde_json::from_str::<Description>(&serde_json::to_string(&gated).unwrap()).unwrap(),
            gated
        );
    }

    #[test]
    fn a_plain_descriptions_json_is_byte_identical_to_before_these_fields() {
        // Belt-and-suspenders: an endpoint using neither new field serializes with exactly
        // the pre-existing keys — so the engine's machine contract is untouched.
        let d = Description::new("toUpper")
            .title("Upper-case")
            .verb(Verb::Source)
            .input(ArgSpec::new("in"))
            .output("text/plain");
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("class") && !json.contains("requires"),
            "{json}"
        );
    }

    #[test]
    fn validate_reaches_every_field_an_emitter_puts_in_an_iri_position() {
        // The predicate must cover the NESTED positions too — an explicit ActionSpec's
        // inputs get their own IRI nodes, and a spec that only checked the flat fields
        // would pass a description whose Turtle still needed encoding.
        let base = || {
            Description::new("cal").action(
                ActionSpec::new(Verb::Sink)
                    .requires("urn:cap:cal:write")
                    .input(
                        ArgSpec::new("start").class("http://www.w3.org/2001/XMLSchema#dateTime"),
                    ),
            )
        };
        base().validate().expect("a real endpoint validates");

        let cases: [(&str, Description); 6] = [
            ("id", Description::new("ca>l")),
            (
                "input name",
                Description::new("cal").input(ArgSpec::new("a b")),
            ),
            (
                "input class",
                Description::new("cal").input(ArgSpec::new("a").class("urn:x>y")),
            ),
            ("requires", Description::new("cal").requires("urn:cap:a b")),
            (
                "input name",
                Description::new("cal")
                    .action(ActionSpec::new(Verb::Sink).input(ArgSpec::new("a>b"))),
            ),
            (
                "requires",
                Description::new("cal").action(ActionSpec::new(Verb::Sink).requires("urn:cap:a>b")),
            ),
        ];
        for (field, d) in cases {
            let err = d.validate().expect_err("must be refused").to_string();
            assert!(
                err.contains(field),
                "expected `{field}` to be blamed: {err}"
            );
        }
    }
}
