<?xml version="1.0"?>
<xsl:stylesheet version="1.0"
  xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
  xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
  xmlns:ledger="https://ikigai-rs.dev/ns/ledger#"
  xmlns:dcterms="http://purl.org/dc/terms/"
  xmlns:view="urn:iki:gonk:view#">
<!--
  ⚠ This comment is INSIDE the root element on purpose: xrust refuses a stylesheet whose
  first node is a comment ("not an XSLT stylesheet").

  gonk's HTML face: ONE stylesheet, dispatching on rdf:type.

  The input is a <view:page> envelope around a resource's graph face as RDF/XML
  (src/render.rs). Each subject's primary type is written as its element name, so
  `match="ledger:Item"` and `match="ledger:Comment"` ARE the type dispatch.

  ⚠ Authored to xrust's SUBSET of XSLT 1.0, measured (src/render.rs has the table):
  no xsl:variable, no xsl:key, no attribute value templates beyond {@name}, no
  predicates in match patterns, no absolute paths from inside a nested template, no
  xsl:sort inside for-each. So every dynamic attribute is xsl:attribute, every sort
  is on apply-templates, and everything a template would otherwise compute arrives as
  a view:* literal on the subject it describes.

  ⚠ No `{` or `}` in any literal attribute value: they are AVT delimiters.
-->
  <xsl:output method="html" omit-xml-declaration="yes"/>

  <xsl:template match="/">
    <xsl:apply-templates select="view:page"/>
  </xsl:template>

  <!-- ============================================================ the shell -->

  <xsl:template match="view:page">
    <xsl:choose>
      <xsl:when test="@full = 'true'">
        <html lang="en">
          <head>
            <meta charset="utf-8"/>
            <meta name="viewport" content="width=device-width, initial-scale=1"/>
            <meta name="htmx-config"><xsl:attribute name="content">{"includeIndicatorStyles":false,"allowEval":false}</xsl:attribute></meta>
            <title><xsl:value-of select="@title"/> · gonk</title>
            <link rel="stylesheet" href="/static/gonk.css"/>
            <!-- A page that hosts another family's faces links that family's own stylesheet
                 (urn:repo:style, through the /k/ adapter). Withheld when the caller could
                 not read it, so the page does not paint itself with a 403. -->
            <xsl:if test="@stylesheet">
              <link rel="stylesheet"><xsl:attribute name="href"><xsl:value-of select="@stylesheet"/></xsl:attribute></link>
            </xsl:if>
            <!-- …and that family's LAYOUT sheet (urn:repo:style:layout, ikigai-browse 0.4.2+),
                 which is the one that makes its markup legible: the theme above only colours
                 the hl- classes inside a file view. Same gate, same reason. Ledger #441. -->
            <xsl:if test="@layout-stylesheet">
              <link rel="stylesheet"><xsl:attribute name="href"><xsl:value-of select="@layout-stylesheet"/></xsl:attribute></link>
            </xsl:if>
            <script src="/static/htmx.min.js" defer="defer"></script>
            <script src="/static/gonk.js" defer="defer"></script>
          </head>
          <body>
            <a class="skip" href="#main">Skip to content</a>
            <header class="top">
              <a class="brand" href="/">gonk</a>
              <nav class="ledgers" aria-label="Ledgers">
                <xsl:apply-templates select="view:ledger"/>
              </nav>
              <xsl:apply-templates select="view:browse"/>
              <xsl:apply-templates select="view:queue"/>
              <a class="navlink" href="/sparql">SPARQL</a>
              <div id="auth" class="auth">
                <span id="auth-who" class="who" hidden="hidden"></span>
                <button id="auth-login" type="button" class="quiet" hidden="hidden">Sign in with passkey</button>
                <button id="auth-logout" type="button" class="quiet" hidden="hidden">Sign out</button>
                <a id="auth-localhost" class="note" hidden="hidden">Passkeys need localhost</a>
              </div>
            </header>
            <section id="enrol" class="panel enrol" hidden="hidden" aria-labelledby="enrol-title">
              <h2 id="enrol-title">Create a passkey for this server</h2>
              <p>This link carries a one-time invite. Creating the passkey enrols it under the grant the invite names; after that, signing in with it gives this browser that grant.</p>
              <form id="enrol-form" class="row">
                <label for="enrol-label">Label</label>
                <input id="enrol-label" name="label" type="text" autocomplete="off" placeholder="e.g. laptop Touch ID"/>
                <button type="submit">Create passkey</button>
              </form>
            </section>
            <div id="flash" class="flash-area" role="status" aria-live="polite"></div>
            <main id="main">
              <xsl:call-template name="body"/>
            </main>
          </body>
        </html>
      </xsl:when>
      <xsl:otherwise>
        <xsl:call-template name="body"/>
      </xsl:otherwise>
    </xsl:choose>
  </xsl:template>

  <xsl:template match="view:ledger">
    <a class="ledger-link">
      <xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute>
      <xsl:if test="@current = 'true'"><xsl:attribute name="aria-current">page</xsl:attribute></xsl:if>
      <xsl:value-of select="@name"/>
    </a>
  </xsl:template>

  <!-- The browse family's entry point, present only when this caller may read a root
       (src/web.rs::nav). A link into a refusal is worse than no link. -->
  <xsl:template match="view:browse">
    <a class="navlink"><xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute>Browse</a>
  </xsl:template>

  <!-- The review queue, present only when this caller may read a root AND may decide
       (src/queue.rs::offers_queue). Ledger #444 settled the label: it names the THING,
       because the page shows the whole pipeline and "Review" is already the file face's
       button that RUNS a pass.

       ⚠ The badge beside it is the armed trigger's ONLY liveness signal (ledger #466), not
       decoration: `watch()` catches up at startup and then lives on a thread nothing else
       observes, so a depth that stops falling is the whole symptom of a dead watcher. It
       polls rather than blinking — the web-demo nav clock's shape, at the interval
       src/queue.rs::BADGE_EVERY names, so the number and the cadence are the Rust side's
       and not this file's. -->
  <xsl:template match="view:queue">
    <a class="navlink"><xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute>Queue<xsl:if test="@depth-url">
      <span class="queue-badge" hx-swap="innerHTML">
        <xsl:attribute name="hx-get"><xsl:value-of select="@depth-url"/></xsl:attribute>
        <xsl:attribute name="hx-trigger">load, every <xsl:value-of select="@every"/></xsl:attribute>
      </span>
    </xsl:if></a>
  </xsl:template>

  <!-- The badge's own answer: one number, one state word as a class, and the depth
       resource's whole sentence as the tooltip. `stuck` is the one a person is looking
       for — armed, not empty, nothing running.

       ⚠ Two things ride along that are NOT the number (ledger #469, and both are computed
       in src/queue.rs — this file decides nothing):

       `@activity` draws a spark: a pass running now, or one that ended inside the window
       the server measures against its own poll interval. Without it a pass shorter than the
       cadence is invisible, which is the whole complaint — the depth was 0 before it and 0
       after it, and the badge had nothing to say either time.

       `@rev` is the queue's whole state as one token. web/gonk.js compares it with the
       previous poll's and, when it differs, tells the findings list there is news. It is an
       attribute rather than markup because the badge must not know what a list looks
       like. -->
  <xsl:template name="queue-badge">
    <span>
      <xsl:attribute name="class">badge-depth <xsl:value-of select="@kind"/></xsl:attribute>
      <xsl:attribute name="title"><xsl:value-of select="@title"/></xsl:attribute>
      <xsl:if test="@rev"><xsl:attribute name="data-rev"><xsl:value-of select="@rev"/></xsl:attribute></xsl:if>
      <xsl:if test="@activity">
        <span aria-hidden="true">
          <xsl:attribute name="class">spark <xsl:value-of select="@activity"/></xsl:attribute>
        </span>
      </xsl:if>
      <xsl:value-of select="@count"/>
    </span>
  </xsl:template>

  <xsl:template match="view:ledger" mode="option">
    <option>
      <xsl:attribute name="value"><xsl:value-of select="@name"/></xsl:attribute>
      <xsl:if test="@current = 'true'"><xsl:attribute name="selected">selected</xsl:attribute></xsl:if>
      <xsl:value-of select="@name"/>
    </option>
  </xsl:template>

  <xsl:template match="view:flash">
    <p>
      <xsl:attribute name="class">flash <xsl:value-of select="@kind"/></xsl:attribute>
      <xsl:value-of select="."/>
    </p>
  </xsl:template>

  <xsl:template name="body">
    <xsl:choose>
      <xsl:when test="@view = 'ledger'"><xsl:call-template name="ledger"/></xsl:when>
      <xsl:when test="@view = 'item'"><xsl:call-template name="item"/></xsl:when>
      <xsl:when test="@view = 'gone'"><xsl:call-template name="gone"/></xsl:when>
      <xsl:when test="@view = 'sparql'"><xsl:call-template name="sparql"/></xsl:when>
      <xsl:when test="@view = 'browse'"><xsl:call-template name="browse"/></xsl:when>
      <xsl:when test="@view = 'roots'"><xsl:call-template name="roots"/></xsl:when>
      <xsl:when test="@view = 'queue'"><xsl:call-template name="queue"/></xsl:when>
      <xsl:when test="@view = 'queue-badge'"><xsl:call-template name="queue-badge"/></xsl:when>
      <xsl:when test="@view = 'results'"><xsl:apply-templates select="view:results"/></xsl:when>
      <xsl:otherwise>
        <section class="panel empty-state">
          <h1><xsl:value-of select="@title"/></h1>
          <p><xsl:value-of select="@message"/></p>
        </section>
      </xsl:otherwise>
    </xsl:choose>
  </xsl:template>

  <!-- ======================================================== browse shell -->

  <!--
    The region ikigai-browse's own HTML faces render into. Everything after the first
    paint is THEIR markup and THEIR affordances (hx-get/hx-post at the /k/ adapter), so
    there is nothing here to keep in step with them — which is the point.
  -->
  <!--
    The roots this caller may browse. One link each, into that root's tree; the rest of
    the browse family is reached from there. ⚠ A root the caller cannot read is not in
    this list at all — the page is built from `k::readable_roots`, not from the config.
  -->
  <xsl:template name="roots">
    <section class="panel roots" aria-labelledby="roots-title">
      <h1 id="roots-title"><xsl:value-of select="@title"/></h1>
      <p class="note"><xsl:value-of select="@message"/></p>
      <ul class="root-list">
        <xsl:apply-templates select="view:root"/>
      </ul>
    </section>
  </xsl:template>

  <xsl:template match="view:root">
    <li>
      <a class="root-link">
        <xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute>
        <xsl:value-of select="@name"/>
      </a>
      <span class="note"><xsl:value-of select="@iri"/></span>
    </li>
  </xsl:template>

  <xsl:template name="browse">
    <section class="browse-shell" aria-label="Browse">
      <!-- The posture the layout sheet keys on, when the door knows this caller cannot
           write annotations. Unanchored in that sheet, so this ancestor will do. -->
      <xsl:if test="@posture">
        <xsl:attribute name="data-browse-posture"><xsl:value-of select="@posture"/></xsl:attribute>
      </xsl:if>
      <xsl:choose>
        <xsl:when test="@start-url">
          <div id="browse" class="browse" hx-trigger="load" hx-swap="innerHTML">
            <xsl:attribute name="hx-get"><xsl:value-of select="@start-url"/></xsl:attribute>
            <p class="note"><xsl:value-of select="@message"/></p>
          </div>
        </xsl:when>
        <xsl:otherwise>
          <div id="browse" class="browse panel">
            <h1><xsl:value-of select="@title"/></h1>
            <p class="note"><xsl:value-of select="@message"/></p>
          </div>
        </xsl:otherwise>
      </xsl:choose>
    </section>
  </xsl:template>


  <!-- ==================================================== the review queue -->

  <!--
    Ledger #444. The rows come from `urn:repo:{root}:findings` in the order that resource
    returns them (triage order: severity rank, then path, then position) and are NOT
    re-sorted here. The severity menu, the decision buttons and this state nav are all
    rendered from `view:*` elements the Rust side built out of the resources' own
    `one_of` declarations — there is no severity word, decision word or state word written
    anywhere in this stylesheet, and that is deliberate (src/queue.rs).
  -->
  <xsl:template name="queue">
    <section id="queue" class="queue">
      <!-- ★ The one page in gonk whose content changes with nobody touching it, and until
           ledger #469 the one page that never refreshed itself.

           It has NO clock of its own. `@news` is the event the header badge's existing poll
           raises when the queue's revision changes (web/gonk.js relays it), so this list is
           re-fetched when there is something to show and never on a timer — one cadence on
           this server, and it is src/queue.rs::BADGE_EVERY. `@refresh-url` keeps the filter
           AND the row bound the human is looking at. -->
      <xsl:if test="@refresh-url">
        <xsl:attribute name="hx-get"><xsl:value-of select="@refresh-url"/></xsl:attribute>
        <xsl:attribute name="hx-trigger"><xsl:value-of select="@news"/></xsl:attribute>
        <xsl:attribute name="hx-target">#queue</xsl:attribute>
        <xsl:attribute name="hx-swap">outerHTML</xsl:attribute>
      </xsl:if>
      <header class="queue-head">
        <h1><xsl:value-of select="@title"/></h1>
        <p class="note"><xsl:value-of select="@message"/></p>
        <nav class="filters" aria-label="Pipeline state">
          <xsl:apply-templates select="view:state"/>
        </nav>
      </header>
      <xsl:apply-templates select="view:intray"/>
      <xsl:apply-templates select="view:flash"/>
      <xsl:if test="@posture-text">
        <p class="note posture"><xsl:value-of select="@posture-text"/></p>
      </xsl:if>
      <xsl:apply-templates select="view:denied"/>
      <xsl:if test="@empty = 'true'">
        <p class="empty"><xsl:value-of select="@empty-text"/></p>
      </xsl:if>
      <xsl:if test="@count-text">
        <p class="count">
          <span class="how-many"><xsl:value-of select="@count-text"/></span>
          <xsl:if test="@more = 'true'">
            <a class="more" hx-target="#queue" hx-swap="outerHTML">
              <xsl:attribute name="href"><xsl:value-of select="@more-url"/></xsl:attribute>
              <xsl:attribute name="hx-get"><xsl:value-of select="@more-rows-url"/></xsl:attribute>
              <xsl:attribute name="hx-push-url"><xsl:value-of select="@more-url"/></xsl:attribute>
              <xsl:value-of select="@more-label"/>
            </a>
          </xsl:if>
        </p>
      </xsl:if>
      <!-- ⚠ A refresh that was HELD BACK, because a human had a decision open on this list
           (web/gonk.js decides; the sentence is the server's). Hidden until that happens,
           and `role="status"` so it is announced rather than only seen. -->
      <xsl:if test="@stale-text">
        <p class="stale" id="queue-stale" hidden="hidden" role="status"><xsl:value-of select="@stale-text"/></p>
      </xsl:if>
      <ol class="findings">
        <xsl:apply-templates select="view:finding"/>
      </ol>
    </section>
  </xsl:template>

  <xsl:template match="view:state">
    <a hx-target="#queue" hx-swap="outerHTML">
      <xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute>
      <xsl:attribute name="hx-get"><xsl:value-of select="@rows-url"/></xsl:attribute>
      <xsl:attribute name="hx-push-url"><xsl:value-of select="@href"/></xsl:attribute>
      <xsl:if test="@current = 'true'"><xsl:attribute name="aria-current">true</xsl:attribute></xsl:if>
      <xsl:value-of select="@name"/>
    </a>
  </xsl:template>

  <!-- The trigger's intray depth, which no other face has. ⚠ Four kinds, and three of
       them are NOT a number: "no queue configured", "empty", and "could not be read" are
       different statements and must not render alike (ledger #446). -->
  <xsl:template match="view:intray">
    <p>
      <xsl:attribute name="class">intray <xsl:value-of select="@kind"/></xsl:attribute>
      <xsl:value-of select="."/>
    </p>
  </xsl:template>

  <!-- One repository's read was refused. The page still renders the others: a partial
       answer that says which part is missing beats a whole page replaced by one 403. -->
  <xsl:template match="view:denied">
    <p class="flash error">
      <xsl:value-of select="@repo"/><xsl:text>: </xsl:text><xsl:value-of select="."/>
    </p>
  </xsl:template>

  <xsl:template match="view:finding">
    <li>
      <xsl:attribute name="class">finding <xsl:value-of select="@state"/></xsl:attribute>
      <div class="finding-head">
        <span>
          <xsl:attribute name="class">badge sev <xsl:value-of select="@severity"/></xsl:attribute>
          <xsl:value-of select="@severity-label"/>
        </span>
        <xsl:if test="@effective">
          <span class="badge final"><xsl:text>human: </xsl:text><xsl:value-of select="@effective"/></span>
        </xsl:if>
        <span>
          <xsl:attribute name="class">badge state <xsl:value-of select="@state"/></xsl:attribute>
          <xsl:value-of select="@state"/>
        </span>
        <xsl:if test="@orphaned = 'true'"><span class="badge warn">orphaned</span></xsl:if>
        <xsl:if test="@reanchored = 'true'"><span class="badge warn">re-anchored</span></xsl:if>
        <xsl:choose>
          <xsl:when test="@browse-href">
            <a class="finding-where">
              <xsl:attribute name="href"><xsl:value-of select="@browse-href"/></xsl:attribute>
              <xsl:value-of select="@repo"/><xsl:text>/</xsl:text><xsl:value-of select="@where"/>
            </a>
          </xsl:when>
          <xsl:otherwise>
            <span class="finding-where"><xsl:value-of select="@repo"/><xsl:text>/</xsl:text><xsl:value-of select="@where"/></span>
          </xsl:otherwise>
        </xsl:choose>
      </div>
      <p class="finding-body"><xsl:value-of select="view:body"/></p>
      <xsl:if test="view:quote">
        <pre class="finding-quote"><xsl:value-of select="view:quote"/></pre>
      </xsl:if>
      <p class="note finding-prov"><xsl:value-of select="@provenance"/></p>
      <xsl:apply-templates select="view:decision"/>
      <xsl:apply-templates select="view:decide"/>
      <xsl:apply-templates select="view:no-form"/>
    </li>
  </xsl:template>

  <!-- A decision already taken. ★ It is final: the form is gone, and the line says what
       undoing it would actually be — deleting the ANNOTATION (a separate, visible act) for
       a publication, and nothing at all for a decline, because the decline is the record. -->
  <xsl:template match="view:decision">
    <div>
      <xsl:attribute name="class">decision <xsl:value-of select="@outcome"/></xsl:attribute>
      <p class="decision-line">
        <xsl:value-of select="@outcome"/><xsl:text> as </xsl:text><xsl:value-of select="@severity"/><xsl:text> · </xsl:text><xsl:value-of select="@at"/>
      </p>
      <xsl:if test="view:note">
        <p class="decision-note"><xsl:value-of select="view:note"/></p>
      </xsl:if>
      <p class="note"><xsl:value-of select="@undo"/></p>
      <xsl:if test="@minted-href">
        <a class="minted">
          <xsl:attribute name="href"><xsl:value-of select="@minted-href"/></xsl:attribute>
          <xsl:value-of select="@minted"/>
        </a>
      </xsl:if>
    </div>
  </xsl:template>

  <!-- The human's answer. The `action` is a plain form post as well as an htmx one, so the
       page works with scripting off; the buttons carry `hx-vals` built in Rust rather than
       a JSON literal here, because a literal attribute value in this engine may not contain
       a curly brace at all (they are attribute-value-template delimiters), and JSON is
       nothing but curly braces. -->
  <xsl:template match="view:decide">
    <form class="decide" method="post">
      <xsl:attribute name="action"><xsl:value-of select="@action"/></xsl:attribute>
      <input type="hidden" name="id"><xsl:attribute name="value"><xsl:value-of select="@id"/></xsl:attribute></input>
      <input type="hidden" name="_state"><xsl:attribute name="value"><xsl:value-of select="@state"/></xsl:attribute></input>
      <input type="hidden" name="_repo"><xsl:attribute name="value"><xsl:value-of select="@repo"/></xsl:attribute></input>
      <label class="decide-severity">
        <xsl:text>Severity</xsl:text>
        <select name="severity">
          <xsl:if test="@required = 'true'"><xsl:attribute name="required">required</xsl:attribute></xsl:if>
          <xsl:apply-templates select="view:severity-option"/>
        </select>
      </label>
      <label class="decide-reason">
        <xsl:text>Reason — kept on both outcomes</xsl:text>
        <textarea name="content" rows="2"></textarea>
      </label>
      <div class="decide-buttons">
        <xsl:apply-templates select="view:decision-option"/>
      </div>
    </form>
  </xsl:template>

  <xsl:template match="view:severity-option">
    <option>
      <xsl:attribute name="value"><xsl:value-of select="@value"/></xsl:attribute>
      <xsl:if test="@selected = 'true'"><xsl:attribute name="selected">selected</xsl:attribute></xsl:if>
      <xsl:if test="@placeholder = 'true'"><xsl:attribute name="disabled">disabled</xsl:attribute></xsl:if>
      <xsl:value-of select="@label"/>
    </option>
  </xsl:template>

  <xsl:template match="view:decision-option">
    <button type="submit" name="decision" hx-target="#queue" hx-swap="outerHTML" hx-include="closest form">
      <xsl:attribute name="value"><xsl:value-of select="@value"/></xsl:attribute>
      <xsl:attribute name="hx-post"><xsl:value-of select="@action"/></xsl:attribute>
      <xsl:attribute name="hx-vals"><xsl:value-of select="@vals"/></xsl:attribute>
      <xsl:attribute name="class">decide-button <xsl:value-of select="@value"/></xsl:attribute>
      <xsl:value-of select="@label"/>
    </button>
  </xsl:template>

  <!-- ⚠ The manifold went quiet: the finding resource did not describe its menus, so this
       page renders NO form rather than inventing one. -->
  <xsl:template match="view:no-form">
    <p class="note warn"><xsl:value-of select="."/></p>
  </xsl:template>

  <!-- ============================================================ a ledger -->

  <xsl:template name="ledger">
    <section id="ledger" class="ledger">
      <header class="ledger-head">
        <h1><xsl:value-of select="@title"/></h1>
        <nav class="filters" aria-label="Status">
          <xsl:call-template name="status-link"><xsl:with-param name="status" select="'open'"/></xsl:call-template>
          <xsl:call-template name="status-link"><xsl:with-param name="status" select="'closed'"/></xsl:call-template>
          <xsl:call-template name="status-link"><xsl:with-param name="status" select="'all'"/></xsl:call-template>
        </nav>
        <form class="search" method="get" role="search" hx-target="#ledger" hx-swap="outerHTML">
          <xsl:attribute name="action"><xsl:value-of select="@page-url"/></xsl:attribute>
          <xsl:attribute name="hx-get"><xsl:value-of select="@items-url"/></xsl:attribute>
          <input type="hidden" name="status"><xsl:attribute name="value"><xsl:value-of select="@status"/></xsl:attribute></input>
          <label class="sr-only" for="search-text">Search titles</label>
          <input id="search-text" type="search" name="text" placeholder="Search titles">
            <xsl:attribute name="value"><xsl:value-of select="@text"/></xsl:attribute>
          </input>
          <button type="submit" class="quiet">Search</button>
        </form>
      </header>
      <xsl:apply-templates select="view:flash"/>
      <xsl:if test="@can-write = 'true'">
        <form class="panel file" method="post" action="/act" hx-post="/act" hx-target="#ledger" hx-swap="outerHTML">
          <input type="hidden" name="_action" value="append"/>
          <input type="hidden" name="_then" value="items"/>
          <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="@ledger"/></xsl:attribute></input>
          <input type="hidden" name="_status"><xsl:attribute name="value"><xsl:value-of select="@status"/></xsl:attribute></input>
          <label for="file-content">File an item — first line is the title, then a blank line, then the body</label>
          <textarea id="file-content" name="content" rows="3" required="required"></textarea>
          <details>
            <summary>More fields</summary>
            <div class="grid">
              <label>Priority
                <select name="priority">
                  <option value="">unset</option>
                  <option value="0">0 highest</option>
                  <option value="1">1</option>
                  <option value="2">2</option>
                  <option value="3">3</option>
                  <option value="4">4 lowest</option>
                </select>
              </label>
              <label>Labels <input type="text" name="labels" placeholder="comma, separated"/></label>
              <label>About <input type="text" name="about" placeholder="resource IRIs, space separated"/></label>
              <label>Revision <input type="text" name="revision" placeholder="commit sha or tag"/></label>
            </div>
          </details>
          <button type="submit">File</button>
        </form>
      </xsl:if>
      <xsl:if test="count(rdf:RDF/ledger:Item) = 0">
        <p class="empty">No items match.</p>
      </xsl:if>
      <!-- ⚠ What this page rendered and what the filter MATCHED are different numbers, and
           the page says both: a listing that reports its own length as the total is the
           defect in #419, one surface over. The sentence and the link are built in Rust
           (`web::Count`) because the engine has no variables to build them with. -->
      <xsl:if test="count(rdf:RDF/ledger:Item) &gt; 0">
        <p class="count">
          <span class="how-many"><xsl:value-of select="@count"/></span>
          <xsl:if test="@more = 'true'">
            <a class="more" hx-target="#ledger" hx-swap="outerHTML">
              <xsl:attribute name="href"><xsl:value-of select="@more-url"/></xsl:attribute>
              <xsl:attribute name="hx-get"><xsl:value-of select="@more-items-url"/></xsl:attribute>
              <xsl:attribute name="hx-push-url"><xsl:value-of select="@more-url"/></xsl:attribute>
              <xsl:value-of select="@more-label"/>
            </a>
          </xsl:if>
        </p>
        <ol class="items">
          <xsl:apply-templates select="rdf:RDF/ledger:Item">
            <xsl:sort select="dcterms:modified" order="descending"/>
          </xsl:apply-templates>
        </ol>
      </xsl:if>
    </section>
  </xsl:template>

  <xsl:template name="status-link">
    <xsl:param name="status"/>
    <a hx-target="#ledger" hx-swap="outerHTML">
      <xsl:attribute name="href"><xsl:value-of select="@page-url"/>?status=<xsl:value-of select="$status"/></xsl:attribute>
      <xsl:attribute name="hx-get"><xsl:value-of select="@items-url"/>?status=<xsl:value-of select="$status"/></xsl:attribute>
      <xsl:attribute name="hx-push-url"><xsl:value-of select="@page-url"/>?status=<xsl:value-of select="$status"/></xsl:attribute>
      <xsl:if test="@status = $status"><xsl:attribute name="aria-current">true</xsl:attribute></xsl:if>
      <xsl:value-of select="$status"/>
    </a>
  </xsl:template>

  <!-- One row of a ledger listing: the default rendering of a ledger:Item. -->
  <xsl:template match="ledger:Item">
    <li>
      <xsl:attribute name="class">row <xsl:value-of select="view:status"/></xsl:attribute>
      <span class="num"><xsl:value-of select="view:short"/></span>
      <a class="title">
        <xsl:attribute name="href"><xsl:value-of select="view:href"/></xsl:attribute>
        <xsl:value-of select="dcterms:title"/>
      </a>
      <span class="badges">
        <xsl:call-template name="badges"/>
      </span>
      <xsl:if test="view:canWrite = 'true'">
        <xsl:if test="view:status = 'open'">
          <form class="inline" method="post" action="/act" hx-post="/act" hx-target="#ledger" hx-swap="outerHTML">
            <input type="hidden" name="_action" value="close"/>
            <input type="hidden" name="_then" value="items"/>
            <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
            <input type="hidden" name="_status"><xsl:attribute name="value"><xsl:value-of select="view:listStatus"/></xsl:attribute></input>
            <input type="hidden" name="item"><xsl:attribute name="value"><xsl:value-of select="view:iri"/></xsl:attribute></input>
            <button type="submit" class="quiet small">
              <xsl:attribute name="aria-label">Close <xsl:value-of select="view:short"/> as done</xsl:attribute>
              Close
            </button>
          </form>
        </xsl:if>
      </xsl:if>
    </li>
  </xsl:template>

  <xsl:template name="badges">
    <span>
      <xsl:attribute name="class">badge <xsl:value-of select="view:status"/></xsl:attribute>
      <xsl:value-of select="view:status"/>
      <xsl:if test="view:reason"> · <xsl:value-of select="view:reason"/></xsl:if>
    </span>
    <span class="badge priority"><xsl:value-of select="view:priority"/></span>
    <xsl:if test="view:deferred = 'true'"><span class="badge deferred">deferred</span></xsl:if>
    <xsl:if test="ledger:claimedBy"><span class="badge claimed">claimed by <xsl:value-of select="ledger:claimedBy"/></span></xsl:if>
    <xsl:for-each select="ledger:label"><span class="badge label"><xsl:value-of select="."/></span></xsl:for-each>
  </xsl:template>

  <!-- ============================================================ one item -->

  <xsl:template name="item">
    <article id="item" class="item-page">
      <xsl:apply-templates select="view:flash"/>
      <xsl:apply-templates select="rdf:RDF/ledger:Item" mode="card"/>
      <xsl:if test="count(rdf:RDF/view:Link) &gt; 0">
        <section class="panel links" aria-labelledby="links-title">
          <h2 id="links-title">Links</h2>
          <ul class="plain">
            <xsl:apply-templates select="rdf:RDF/view:Link">
              <xsl:sort select="view:order"/>
            </xsl:apply-templates>
          </ul>
        </section>
      </xsl:if>
      <section class="panel comments" aria-labelledby="comments-title">
        <h2 id="comments-title">Comments</h2>
        <xsl:if test="count(rdf:RDF/ledger:Comment) = 0"><p class="empty">No comments yet.</p></xsl:if>
        <xsl:if test="count(rdf:RDF/ledger:Comment) &gt; 0">
          <ol class="plain comment-list">
            <xsl:apply-templates select="rdf:RDF/ledger:Comment">
              <xsl:sort select="dcterms:created"/>
            </xsl:apply-templates>
          </ol>
        </xsl:if>
      </section>
      <xsl:apply-templates select="rdf:RDF/ledger:Item" mode="actions"/>
    </article>
  </xsl:template>

  <xsl:template match="ledger:Item" mode="card">
    <header class="item-head">
      <p class="crumbs">
        <a><xsl:attribute name="href"><xsl:value-of select="view:ledgerHref"/></xsl:attribute><xsl:value-of select="view:ledger"/></a>
        <span class="num"><xsl:value-of select="view:short"/></span>
      </p>
      <h1 class="item-title"><xsl:value-of select="dcterms:title"/></h1>
      <p class="badges"><xsl:call-template name="badges"/></p>
    </header>
    <xsl:if test="ledger:body"><div class="body"><xsl:value-of select="ledger:body"/></div></xsl:if>
    <dl class="meta">
      <dt>Filed</dt><dd><xsl:value-of select="view:created"/><xsl:if test="ledger:author"> by <xsl:value-of select="ledger:author"/></xsl:if></dd>
      <dt>Updated</dt><dd><xsl:value-of select="view:modified"/></dd>
      <xsl:if test="view:kind"><dt>Level</dt><dd><code><xsl:value-of select="view:kind"/></code></dd></xsl:if>
      <xsl:if test="ledger:revision"><dt>Revision</dt><dd><code><xsl:value-of select="ledger:revision"/></code></dd></xsl:if>
      <xsl:if test="ledger:purpose"><dt>Purpose</dt><dd><xsl:value-of select="ledger:purpose"/></dd></xsl:if>
      <dt>IRI</dt><dd><code class="iri"><xsl:value-of select="view:iri"/></code></dd>
    </dl>
  </xsl:template>

  <xsl:template match="view:Link">
    <li>
      <span class="rel"><xsl:value-of select="view:rel"/></span>
      <xsl:choose>
        <xsl:when test="view:href">
          <a><xsl:attribute name="href"><xsl:value-of select="view:href"/></xsl:attribute><xsl:value-of select="view:text"/></a>
        </xsl:when>
        <xsl:otherwise><code class="iri"><xsl:value-of select="view:text"/></code></xsl:otherwise>
      </xsl:choose>
      <xsl:if test="view:canUnlink = 'true'">
        <form class="inline" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
          <input type="hidden" name="_action" value="link"/>
          <input type="hidden" name="_verb" value="Delete"/>
          <input type="hidden" name="_then" value="card"/>
          <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
          <input type="hidden" name="_id"><xsl:attribute name="value"><xsl:value-of select="view:id"/></xsl:attribute></input>
          <input type="hidden" name="item"><xsl:attribute name="value"><xsl:value-of select="view:item"/></xsl:attribute></input>
          <input type="hidden" name="content"><xsl:attribute name="value"><xsl:value-of select="view:target"/></xsl:attribute></input>
          <input type="hidden" name="type"><xsl:attribute name="value"><xsl:value-of select="view:rel"/></xsl:attribute></input>
          <button type="submit" class="quiet small">Unlink</button>
        </form>
      </xsl:if>
    </li>
  </xsl:template>

  <xsl:template match="ledger:Comment">
    <li class="comment">
      <p class="comment-meta">
        <xsl:value-of select="view:created"/>
        <xsl:text> · </xsl:text>
        <xsl:choose>
          <xsl:when test="ledger:author"><xsl:value-of select="ledger:author"/></xsl:when>
          <xsl:otherwise>unattributed</xsl:otherwise>
        </xsl:choose>
      </p>
      <div class="body"><xsl:value-of select="ledger:body"/></div>
    </li>
  </xsl:template>

  <!-- Everything this caller may do to the item: offered only when the grant allows it. -->
  <xsl:template match="ledger:Item" mode="actions">
    <xsl:if test="view:canWrite = 'true'">
      <section class="panel actions" aria-labelledby="act-title">
        <h2 id="act-title">Work on it</h2>

        <form class="stack" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
          <xsl:call-template name="hidden"><xsl:with-param name="action" select="'comment'"/></xsl:call-template>
          <label for="comment-content">Comment</label>
          <textarea id="comment-content" name="content" rows="3" required="required"></textarea>
          <button type="submit">Comment</button>
        </form>

        <div class="row wrap">
          <xsl:choose>
            <xsl:when test="view:status = 'open'">
              <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'close'"/></xsl:call-template>
                <label for="close-reason">Close as</label>
                <select id="close-reason" name="reason">
                  <option value="done">done</option>
                  <option value="wontfix">wontfix</option>
                  <option value="duplicate">duplicate</option>
                  <option value="superseded">superseded</option>
                  <option value="audit-no-change">audit-no-change</option>
                </select>
                <button type="submit">Close</button>
              </form>
            </xsl:when>
            <xsl:otherwise>
              <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'reopen'"/></xsl:call-template>
                <button type="submit">Reopen</button>
              </form>
            </xsl:otherwise>
          </xsl:choose>

          <xsl:choose>
            <xsl:when test="ledger:claimedBy">
              <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'claim'"/><xsl:with-param name="verb" select="'Delete'"/></xsl:call-template>
                <button type="submit" class="quiet">Release claim</button>
              </form>
            </xsl:when>
            <xsl:otherwise>
              <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'claim'"/></xsl:call-template>
                <label for="claim-holder">Claim for</label>
                <input id="claim-holder" type="text" name="content" required="required" autocomplete="off"/>
                <button type="submit" class="quiet">Claim</button>
              </form>
            </xsl:otherwise>
          </xsl:choose>

          <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
            <xsl:choose>
              <xsl:when test="view:deferred = 'true'">
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'defer'"/><xsl:with-param name="verb" select="'Delete'"/></xsl:call-template>
                <button type="submit" class="quiet">Resume</button>
              </xsl:when>
              <xsl:otherwise>
                <xsl:call-template name="hidden"><xsl:with-param name="action" select="'defer'"/></xsl:call-template>
                <button type="submit" class="quiet">Defer</button>
              </xsl:otherwise>
            </xsl:choose>
          </form>
        </div>

        <details>
          <summary>Edit title, body and priority</summary>
          <form class="stack" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
            <input type="hidden" name="_action" value="item"/>
            <input type="hidden" name="_then" value="card"/>
            <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
            <input type="hidden" name="_id"><xsl:attribute name="value"><xsl:value-of select="view:id"/></xsl:attribute></input>
            <label for="edit-content">Title, blank line, body</label>
            <textarea id="edit-content" name="content" rows="6" required="required"><xsl:value-of select="view:content"/></textarea>
            <label for="edit-priority">Priority</label>
            <select id="edit-priority" name="priority">
              <option value="">unchanged</option>
              <option value="0">0 highest</option>
              <option value="1">1</option>
              <option value="2">2</option>
              <option value="3">3</option>
              <option value="4">4 lowest</option>
            </select>
            <button type="submit">Save</button>
          </form>
        </details>

        <div class="row wrap">
          <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
            <xsl:call-template name="hidden"><xsl:with-param name="action" select="'label'"/></xsl:call-template>
            <label for="label-add">Label</label>
            <input id="label-add" type="text" name="content" required="required" autocomplete="off"/>
            <button type="submit" class="quiet">Add</button>
          </form>
          <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
            <xsl:call-template name="hidden"><xsl:with-param name="action" select="'label'"/><xsl:with-param name="verb" select="'Delete'"/></xsl:call-template>
            <label for="label-remove">Remove label</label>
            <input id="label-remove" type="text" name="content" required="required" autocomplete="off"/>
            <button type="submit" class="quiet">Remove</button>
          </form>
          <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
            <xsl:call-template name="hidden"><xsl:with-param name="action" select="'link'"/></xsl:call-template>
            <label for="link-type">Link</label>
            <select id="link-type" name="type">
              <option value="related">related to</option>
              <option value="blocks">blocks</option>
              <option value="parent">has parent</option>
            </select>
            <input type="text" name="content" required="required" placeholder="#12" aria-label="Linked item" autocomplete="off"/>
            <button type="submit" class="quiet">Link</button>
          </form>
        </div>
      </section>
    </xsl:if>

    <xsl:if test="view:canDelete = 'true'">
      <section class="panel danger" aria-labelledby="delete-title">
        <h2 id="delete-title">Delete</h2>
        <p>Moves the item, its comments and the links pointing at it into this ledger's graveyard graph, and leaves a tombstone. Recoverable by hand; it leaves every listing now.</p>
        <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
          <xsl:attribute name="hx-confirm">Delete <xsl:value-of select="view:short"/>? It can be recovered from the graveyard graph.</xsl:attribute>
          <input type="hidden" name="_action" value="item"/>
          <input type="hidden" name="_verb" value="Delete"/>
          <input type="hidden" name="_then" value="gone"/>
          <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
          <input type="hidden" name="_id"><xsl:attribute name="value"><xsl:value-of select="view:id"/></xsl:attribute></input>
          <label for="delete-reason">Reason</label>
          <input id="delete-reason" type="text" name="reason" autocomplete="off"/>
          <button type="submit" class="danger">Delete</button>
        </form>
      </section>
    </xsl:if>

    <xsl:if test="view:canPurge = 'true'">
      <section class="panel danger purge" aria-labelledby="purge-title">
        <h2 id="purge-title">Purge</h2>
        <p>Destroys the content in both the live graph and the graveyard. Only a tombstone remains: the number, the time, the reason, the quad count and a hash of what was destroyed. This is not the same act as Delete and cannot be undone.</p>
        <form class="row" method="post" action="/act" hx-post="/act" hx-target="#item" hx-swap="outerHTML">
          <xsl:attribute name="hx-confirm">Purge <xsl:value-of select="view:short"/> permanently? This cannot be undone.</xsl:attribute>
          <input type="hidden" name="_action" value="purge"/>
          <input type="hidden" name="_verb" value="Delete"/>
          <input type="hidden" name="_then" value="gone"/>
          <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
          <input type="hidden" name="content"><xsl:attribute name="value"><xsl:value-of select="view:iri"/></xsl:attribute></input>
          <label for="purge-reason">Reason</label>
          <input id="purge-reason" type="text" name="reason" autocomplete="off"/>
          <button type="submit" class="danger">Purge permanently</button>
        </form>
      </section>
    </xsl:if>
  </xsl:template>

  <!-- The hidden fields every item form carries. Context: the ledger:Item. -->
  <xsl:template name="hidden">
    <xsl:param name="action"/>
    <xsl:param name="verb" select="'Sink'"/>
    <input type="hidden" name="_action"><xsl:attribute name="value"><xsl:value-of select="$action"/></xsl:attribute></input>
    <input type="hidden" name="_verb"><xsl:attribute name="value"><xsl:value-of select="$verb"/></xsl:attribute></input>
    <input type="hidden" name="_then" value="card"/>
    <input type="hidden" name="_ledger"><xsl:attribute name="value"><xsl:value-of select="view:ledger"/></xsl:attribute></input>
    <input type="hidden" name="_id"><xsl:attribute name="value"><xsl:value-of select="view:id"/></xsl:attribute></input>
    <input type="hidden" name="item"><xsl:attribute name="value"><xsl:value-of select="view:iri"/></xsl:attribute></input>
  </xsl:template>

  <xsl:template name="gone">
    <article id="item" class="item-page">
      <xsl:apply-templates select="view:flash"/>
      <p><a><xsl:attribute name="href"><xsl:value-of select="@page-url"/></xsl:attribute>Back to <xsl:value-of select="@ledger"/></a></p>
    </article>
  </xsl:template>

  <!-- ============================================================ SPARQL -->

  <xsl:template name="sparql">
    <section id="sparql" class="sparql">
      <h1>SPARQL</h1>
      <!-- ★ The limit belongs to THIS PAGE, not to the endpoint (#401). Since ikigai-store
           0.2.5 a scoped read takes a SET of graphs, so the paragraph that read as "the
           endpoint can only do one" was true of every sentence and false as a whole — a
           reader concluded the ledger↔browse join was impossible, which it is not. Keep the
           two halves apart: what this box sends, and what the endpoint accepts. -->
      <p class="hint">Read-only. <strong>This page</strong> runs your query over one ledger's graph, as its whole dataset, under your grant. The endpoint behind it — <code>urn:iki:store:graph-select</code> (or <code>-ask</code>, <code>-construct</code>, <code>-describe</code>) — accepts <em>several</em> graphs in one read, so a caller holding a read token for each can join across them; this page has no graph selector yet. <code>FROM</code> and <code>FROM NAMED</code> are refused either way: the named graphs already are the dataset, and nothing outside them is visible.</p>
      <!--
        The sample queries. A button REPLACES the textarea's contents and runs nothing: the
        person presses Run, so an edit is never lost to a surprise execution. Each query
        travels as the TEXT of a view:sample (never an attribute — an XML parser normalizes
        newlines in attribute values away, and a SPARQL query is nothing without them),
        rendered into a hidden <pre> that web/gonk.js reads by id.
      -->
      <div class="samples" role="group" aria-label="Sample queries">
        <xsl:apply-templates select="view:sample"/>
      </div>
      <p class="hint"><xsl:value-of select="view:hint"/></p>
      <xsl:apply-templates select="view:cross-graph"/>
      <div id="sample-queries" hidden="hidden"><xsl:apply-templates select="view:sample" mode="text"/></div>
      <form class="panel stack" method="get" action="/sparql" hx-get="/sparql/results" hx-target="#results" hx-swap="innerHTML">
        <div class="row">
          <label for="q-ledger">Ledger</label>
          <select id="q-ledger" name="ledger">
            <xsl:apply-templates select="view:ledger" mode="option"/>
          </select>
        </div>
        <label for="q">Query</label>
        <textarea id="q" name="query" rows="12" spellcheck="false" class="mono"><xsl:value-of select="view:query"/></textarea>
        <button type="submit">Run</button>
      </form>
      <div id="results" class="results" aria-live="polite">
        <xsl:apply-templates select="view:results"/>
      </div>
    </section>
  </xsl:template>

  <!-- A sample button is a TOGGLE, not a link: `aria-pressed` says whether the editor holds
       this sample's query. The server renders every one as `false` — a freshly served page
       holds the default query, never a sample — and web/gonk.js moves the `true` on a click
       and clears it on the first keystroke in the editor (#370). -->
  <xsl:template match="view:sample">
    <button type="button" class="sample quiet small" aria-pressed="false">
      <xsl:attribute name="data-query"><xsl:value-of select="@id"/></xsl:attribute>
      <xsl:value-of select="@label"/>
    </button>
  </xsl:template>

  <xsl:template match="view:sample" mode="text">
    <pre class="sample-text"><xsl:attribute name="id"><xsl:value-of select="@id"/></xsl:attribute><xsl:value-of select="."/></pre>
  </xsl:template>

  <!-- ★ The cross-graph example: SHOWN, never loadable. It is a `details` (the same idiom the
       item form already uses) and NOT a `.sample` button — no `data-query`, no id under
       `#sample-queries` — because web/gonk.js loads a sample by looking its `data-query` up as
       an element id, and this page sends ONE graph: a multi-graph query run from the box would
       come back empty and present a working capability as a broken feature. See
       web::CROSS_GRAPH. -->
  <xsl:template match="view:cross-graph">
    <details class="cross-graph">
      <summary>Joining two graphs — a query for the endpoint, not for this box</summary>
      <p class="hint"><xsl:value-of select="@why"/></p>
      <pre class="mono cross-graph-text"><xsl:value-of select="."/></pre>
    </details>
  </xsl:template>

  <xsl:template match="view:results">
    <xsl:apply-templates select="view:error"/>
    <xsl:if test="@summary"><p class="result-meta"><xsl:value-of select="@summary"/></p></xsl:if>
    <!-- The prefix legend: once per result set, as TEXT, so a shortened IRI is reachable
         without a tooltip. See web::PREFIX_LEGEND. -->
    <xsl:if test="view:prefix"><p class="result-meta legend"><xsl:value-of select="view:prefix"/></p></xsl:if>
    <xsl:apply-templates select="view:table"/>
    <xsl:if test="view:boolean"><p class="boolean"><xsl:value-of select="view:boolean"/></p></xsl:if>
    <xsl:if test="view:graph"><pre class="mono graph"><xsl:value-of select="view:graph"/></pre></xsl:if>
  </xsl:template>

  <xsl:template match="view:error">
    <p class="flash error" role="alert"><xsl:value-of select="."/></p>
  </xsl:template>

  <xsl:template match="view:table">
    <div class="table-wrap">
      <table>
        <thead>
          <tr><xsl:for-each select="view:col"><th scope="col"><xsl:value-of select="."/></th></xsl:for-each></tr>
        </thead>
        <tbody>
          <xsl:for-each select="view:row">
            <tr>
              <xsl:for-each select="view:cell">
                <td><xsl:attribute name="class"><xsl:value-of select="@kind"/></xsl:attribute><xsl:if test="@title"><xsl:attribute name="title"><xsl:value-of select="@title"/></xsl:attribute></xsl:if><xsl:choose><xsl:when test="@href"><a><xsl:attribute name="href"><xsl:value-of select="@href"/></xsl:attribute><xsl:attribute name="aria-label"><xsl:value-of select="@label"/></xsl:attribute><xsl:attribute name="class">cell-link</xsl:attribute><xsl:attribute name="data-action"><xsl:value-of select="@action"/></xsl:attribute><xsl:value-of select="."/></a></xsl:when><xsl:otherwise><xsl:value-of select="."/></xsl:otherwise></xsl:choose></td>
              </xsl:for-each>
            </tr>
          </xsl:for-each>
        </tbody>
      </table>
    </div>
  </xsl:template>
</xsl:stylesheet>
