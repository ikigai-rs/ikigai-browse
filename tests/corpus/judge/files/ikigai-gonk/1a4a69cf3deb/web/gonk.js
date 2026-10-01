// gonk's only application script: error display for htmx, and the passkey ceremonies.
//
// Everything the page SHOWS comes from the server as HTML; this file does three things htmx
// cannot. (1) htmx does not swap a 4xx/5xx response, so a refused action would otherwise
// vanish silently — the error body is written into #flash as TEXT (never as HTML: an error
// can quote what a caller typed). (2) WebAuthn is a browser API; the ceremony is the
// standard one, with byte fields carried as base64url. (3) the browse family's affordances
// name the /k/ adapter with a COMMAND in the path, which gonk's door cannot parse back into
// a resource — so the command is folded into a query value here, one line, grammar-level.
//
// ⚠ The session cookie is set HERE, not by the server — the HTTP transport cannot add a
// Set-Cookie header — so it is SameSite=Strict but not HttpOnly. The CSP forbids inline and
// third-party script, which is what that leaves as the defence.
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const COOKIE = "gonk_session";

  function flash(text, kind) {
    const area = $("flash");
    if (!area) return;
    area.textContent = "";
    if (!text) return;
    const p = document.createElement("p");
    p.className = "flash " + (kind || "ok");
    p.setAttribute("role", kind === "error" ? "alert" : "status");
    p.textContent = text;
    area.appendChild(p);
  }

  // ★ A REFUSAL FROM ONE AFFORDANCE IS NOT A PAGE FAILURE (ledger #442).
  //
  // Everything a browse page shows after the first paint is fetched by an affordance of its
  // own: the tree, a file, an explanation, and — with hx-trigger="load", before anyone has
  // clicked anything — the recent-pull-requests block. When the caller's grant does not
  // carry `urn:cap:exec:gh`, that last one answers a typed 403 while the tree underneath is
  // perfectly fine, and writing it into #flash made the whole page read as broken. So the
  // message goes where the request came FROM, and the page keeps its head.
  //
  // ⚠ This knows nothing about which affordance failed, on purpose — the same rule that
  // keeps the /k/ rewrite below grammar-level. It asks only where the element sits, which
  // is why a face that grows a new button tomorrow is covered without an edit here.
  //
  // ⚠ Only a 403 disables the control: an authority refusal is permanent for this grant, so
  // a retry cannot succeed and a live button would lie. Every other status (a 404 from an
  // unbound facade, a 503, a 500) leaves the control usable — those can change under it.
  function denyAt(elt, text, status) {
    if (!elt || !elt.closest || !elt.closest("#browse")) return false;
    const note = document.createElement("p");
    note.className = "affordance-error";
    note.setAttribute("role", "status");
    note.textContent = text;
    // An element htmx would have swapped INTO itself (the lazy blocks: no hx-target, or
    // "this") gets the note in place of its "loading…" placeholder — otherwise that
    // placeholder sits there spinning forever on a refusal.
    const target = elt.getAttribute("hx-target");
    if (!target || target === "this") {
      elt.textContent = "";
      elt.appendChild(note);
    } else {
      elt.insertAdjacentElement("afterend", note);
    }
    if (status === 403) {
      elt.setAttribute("aria-disabled", "true");
      elt.setAttribute("data-denied", "true");
      if (elt.tagName === "BUTTON") elt.disabled = true;
    }
    return true;
  }

  document.addEventListener("htmx:responseError", (e) => {
    const xhr = e.detail.xhr;
    const text = ((xhr && xhr.responseText) || "The server refused that.").trim();
    if (denyAt(e.detail.elt, text, xhr && xhr.status)) return;
    flash(text, "error");
  });
  document.addEventListener("htmx:sendError", () => flash("The server did not answer.", "error"));
  document.addEventListener("htmx:beforeRequest", () => flash("", "ok"));

  // ---------------------------------------------- the /k/ adapter's path spelling
  //
  // ikigai-browse authors every affordance as hx-get="/k/source {iri} [k=v ...]" or
  // hx-post="/k/sink urn:iki:annotation" — a COMMAND in the path, with spaces in it, and
  // with slashes inside the IRI. gonk's HTTP door percent-decodes the path and then rebuilds
  // a target IRI from it, so such a path is a 400 before any of gonk's code sees it: a space
  // is not legal in an IRI. (ikigai-web's standalone server parses the raw request-target
  // itself, which is why the same affordances work at 8642 untouched.)
  //
  // So the command travels as one query value instead, and this is where the two spellings
  // meet. It is grammar-level and knows nothing about which button was pressed: any /k/
  // request, from any face, present or future, folds the same way. The server's own shell
  // already emits the query form, so a broken rewrite here cannot make the first paint fail
  // silently — it fails on the first affordance, loudly, in #flash.
  document.addEventListener("htmx:configRequest", (e) => {
    const path = e.detail && e.detail.path;
    if (typeof path !== "string" || path.slice(0, 3) !== "/k/") return;
    e.detail.path = "/k?c=" + encodeURIComponent(path.slice(3));
  });

  // ------------------------------------------------- the queue's news, and who is mid-decision
  //
  // ★ THE FINDINGS LIST HAS NO CLOCK (ledger #469). The header badge already polls the
  // depth at the server's own interval; every answer carries `data-rev`, the queue's whole
  // state as one token. When that token changes there is something new to show, and this
  // relays it to the list as one event. When it does not change nothing is fetched, which
  // is the difference between this and a second `every 10s` on the list.
  //
  // ⚠ Why this is a script at all, when everything else the page shows comes from the
  // server: the badge cannot carry an `HX-Trigger` response header — gonk's HTTP transport
  // has no way to add one, which is the same limitation that puts the session cookie here —
  // and the badge must not know what a findings list looks like. So the DECISION is the
  // server's (it computes the revision) and this is the wire between two fragments.
  //
  // ⚠⚠ AND IT NEVER YANKS A ROW SOMEONE IS DECIDING ON. A swap that replaces the list while
  // a human is choosing a severity or typing a reason is worse than a stale page: it throws
  // away work they did. So news arriving during a decision is HELD — the page says so, out
  // loud — and delivered when the decision is submitted (which swaps the list anyway) or
  // abandoned.
  const NEWS = "gonk:news";
  let seenRev = null;
  let held = false;

  function queueSection() {
    return $("queue");
  }

  // Is a human in the middle of deciding? Focus is the obvious half; the other half is a
  // control they have already changed and not yet submitted, which survives losing focus.
  function midDecision(queue) {
    const active = document.activeElement;
    if (active && queue.contains(active) && active.matches("select, textarea, input, button")) {
      return true;
    }
    const changed = (sel) => {
      const rendered = sel.querySelector("option[selected]");
      const initial = rendered ? rendered.value : sel.options.length ? sel.options[0].value : "";
      return sel.value !== initial;
    };
    if (Array.prototype.some.call(queue.querySelectorAll("select"), changed)) return true;
    return Array.prototype.some.call(queue.querySelectorAll("textarea"), (t) => t.value !== "");
  }

  function deliver() {
    const queue = queueSection();
    // Not on the Queue page: the badge still polls (it is in every header) and there is
    // simply nothing here to refresh.
    if (!queue) return;
    if (midDecision(queue)) {
      held = true;
      const notice = $("queue-stale");
      if (notice) notice.hidden = false;
      return;
    }
    held = false;
    if (window.htmx) window.htmx.trigger(queue, NEWS);
  }

  // The badge's own span is the swap target; its answer is the element inside it.
  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || !target.classList || !target.classList.contains("queue-badge")) return;
    const badge = target.querySelector("[data-rev]");
    if (!badge) return;
    const rev = badge.getAttribute("data-rev");
    // ⚠ The FIRST poll only establishes the baseline. Treating it as news would refresh the
    // list once on every page load, for nothing.
    if (seenRev === null) {
      seenRev = rev;
      return;
    }
    if (rev === seenRev) return;
    seenRev = rev;
    deliver();
  });

  // A held refresh, let through the moment the decision is over. `change` and `focusout`
  // are when a selection is put back or a field is emptied; the swap after a submitted
  // decision brings the fresh list with it, so nothing is held across it.
  // ⚠ On a tick, not inline: during `focusout` the focus has LEFT and not yet ARRIVED, so
  // `document.activeElement` is the body — a human tabbing from the severity select to the
  // publish button would read as idle for exactly that instant, and the list would swap out
  // from under the button they were reaching for.
  const retry = () => {
    if (!held) return;
    window.setTimeout(() => {
      if (!held) return;
      const queue = queueSection();
      if (queue && !midDecision(queue)) deliver();
    }, 0);
  };
  document.addEventListener("focusout", retry);
  document.addEventListener("change", retry);

  // ------------------------------------------------------------------ bytes

  function toB64(buffer) {
    const bytes = new Uint8Array(buffer);
    let s = "";
    for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function fromB64(text) {
    const b = text.replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(b + "===".slice((b.length + 3) % 4));
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }

  async function post(path, body) {
    const res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: typeof body === "string" ? body : JSON.stringify(body || {}),
      credentials: "same-origin",
    });
    const text = await res.text();
    if (!res.ok) throw new Error(text.trim() || res.statusText);
    return text ? JSON.parse(text) : {};
  }

  function token() {
    const hit = document.cookie.split("; ").find((c) => c.startsWith(COOKIE + "="));
    return hit ? decodeURIComponent(hit.slice(COOKIE.length + 1)) : "";
  }

  function setToken(value, seconds) {
    document.cookie =
      COOKIE + "=" + encodeURIComponent(value) + "; Path=/; SameSite=Strict; Max-Age=" + seconds;
  }

  // ------------------------------------------------------------------ ceremonies

  // A WebAuthn call the browser refuses rejects with NotAllowedError whatever the cause — a
  // cancelled sheet, a timeout, or a page without focus — and its DOM text ("The document is
  // not focused", "The operation either timed out or was not allowed") says nothing a person
  // can act on. Say what to do instead. Anything else (a server refusal) is already words.
  function ceremonyError(err, retry) {
    if (err && err.name === "NotAllowedError") {
      return "the passkey prompt was cancelled, timed out, or opened while this window did not have focus. " + retry;
    }
    return (err && err.message) || String(err);
  }

  async function signIn() {
    try {
      const { challenge } = await post("/auth/login-options");
      const cred = await navigator.credentials.get({
        publicKey: {
          challenge: fromB64(challenge),
          rpId: "localhost",
          userVerification: "required",
          timeout: 120000,
        },
      });
      const r = cred.response;
      const done = await post("/auth/login", {
        challenge,
        id: cred.id,
        authenticatorData: toB64(r.authenticatorData),
        clientDataJSON: toB64(r.clientDataJSON),
        signature: toB64(r.signature),
      });
      setToken(done.session, done.seconds);
      location.reload();
    } catch (err) {
      flash("Sign-in did not complete: " + ceremonyError(err, "Click Sign in with passkey to try again."), "error");
      const login = $("auth-login");
      if (login && !login.hidden) login.focus();
    }
  }

  async function signOut() {
    const t = token();
    setToken("", 0);
    try {
      if (t) await post("/auth/logout", t);
    } catch (_) {
      // Signed out locally either way; the server session expires on its own.
    }
    location.reload();
  }

  async function enrol(invite, label) {
    try {
      const { challenge } = await post("/auth/register-options");
      const user = new Uint8Array(16);
      crypto.getRandomValues(user);
      const name = label || "gonk passkey";
      const cred = await navigator.credentials.create({
        publicKey: {
          challenge: fromB64(challenge),
          rp: { id: "localhost", name: "gonk" },
          user: { id: user, name, displayName: name },
          pubKeyCredParams: [{ type: "public-key", alg: -7 }],
          authenticatorSelection: { residentKey: "required", userVerification: "required" },
          attestation: "none",
          timeout: 120000,
        },
      });
      const key = cred.response.getPublicKey && cred.response.getPublicKey();
      if (!key) throw new Error("this browser does not expose the credential's public key");
      const done = await post("/auth/register", {
        challenge,
        invite,
        label: name,
        id: cred.id,
        publicKey: toB64(key),
        clientDataJSON: toB64(cred.response.clientDataJSON),
      });
      history.replaceState(null, "", location.pathname + location.search);
      $("enrol").hidden = true;
      // ★ Do NOT chain navigator.credentials.get() here. When create() resolves, the system
      // passkey sheet still holds focus, and Chrome refuses get() from an unfocused document
      // ("The document is not focused") — found by the first real Touch ID enrolment. A click
      // on the sign-in button is a fresh user gesture on a page that has focus back.
      const created = "Passkey created for " + done.label + " (grant " + done.grant + "). ";
      const login = $("auth-login");
      if ($("auth-logout").hidden) {
        login.hidden = false;
        flash(created + "Now click Sign in with passkey to use it.", "ok");
        login.focus();
      } else {
        flash(created + "Sign out, then sign in with the new passkey to use its grant.", "ok");
      }
    } catch (err) {
      flash(
        "Could not create the passkey: " +
          ceremonyError(err, "The invite is still good until it expires — click Create passkey to try again."),
        "error"
      );
    }
  }

  // ------------------------------------------------------------------ samples

  // A sample button puts its query in the editor and stops there. It never submits: the
  // person presses Run, so a half-typed query is never lost to a click, and a query that
  // would be expensive is never started by accident. The text lives in a hidden <pre> the
  // server rendered (newlines survive an element; an attribute's would not).
  //
  // ★ The buttons are mutually exclusive TOGGLES, and the state they carry is a claim about
  // the editor: `aria-pressed="true"` means "the box holds this sample's query". So the
  // interesting half is not setting it but CLEARING it — the moment a character is typed the
  // claim is false, and a button still making it is the UI lying about what is on screen.
  // Cleared on `input`, not on blur and not on submit.
  //
  // ⚠ Typing and then undoing back to the exact sample text leaves it cleared, deliberately.
  // Re-deriving the state by comparing the text would be a guess about intent, and it is
  // wrong in the other direction too: two samples can be edited into each other.
  function wireSamples() {
    const box = $("q");
    if (!box) return;
    const buttons = document.querySelectorAll("button.sample");
    const active = () => document.querySelector('button.sample[aria-pressed="true"]');
    const press = (button) => {
      for (const b of buttons) b.setAttribute("aria-pressed", b === button ? "true" : "false");
    };
    for (const button of buttons) {
      button.addEventListener("click", () => {
        const source = $(button.getAttribute("data-query"));
        if (!source) return;
        // Assigning `.value` fires no `input` event, so this does not immediately undo
        // itself. Re-clicking the pressed sample restores its text after an edit.
        box.value = source.textContent;
        press(button);
        box.focus();
        box.setSelectionRange(box.value.length, box.value.length);
        flash("Loaded the sample query. Press Run to execute it.", "ok");
      });
    }
    // Only when something is actually pressed: rewriting nine unchanged attributes on every
    // keystroke is work a screen reader may notice.
    box.addEventListener("input", () => {
      if (active()) press(null);
    });
  }

  async function init() {
    wireSamples();
    const login = $("auth-login");
    if (!login) return; // a fragment, not a page
    if (location.hostname !== "localhost") {
      const note = $("auth-localhost");
      note.href = location.href.replace(location.hostname, "localhost");
      note.hidden = false;
      return;
    }
    if (!window.PublicKeyCredential) {
      $("auth-localhost").textContent = "This browser has no passkey support";
      $("auth-localhost").hidden = false;
      return;
    }
    login.addEventListener("click", signIn);
    $("auth-logout").addEventListener("click", signOut);

    const t = token();
    if (t) {
      try {
        const who = await post("/auth/session", t);
        $("auth-who").textContent = "Signed in as " + who.label + " (grant " + who.grant + ")";
        $("auth-who").hidden = false;
        $("auth-logout").hidden = false;
      } catch (_) {
        setToken("", 0);
        login.hidden = false;
      }
    } else {
      login.hidden = false;
    }

    const m = location.hash.match(/^#invite=([A-Za-z0-9_-]+)$/);
    if (m) {
      $("enrol").hidden = false;
      $("enrol-form").addEventListener("submit", (e) => {
        e.preventDefault();
        enrol(m[1], $("enrol-label").value.trim());
      });
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
