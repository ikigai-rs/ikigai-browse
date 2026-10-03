//! Passkey identity on the HTTP door — the same thing a client certificate is on the QUIC
//! door, arriving by a different route.
//!
//! # ★ One grant table for both doors
//!
//! A QUIC certificate fingerprint and a passkey credential are both **identities**, and
//! both map to a grant NAME in `gonk/clients.json`; the grant name maps to scopes in
//! `gonk/grants.json`, which is the only place a scope list lives:
//!
//! ```text
//! gonk/clients.json   { "clients":  { "<cert sha256>":     { "grant": "brian" } },
//!                       "passkeys": { "<credential id>":   { "grant": "brian", "label": "brian",
//!                                                             "public_key": "<SPKI, base64url>",
//!                                                             "sign_count": 0 } } }
//! gonk/grants.json    { "brian": ["urn:cap:ledger:read:default", …] }
//! gonk/invites.json   { "<sha256 of an invite code>": { "grant": "brian", "expires": 1757… } }
//! ```
//!
//! So a person with a laptop certificate and a passkey who are enrolled under one grant
//! cannot hold read over QUIC and write over HTTP by accident: there is one list to be
//! wrong in. Both files are re-read on every use — every QUIC connection, every HTTP request
//! carrying a session — so editing a grant or deleting an enrolment takes effect on the next
//! request, with no restart.
//!
//! # Registration: the INVITE is the trust anchor
//!
//! `ikigai-gonk passkey invite <name> --ledger …` writes the grant and a single-use,
//! expiring invite, and prints `http://localhost:<port>/#invite=<code>`. The browser that
//! opens it creates a credential and posts its public key with the code. Nothing here parses
//! an attestation — `ikigai-passkey` is an assertion verifier and holds no CBOR decoder —
//! because attestation answers "what kind of authenticator is this", and the question this
//! server needs answered is "was this person invited", which the code answers. Whoever holds
//! the invite IS the enrollee; that is what makes an invite a secret worth expiring.
//!
//! # Login: `ikigai_passkey::verify`, then a session
//!
//! A login answers a single-use challenge; the assertion is verified against the stored
//! key for the exact origin `http://localhost:<port>` and RP ID `localhost`, with user
//! verification REQUIRED (an identity that grants authority is a biometric or a PIN, not a
//! tap). Success mints a 256-bit session token, kept only as its SHA-256, for
//! [`SESSION_SECONDS`]. Sessions live in memory: a restart signs everyone out, which is the
//! safe direction to be wrong in.
//!
//! ⚠ **The cookie is set by the page's script, not by the server, so it is not HttpOnly.**
//! `ikigai-web` builds every response from a kernel representation and has no way to add a
//! `Set-Cookie` header. The mitigations are the ones that remain: `SameSite=Strict`, a
//! strict same-origin CSP with no inline script, and a token that dies with the process.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ikigai_passkey::{Assertion, Policy, RegisteredCredential};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::grants::broad_store_scopes;
use crate::quic::{self, Layout};

/// The relying-party ID. ⚠ An IP address is not a valid RP ID, so the page must be opened
/// as `http://localhost:<port>` — never `http://127.0.0.1:<port>` — or the browser refuses
/// to create or use a credential at all.
pub const RP_ID: &str = "localhost";

/// The cookie the page stores its session token in.
pub const SESSION_COOKIE: &str = "gonk_session";

/// How long a session lasts.
pub const SESSION_SECONDS: u64 = 12 * 60 * 60;

/// How long a challenge may wait for its answer.
pub const CHALLENGE_SECONDS: u64 = 5 * 60;

/// How long an invite is valid by default.
pub const INVITE_MINUTES: u64 = 30;

/// The most challenges or sessions held at once. ★ A bound REFUSES past it rather than
/// evicting: an evicted challenge would fail someone's login mid-ceremony with an error
/// that names nothing, and anonymous callers can mint challenges.
pub const MAX_PENDING: usize = 256;

/// The IRI prefix a signed-in passkey is NAMED by when the door stamps a write — the
/// `principal` the HTTP transport attaches (see [`crate::doors::http_principal`]), and the
/// `author` the form adapter forwards to the ledger.
///
/// ★ The tail is the CREDENTIAL ID, not the label. A label is a display name an operator
/// may change (`"Touch ID"`), and two passkeys can share one; the credential id is what the
/// authenticator minted and what `clients.json` is keyed by, so a relabelled passkey keeps
/// its history and two passkeys named alike stay distinct. The IRI is what the store holds;
/// only the face translates it back to a label ([`Passkeys::labels`]).
pub const PASSKEY_IRI_PREFIX: &str = "urn:iki:gonk:passkey:";

/// The stable IRI of the passkey enrolled under `credential_id` (base64url, so the tail
/// needs no escaping).
pub fn passkey_iri(credential_id: &str) -> String {
    format!("{PASSKEY_IRI_PREFIX}{credential_id}")
}

/// The credential id a [`passkey_iri`] names, or `None` for any other text — a plain-name
/// author written by hand, or by the cli.
pub fn passkey_credential(author: &str) -> Option<&str> {
    author.strip_prefix(PASSKEY_IRI_PREFIX)
}

/// One enrolled passkey, as `clients.json` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// base64url of the credential id.
    pub credential_id: String,
    /// The grant name — a key of `grants.json`.
    pub grant: String,
    /// A human label.
    pub label: String,
    /// base64url of the SPKI DER public key.
    pub public_key: String,
    /// The highest signature counter seen.
    pub sign_count: u32,
}

/// Every enrolled passkey, by credential id. Absent file or key: none.
pub fn read_passkeys(layout: &Layout) -> Result<BTreeMap<String, Enrolled>, String> {
    let doc = quic::read_object(&layout.clients_json())?;
    let mut out = BTreeMap::new();
    let Some(map) = doc.get("passkeys") else {
        return Ok(out);
    };
    let map = map
        .as_object()
        .ok_or("`passkeys` in clients.json must be an object of credential id → entry")?;
    for (id, entry) in map {
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("passkey `{id}` has no `{name}`"))
        };
        out.insert(
            id.clone(),
            Enrolled {
                credential_id: id.clone(),
                grant: field("grant")?,
                label: field("label").unwrap_or_else(|_| field("grant").unwrap_or_default()),
                public_key: field("public_key")?,
                sign_count: entry
                    .get("sign_count")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
            },
        );
    }
    Ok(out)
}

/// Write one passkey entry into `clients.json`, keeping every other key (the QUIC clients,
/// the default) exactly as it was.
fn write_passkey(layout: &Layout, entry: &Enrolled) -> Result<(), String> {
    let mut doc = quic::read_object(&layout.clients_json())?;
    let mut passkeys = match doc.remove("passkeys") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`passkeys` in clients.json must be an object".to_string()),
    };
    passkeys.insert(
        entry.credential_id.clone(),
        json!({
            "grant": entry.grant,
            "label": entry.label,
            "public_key": entry.public_key,
            "sign_count": entry.sign_count,
        }),
    );
    doc.insert("passkeys".to_string(), Value::Object(passkeys));
    let text = quic::pretty(&Value::Object(doc))?;
    // What is written must still be what the QUIC door reads.
    quic::parse_enrolment(&text)?;
    quic::write_private(&layout.clients_json(), &text)
}

/// Create an invite for `grant`, writing the grant's scopes into `grants.json` first. Returns
/// the one-time code — the only copy; the file keeps its SHA-256.
///
/// An existing grant with DIFFERENT scopes is refused unless `force`, exactly as
/// `ikigai-gonk client add` refuses — the same grant name may be shared by a certificate and
/// a passkey, and that sharing is the point, so silently replacing it would change the other
/// identity's authority too.
pub fn invite(
    layout: &Layout,
    grant: &str,
    scopes: &[String],
    force: bool,
    minutes: u64,
    now: u64,
) -> Result<String, String> {
    quic::put_grant(layout, grant, scopes, force)?;
    let code = random_b64(24)?;
    let mut doc = quic::read_object(&layout.invites_json())?;
    // Expired invites are dropped whenever the file is written, so it does not grow.
    doc.retain(|_, entry| entry.get("expires").and_then(Value::as_u64).unwrap_or(0) > now);
    doc.insert(
        sha256_hex(code.as_bytes()),
        json!({ "grant": grant, "expires": now + minutes * 60 }),
    );
    quic::write_private(&layout.invites_json(), &quic::pretty(&Value::Object(doc))?)?;
    Ok(code)
}

/// Consume an invite: its grant, if the code is live. Single use — removed on success.
fn redeem(layout: &Layout, code: &str, now: u64) -> Result<String, String> {
    let mut doc = quic::read_object(&layout.invites_json())?;
    let key = sha256_hex(code.as_bytes());
    let entry = doc
        .remove(&key)
        .ok_or("this invite is not valid — it was never issued, or it has been used")?;
    let expires = entry.get("expires").and_then(Value::as_u64).unwrap_or(0);
    let grant = entry
        .get("grant")
        .and_then(Value::as_str)
        .ok_or("the invite names no grant")?
        .to_string();
    quic::write_private(&layout.invites_json(), &quic::pretty(&Value::Object(doc))?)?;
    if expires <= now {
        return Err(
            "this invite has expired — ask for a new one (`ikigai-gonk passkey invite`)"
                .to_string(),
        );
    }
    Ok(grant)
}

/// The scopes a grant name holds, fail-closed: unknown, empty, or naming a broad store
/// token is an error. The same rule the QUIC door applies.
pub fn scopes_of(layout: &Layout, grant: &str) -> Result<Vec<String>, String> {
    let grants = quic::read_grants(&layout.grants_json())?;
    quic::scopes_for_grant(&grants, grant)
}

/// What a challenge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `navigator.credentials.create`.
    Register,
    /// `navigator.credentials.get`.
    Login,
}

struct Pending {
    purpose: Purpose,
    expires: u64,
}

struct Session {
    credential_id: String,
    expires: u64,
}

#[derive(Default)]
struct State {
    challenges: HashMap<String, Pending>,
    /// Keyed by the SHA-256 of the token — the token itself is never held.
    sessions: HashMap<String, Session>,
}

/// The HTTP door's passkey relying party and session table.
pub struct Passkeys {
    layout: Layout,
    origin: String,
    state: Mutex<State>,
}

/// A signed-in identity, as a page or the capability function sees it.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The enrolment.
    pub enrolled: Enrolled,
    /// The grant's scopes, resolved now.
    pub scopes: Vec<String>,
    /// When the session ends, in seconds since the epoch.
    pub expires: u64,
}

impl Passkeys {
    /// A relying party for the page served at `http://localhost:{port}`.
    pub fn new(layout: Layout, port: u16) -> Passkeys {
        Passkeys {
            layout,
            origin: format!("http://localhost:{port}"),
            state: Mutex::new(State::default()),
        }
    }

    /// The only origin a ceremony is accepted from.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// How many passkeys are enrolled (for the banner). An unreadable file counts as none
    /// here; every use that matters re-reads and fails loud.
    pub fn enrolled_count(&self) -> usize {
        read_passkeys(&self.layout).map(|m| m.len()).unwrap_or(0)
    }

    /// Every enrolled passkey's CURRENT label, by credential id — what a page renders a
    /// [`passkey_iri`] author as. Re-read from `clients.json` on every call, like every
    /// other use of that file, so a relabel shows on the next request; an unreadable file
    /// is an empty map here, and a caller falls back to the IRI's tail.
    pub fn labels(&self) -> BTreeMap<String, String> {
        read_passkeys(&self.layout)
            .map(|m| m.into_iter().map(|(id, e)| (id, e.label)).collect())
            .unwrap_or_default()
    }

    /// A fresh single-use challenge, base64url.
    pub fn challenge(&self, purpose: Purpose, now: u64) -> Result<String, String> {
        let challenge = random_b64(32)?;
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        state.challenges.retain(|_, p| p.expires > now);
        if state.challenges.len() >= MAX_PENDING {
            return Err(format!(
                "{MAX_PENDING} passkey ceremonies are already waiting for an answer; try again \
                 in a few minutes"
            ));
        }
        state.challenges.insert(
            challenge.clone(),
            Pending {
                purpose,
                expires: now + CHALLENGE_SECONDS,
            },
        );
        Ok(challenge)
    }

    fn take_challenge(&self, challenge: &str, purpose: Purpose, now: u64) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        match state.challenges.remove(challenge) {
            Some(p) if p.purpose == purpose && p.expires > now => Ok(()),
            Some(_) => {
                Err("that challenge has expired or was issued for another ceremony".to_string())
            }
            None => {
                Err("that challenge was not issued here, or has already been answered".to_string())
            }
        }
    }

    /// Finish a registration. The body is the page's JSON:
    /// `{challenge, invite, id, publicKey, clientDataJSON, label?}` (byte fields base64url).
    pub fn register(&self, body: &[u8], now: u64) -> Result<Enrolled, String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let challenge = text("challenge")?;
        self.take_challenge(&challenge, Purpose::Register, now)?;
        let client_data = b64(&text("clientDataJSON")?, "clientDataJSON")?;
        self.check_client_data(&client_data, "webauthn.create", &challenge)?;
        let id = text("id")?;
        let key = b64(&text("publicKey")?, "publicKey")?;
        // Parsing is the check that this is an ES256 key this server can verify against.
        RegisteredCredential::from_spki_der(b64(&id, "id")?, &key, 0)
            .map_err(|e| format!("{e} — this server verifies ES256 (P-256) passkeys only"))?;
        if read_passkeys(&self.layout)?.contains_key(&id) {
            return Err("this credential is already enrolled".to_string());
        }
        // The invite is redeemed LAST, after everything that could refuse the credential:
        // a malformed key must not burn a single-use code.
        let grant = redeem(&self.layout, &text("invite")?, now)?;
        scopes_of(&self.layout, &grant)?;
        let label = v
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| grant.clone());
        let entry = Enrolled {
            credential_id: id,
            grant,
            label,
            public_key: URL_SAFE_NO_PAD.encode(&key),
            sign_count: 0,
        };
        write_passkey(&self.layout, &entry)?;
        Ok(entry)
    }

    /// Finish a login. The body: `{challenge, id, authenticatorData, clientDataJSON,
    /// signature}`. Returns the session token (the only copy) and the identity.
    pub fn login(&self, body: &[u8], now: u64) -> Result<(String, Identity), String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let challenge = text("challenge")?;
        self.take_challenge(&challenge, Purpose::Login, now)?;
        let id = text("id")?;
        let mut enrolled = read_passkeys(&self.layout)?
            .remove(&id)
            .ok_or("this passkey is not enrolled on this server")?;
        let credential = RegisteredCredential::from_spki_der(
            b64(&id, "id")?,
            &b64(&enrolled.public_key, "public_key")?,
            enrolled.sign_count,
        )
        .map_err(|e| e.to_string())?;
        let (auth, client, signature) = (
            b64(&text("authenticatorData")?, "authenticatorData")?,
            b64(&text("clientDataJSON")?, "clientDataJSON")?,
            b64(&text("signature")?, "signature")?,
        );
        let count = ikigai_passkey::verify(
            &credential,
            &Assertion {
                credential_id: &credential.id,
                authenticator_data: &auth,
                client_data_json: &client,
                signature: &signature,
            },
            &Policy {
                rp_id: RP_ID,
                origin: &self.origin,
                challenge_b64url: &challenge,
                require_user_verified: true,
            },
        )
        // One refusal on the wire; the precise reason is in the error the caller logs.
        .map_err(|e| format!("the passkey assertion was refused ({e})"))?;
        if count != enrolled.sign_count {
            enrolled.sign_count = count;
            write_passkey(&self.layout, &enrolled)?;
        }
        let scopes = scopes_of(&self.layout, &enrolled.grant)?;
        let token = random_b64(32)?;
        let expires = now + SESSION_SECONDS;
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        state.sessions.retain(|_, s| s.expires > now);
        if state.sessions.len() >= MAX_PENDING {
            return Err(format!("{MAX_PENDING} sessions are already open"));
        }
        state.sessions.insert(
            sha256_hex(token.as_bytes()),
            Session {
                credential_id: id,
                expires,
            },
        );
        Ok((
            token,
            Identity {
                enrolled,
                scopes,
                expires,
            },
        ))
    }

    /// End a session. Unknown tokens are not an error: signing out twice is still signed out.
    pub fn logout(&self, token: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.sessions.remove(&sha256_hex(token.as_bytes()));
        }
    }

    /// The identity behind a session token, re-resolved NOW: a session whose passkey was
    /// removed from `clients.json`, or whose grant no longer resolves, is no identity.
    pub fn identity(&self, token: &str, now: u64) -> Option<Identity> {
        let (credential_id, expires) = {
            let state = self.state.lock().ok()?;
            let session = state.sessions.get(&sha256_hex(token.as_bytes()))?;
            if session.expires <= now {
                return None;
            }
            (session.credential_id.clone(), session.expires)
        };
        let enrolled = read_passkeys(&self.layout).ok()?.remove(&credential_id)?;
        let scopes = scopes_of(&self.layout, &enrolled.grant).ok()?;
        Some(Identity {
            enrolled,
            scopes,
            expires,
        })
    }

    fn check_client_data(&self, bytes: &[u8], kind: &str, challenge: &str) -> Result<(), String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|_| "clientDataJSON is not JSON".to_string())?;
        let field = |name: &str| v.get(name).and_then(Value::as_str).unwrap_or_default();
        if field("type") != kind {
            return Err(format!("clientDataJSON is not a `{kind}` ceremony"));
        }
        if field("origin") != self.origin {
            return Err(format!(
                "the ceremony ran at `{}`, and this server accepts only `{}` — open the page \
                 through `localhost`, not an IP address",
                field("origin"),
                self.origin
            ));
        }
        if field("challenge") != challenge {
            return Err("clientDataJSON answers a different challenge".to_string());
        }
        Ok(())
    }
}

/// Refuse a scope list naming a broad store token — shared with the invite command.
pub fn refuse_broad(grant: &str, scopes: &[String]) -> Result<(), String> {
    let broad = broad_store_scopes(scopes);
    if broad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "grant `{grant}` would name {} — the store's whole-dataset tokens",
            broad.join(" and ")
        ))
    }
}

fn b64(text: &str, field: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| format!("`{field}` is not base64url"))
}

fn random_b64(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("the OS random source failed: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Seconds since the epoch, from the wall clock. The capability function has no kernel to
/// take an injected clock from; everything testable here takes `now` as an argument instead.
pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{grants_for, Authority};

    #[test]
    fn an_invite_is_single_use_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let scopes = grants_for("default", Authority::Delete).unwrap();
        let code = invite(&layout, "brian", &scopes, false, 30, 1_000).unwrap();
        assert!(
            !std::fs::read_to_string(layout.invites_json())
                .unwrap()
                .contains(&code),
            "the file keeps a hash, never the code"
        );
        assert_eq!(redeem(&layout, &code, 1_001).unwrap(), "brian");
        assert!(redeem(&layout, &code, 1_002).is_err(), "single use");
        let late = invite(&layout, "brian", &scopes, false, 1, 1_000).unwrap();
        assert!(redeem(&layout, &late, 1_000 + 61)
            .unwrap_err()
            .contains("expired"));
        // The same grant name with different scopes is refused without --force.
        let read = grants_for("default", Authority::Read).unwrap();
        assert!(invite(&layout, "brian", &read, false, 30, 1_000).is_err());
    }

    #[test]
    fn a_challenge_is_single_use_and_bound_to_its_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp.take_challenge(&c, Purpose::Register, 11).is_err());
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp.take_challenge(&c, Purpose::Login, 11).is_ok());
        assert!(rp.take_challenge(&c, Purpose::Login, 12).is_err());
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp
            .take_challenge(&c, Purpose::Login, 10 + CHALLENGE_SECONDS)
            .is_err());
    }

    /// ★ A bound refuses; it never evicts someone's ceremony.
    #[test]
    fn past_the_bound_a_challenge_is_refused_not_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let first = rp.challenge(Purpose::Login, 0).unwrap();
        for _ in 1..MAX_PENDING {
            rp.challenge(Purpose::Login, 0).unwrap();
        }
        assert!(rp.challenge(Purpose::Login, 0).is_err());
        assert!(rp.take_challenge(&first, Purpose::Login, 1).is_ok());
    }
}
