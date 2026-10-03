use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::iri::Iri;
use crate::topology::MatchKind;

/// Variables captured when a grammar matches an identifier.
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bindings(BTreeMap<String, String>);

impl Bindings {
    /// An empty binding set.
    pub fn new() -> Self {
        Bindings(BTreeMap::new())
    }

    /// Look up a captured variable.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Insert a captured variable.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    /// Whether no variables were captured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over the captured `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// A grammar decides whether an identifier belongs to an endpoint and, if so,
/// extracts variable bindings from it. Resolution matches a request's target
/// against the grammars in scope.
pub trait Grammar: Send + Sync {
    /// Return captured bindings if `iri` matches this grammar, else `None`.
    fn match_iri(&self, iri: &Iri) -> Option<Bindings>;

    /// A human-readable pattern for this grammar, used when enumerating a space
    /// (e.g. the exact IRI, or a template like `urn:demo:echo/{message}`). The
    /// default is uninformative; concrete grammars override it.
    fn pattern(&self) -> String {
        "<opaque>".to_string()
    }

    /// How [`pattern`](Self::pattern) matches, for `urn:kernel:topology`'s
    /// `ik:matchKind`. The default is [`MatchKind::Custom`]: a grammar written
    /// outside core is described by its pattern and not defined by it (it may add
    /// a binding the pattern does not show, or match by rules no pattern states),
    /// so the graph must not claim it can be rebuilt from the text. Core's
    /// [`Exact`] and [`UriTemplate`] answer their own kinds; override it only for a
    /// grammar that matches exactly as one of those would.
    fn match_kind(&self) -> MatchKind {
        MatchKind::Custom
    }
}

/// Matches one exact identifier and captures no variables.
pub struct Exact(String);

impl Exact {
    /// A grammar matching exactly `iri`.
    pub fn new(iri: impl Into<String>) -> Self {
        Exact(iri.into())
    }
}

impl Grammar for Exact {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        (iri.as_str() == self.0).then(Bindings::new)
    }

    fn pattern(&self) -> String {
        self.0.clone()
    }

    fn match_kind(&self) -> MatchKind {
        MatchKind::Exact
    }
}

/// A constrained RFC 6570 URI template — Level 1 `{var}` expansion only.
///
/// Matching is deterministic: literal text matches verbatim, each `{var}`
/// captures the run up to the next literal (leftmost occurrence), and a final
/// `{var}` captures the remainder. Captures must be non-empty, and adjacent
/// variables with no separating literal are rejected at construction as
/// ambiguous. (Operators like `{+var}`, `{?q}`, `{/p}` are intentionally out of
/// scope for now; `Grammar` lets richer grammars slot in beside this.)
pub struct UriTemplate {
    parts: Vec<Part>,
    source: String,
}

enum Part {
    Lit(String),
    Var(String),
}

impl UriTemplate {
    /// Parse a template, rejecting malformed or ambiguous forms.
    pub fn parse(template: impl Into<String>) -> Result<Self, TemplateError> {
        let source = template.into();
        let mut parts = Vec::new();
        let mut rest = source.as_str();
        let mut offset = 0;
        while let Some(rel) = rest.find('{') {
            let open = offset + rel;
            if rel > 0 {
                parts.push(Part::Lit(rest[..rel].to_string()));
            }
            let close_rel = rest[rel..]
                .find('}')
                .ok_or_else(|| TemplateError(format!("unclosed '{{' in `{source}`")))?;
            let name = &rest[rel + 1..rel + close_rel];
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(TemplateError(format!(
                    "invalid variable `{{{name}}}` in `{source}`"
                )));
            }
            parts.push(Part::Var(name.to_string()));
            offset = open + close_rel + 1;
            rest = &source[offset..];
        }
        if !rest.is_empty() {
            parts.push(Part::Lit(rest.to_string()));
        }
        for window in parts.windows(2) {
            if matches!(window[0], Part::Var(_)) && matches!(window[1], Part::Var(_)) {
                return Err(TemplateError(format!(
                    "adjacent variables are ambiguous in `{source}`"
                )));
            }
        }
        Ok(UriTemplate { parts, source })
    }

    /// The template's source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The template's variable names, in order of appearance — the binding
    /// arguments a caller must supply (via [`expand`](Self::expand)) to form a
    /// concrete IRI from this pattern.
    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Var(name) => Some(name.as_str()),
            Part::Lit(_) => None,
        })
    }

    /// Expand the template with the given bindings, or `None` if a variable is
    /// missing.
    pub fn expand(&self, bindings: &Bindings) -> Option<String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(lit) => out.push_str(lit),
                Part::Var(var) => out.push_str(bindings.get(var)?),
            }
        }
        Some(out)
    }

    fn match_str(&self, input: &str) -> Option<Bindings> {
        let mut bindings = Bindings::new();
        let mut pos = 0;
        let mut i = 0;
        while i < self.parts.len() {
            match &self.parts[i] {
                Part::Lit(lit) => {
                    if input[pos..].starts_with(lit.as_str()) {
                        pos += lit.len();
                    } else {
                        return None;
                    }
                }
                Part::Var(name) => match self.parts.get(i + 1) {
                    Some(Part::Lit(next)) => {
                        let idx = input[pos..].find(next.as_str())?;
                        if idx == 0 {
                            return None; // empty capture
                        }
                        bindings.insert(name.clone(), input[pos..pos + idx].to_string());
                        pos += idx;
                    }
                    _ => {
                        if pos == input.len() {
                            return None; // empty capture
                        }
                        bindings.insert(name.clone(), input[pos..].to_string());
                        pos = input.len();
                    }
                },
            }
            i += 1;
        }
        (pos == input.len()).then_some(bindings)
    }
}

impl Grammar for UriTemplate {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        self.match_str(iri.as_str())
    }

    fn pattern(&self) -> String {
        self.source.clone()
    }

    fn match_kind(&self) -> MatchKind {
        MatchKind::Template
    }
}

/// Error parsing a [`UriTemplate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(String);

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid URI template: {}", self.0)
    }
}

impl std::error::Error for TemplateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn iri(s: &str) -> Iri {
        Iri::parse(s).unwrap()
    }

    #[test]
    fn exact_matches_only_itself() {
        let g = Exact::new("urn:test:to-upper");
        assert!(g.match_iri(&iri("urn:test:to-upper")).is_some());
        assert!(g.match_iri(&iri("urn:test:to-lower")).is_none());
    }

    #[test]
    fn template_captures_trailing_var() {
        let t = UriTemplate::parse("urn:test:echo/{message}").unwrap();
        let b = t.match_iri(&iri("urn:test:echo/hello")).unwrap();
        assert_eq!(b.get("message"), Some("hello"));
        assert!(t.match_iri(&iri("urn:test:echo/")).is_none()); // empty capture
        assert!(t.match_iri(&iri("urn:other:echo/hi")).is_none());
    }

    #[test]
    fn template_captures_middle_var() {
        let t = UriTemplate::parse("urn:r:{id}/data").unwrap();
        let b = t.match_iri(&iri("urn:r:42/data")).unwrap();
        assert_eq!(b.get("id"), Some("42"));
        assert!(t.match_iri(&iri("urn:r:42/other")).is_none());
    }

    #[test]
    fn expand_is_inverse_of_match() {
        let t = UriTemplate::parse("urn:r:{id}/data").unwrap();
        let b = t.match_iri(&iri("urn:r:7/data")).unwrap();
        assert_eq!(t.expand(&b).as_deref(), Some("urn:r:7/data"));
    }

    #[test]
    fn pattern_reflects_the_grammar() {
        assert_eq!(
            Exact::new("urn:test:to-upper").pattern(),
            "urn:test:to-upper"
        );
        assert_eq!(
            UriTemplate::parse("urn:demo:echo/{message}")
                .unwrap()
                .pattern(),
            "urn:demo:echo/{message}"
        );
    }

    #[test]
    fn rejects_ambiguous_and_malformed() {
        assert!(UriTemplate::parse("urn:{a}{b}").is_err()); // adjacent vars
        assert!(UriTemplate::parse("urn:{a").is_err()); // unclosed
        assert!(UriTemplate::parse("urn:{}").is_err()); // empty name
    }
}
