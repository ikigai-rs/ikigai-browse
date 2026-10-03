;;; ikigai.el --- A thin Emacs client for the ikigai kernel -*- lexical-binding: t; -*-

;; A command surface over the ikigai kernel: evaluate resources from a buffer, and
;; (first use case) schedule a Zoom for an org heading that doesn't have one. It shells
;; to the `ikigai' CLI and nothing heavier — a bounded front-end, not a fork of anything.
;;
;; Load it and bind what you like, e.g.:
;;   (with-eval-after-load 'org
;;     (define-key org-mode-map (kbd "C-c z") #'ikigai-org-schedule-zoom))
;;   (global-set-key (kbd "C-c i e") #'ikigai-eval-dwim)

;;; Code:

(require 'org)
(require 'org-element)
(require 'json)
(require 'subr-x)
(require 'comint)
(require 'ansi-color)

(defgroup ikigai nil "Talk to the ikigai kernel from Emacs." :group 'tools)

(defcustom ikigai-program "ikigai"
  "The ikigai CLI executable (found on `exec-path')."
  :type 'string :group 'ikigai)

(defcustom ikigai-connect nil
  "If non-nil, an IPC socket path of a running `ikigai serve' host to talk to.
When set, `ikigai-eval'/`ikigai-repl' prepend `--connect PATH' and do NOT pass
`--mount' — a connected host owns its own mounts.  So a machine's transport and
topology are a property of that host, and this Emacs config stays identical
across machines.  When nil, commands run embedded (a cold kernel) composing
`ikigai-mounts'."
  :type '(choice (const :tag "Embedded (use ikigai-mounts)" nil)
                 (file :tag "IPC socket of a serve host"))
  :group 'ikigai)

(defcustom ikigai-default-meeting-minutes 30
  "Default meeting length when a heading's timestamp carries no end time."
  :type 'integer :group 'ikigai)

(defcustom ikigai-mounts nil
  "Remote kernels to compose into every `ikigai-eval' call.
A list of plists, each `(:prefix PFX :target TGT :cert-dir DIR :alias ALIAS
:mode MODE)'.  TGT may be `peer:NAME' to find that peer on the local network by
mDNS instead of naming an address.  MODE is nil/`alias' (default), `override' or
`prefer' — see `ikigai--mount-flag'.
Routing a prefix to a daemon that holds the resource lets Emacs reach it
through that daemon's grants and cache instead of the (grant-less, cold)
process Emacs spawns.  On macOS calendar access is pinned to the launching
app, so `urn:personal:*' must resolve through the freebusy daemon.

PFX is the canonical prefix you write in buffers.  TARGET/CERT-DIR become
`--mount …=TARGET --cert-dir DIR'.

ALIAS handles the collision case: a mount rewrites `<mount-prefix>rest' to
`urn:rest' on the remote, so a mount cannot win a prefix the LOCAL kernel
already serves (it does serve `urn:personal:').  Set ALIAS to a wire prefix
the local kernel does NOT serve; the remote is mounted there and this client
rewrites PFX to ALIAS on the wire, so you keep writing canonical PFX.
Errors will echo the ALIAS form — that is the wire address.  (When the
kernel grows override-mounts this field goes away.)  Example:

  (setq ikigai-mounts
        \\='((:prefix \"urn:personal:\" :alias \"urn:cal:\"
            :target \"quic://localhost:4444\"
            :cert-dir \"~/.config/ikigai/quic/calendar\")))"
  :type '(repeat
          (plist :key-type symbol :value-type string
                 :options ((:prefix string) (:alias string)
                           (:target string) (:cert-dir directory))))
  :group 'ikigai)

;;; --- the wrapper: call the kernel -------------------------------------------

(defun ikigai--mount-flag (mount)
  "The CLI flag for MOUNT: `--mount', `--override' or `--prefer'.

`:mode' says how the mount relates to the local namespace, matching the CLI:

  nil / `alias'  the prefix is a local ALIAS for a remote namespace (rewritten,
                 tried AFTER local) — the default, and what `:alias' is for.
  `override'     the SAME namespace served remotely: IRIs forward unchanged and
                 win over local.  If the peer is unreachable the call FAILS —
                 you asked for that machine.
  `prefer'       like `override', but falls back to the LOCAL binding when the
                 peer is unreachable: use plasma's inference when plasma is
                 around, this machine when it is not."
  (pcase (plist-get mount :mode)
    ('override "--override")
    ('prefer "--prefer")
    (_ "--mount")))

(defun ikigai--mount-args ()
  "Build the mount flags from `ikigai-mounts'.
An alias mount is bound at its :alias when set (so it does not collide with a
locally-served :prefix), else at :prefix.  An override/prefer mount always uses
:prefix — forwarding IRIs unchanged is the point of it, so rewriting them would
defeat it."
  (apply #'append
         (mapcar (lambda (m)
                   (let* ((flag (ikigai--mount-flag m))
                          (alias (plist-get m :alias))
                          (at (if (string= flag "--mount")
                                  (or alias (plist-get m :prefix))
                                (plist-get m :prefix))))
                     (append
                      (list flag (format "%s=%s" at (plist-get m :target)))
                      (when-let ((dir (plist-get m :cert-dir)))
                        (list "--cert-dir" (expand-file-name dir))))))
                 ikigai-mounts)))

(defun ikigai--transport-args ()
  "Transport flags: `--connect' to a host when `ikigai-connect' is set, else the
embedded `--mount' flags from `ikigai-mounts'.  A connected host owns its mounts,
so the two are mutually exclusive by design."
  (if ikigai-connect
      (list "--connect" (expand-file-name ikigai-connect))
    (ikigai--mount-args)))

(defun ikigai--apply-aliases (command)
  "Rewrite each aliased mount's canonical :prefix to its wire :alias in COMMAND.
The remote maps `<alias>rest'->`urn:rest', so canonical `urn:personal:X' must
go out as `<alias>personal:X' — i.e. :prefix with its `urn:' swapped for :alias."
  (dolist (m ikigai-mounts command)
    (let ((prefix (plist-get m :prefix))
          (alias (plist-get m :alias)))
      (when (and prefix alias (string-prefix-p "urn:" prefix))
        (setq command
              (replace-regexp-in-string
               (regexp-quote prefix)
               (concat alias (substring prefix (length "urn:")))
               command t t))))))

(defun ikigai--quote (s)
  "Escape S as an ikigai/Steel string literal (quotes and backslashes)."
  (concat "\"" (replace-regexp-in-string "[\"\\]" "\\\\\\&" s) "\""))

(defun ikigai-eval (command)
  "Run ikigai COMMAND (a resolution string or a Lisp form) via `ikigai -c'.
Return trimmed stdout on success; signal `user-error' with stderr on failure.
Cache tags (`[uncacheable]' etc.) go to stderr, so stdout stays clean."
  (let ((errfile (make-temp-file "ikigai-err"))
        (command (ikigai--apply-aliases command)))
    (unwind-protect
        (with-temp-buffer
          (let ((status (apply #'call-process ikigai-program nil
                               (list (current-buffer) errfile) nil
                               (append (ikigai--transport-args) (list "-c" command)))))
            (if (eq status 0)
                (string-trim (buffer-string))
              (user-error "ikigai: %s"
                          (string-trim
                           (with-temp-buffer
                             (insert-file-contents errfile)
                             (buffer-string)))))))
      (delete-file errfile))))

(defun ikigai-invoke (verb uri &rest name-value)
  "Invoke URI with VERB (a symbol, e.g. `sink') and NAME-VALUE string pairs.
Builds `(invoke (quote VERB) \"URI\" \"n\" \"v\" ...)' so values with spaces are safe."
  (ikigai-eval
   (format "(invoke '%s %s %s)"
           verb (ikigai--quote uri)
           (mapconcat #'ikigai--quote name-value " "))))

;;; --- evaluate a resource reference from a buffer ----------------------------

(defun ikigai-eval-dwim (arg)
  "Evaluate the region (or the current line) as an ikigai command; show the result.
The result is also pushed onto the kill ring, so \\[yank] pastes it anywhere.
With a prefix ARG, insert the result into the buffer after point as well."
  (interactive "P")
  (let* ((cmd (string-trim
               (if (use-region-p)
                   (buffer-substring-no-properties (region-beginning) (region-end))
                 (thing-at-point 'line t))))
         (out (ikigai-eval cmd)))
    (kill-new out)
    (when arg
      (save-excursion (end-of-line) (insert "\n" out)))
    (message "%s%s" out (if arg "" "  (copied)"))
    out))

;;; --- meeting scheduling -----------------------------------------------------

(defun ikigai-schedule-zoom (topic start &optional minutes timezone)
  "Schedule a Zoom via `urn:meeting:zoom:schedule'.
TOPIC and START (ISO 8601, e.g. \"2026-07-30T22:00:00Z\") are required.
Return an alist parsed from the JSON envelope: `id', `join_url', `start_url',
`passcode', `start_time', `duration'."
  (let* ((args (append (list "topic" topic "start" start "as" "application/json")
                       (when minutes (list "duration" (number-to-string minutes)))
                       (when timezone (list "timezone" timezone))))
         (json (apply #'ikigai-invoke 'sink "urn:meeting:zoom:schedule" args)))
    (json-parse-string json :object-type 'alist :null-object nil)))

;;; --- the org command --------------------------------------------------------

(defun ikigai--org-when ()
  "Return (ISO-START . MINUTES) for the org entry at point.
ISO-START is UTC ISO-8601 from the heading's timestamp (or SCHEDULED).
MINUTES is the timestamp's own duration when it is a range (e.g.
12:00-13:00 yields 60), else nil so the caller supplies a default.
Signals if the heading has no timestamp."
  (let ((ts (or (org-entry-get (point) "TIMESTAMP")
                (org-entry-get (point) "SCHEDULED"))))
    (unless ts (user-error "This heading has no timestamp to meet at"))
    (let* ((start (org-time-string-to-time ts))
           (obj (ignore-errors (org-timestamp-from-string ts)))
           (h1 (and obj (org-element-property :hour-start obj)))
           (m1 (and obj (org-element-property :minute-start obj)))
           (h2 (and obj (org-element-property :hour-end obj)))
           (m2 (and obj (org-element-property :minute-end obj)))
           ;; A range only when an end time exists AND differs from the start.
           (minutes (when (and h1 m1 h2 m2 (or (/= h1 h2) (/= m1 m2)))
                      (let ((d (- (+ (* h2 60) m2) (+ (* h1 60) m1))))
                        ;; A range that crosses midnight (end < start) wraps a day.
                        (if (<= d 0) (+ d 1440) d)))))
      (cons (format-time-string "%Y-%m-%dT%H:%M:%SZ" start t) minutes))))

(defun ikigai--org-attendees ()
  "The heading's `:ATTENDEES:' property, prompting (and storing) if absent.
Returns a possibly-empty string of comma-separated addresses."
  (or (org-entry-get (point) "ATTENDEES")
      (let ((a (string-trim (read-string "Attendee emails (comma-separated, RET to skip): "))))
        (unless (string-empty-p a) (org-entry-put (point) "ATTENDEES" a))
        a)))

;;;###autoload
(defun ikigai-org-schedule-zoom (&optional minutes)
  "Schedule a Zoom for the org heading at point and file the join link.
Reads the heading title (topic) and its timestamp (start), and the
`:ATTENDEES:' property (prompting for emails if absent). Files the join
URL into the `:URL:' drawer (which ikigai-org emits as `ical:description',
so it rides to Brian-Busy) and the passcode into `:ZOOM_PASSCODE:'.
With a prefix arg, prompt for MINUTES.

Attendees are RECORDED here; email them a calendar invite with
`ikigai-org-email-invite' once the Zoom is scheduled."
  (interactive (list (when current-prefix-arg
                       (read-number "Minutes: " ikigai-default-meeting-minutes))))
  (save-excursion
    (org-back-to-heading t)
    (let* ((topic (org-get-heading t t t t))
           (sched (ikigai--org-when))
           (start (car sched))
           ;; A prefix arg wins; else the timestamp's own range; else the default.
           (mins (or minutes (cdr sched) ikigai-default-meeting-minutes))
           (attendees (ikigai--org-attendees))
           (meeting (ikigai-schedule-zoom topic start mins))
           (join (alist-get 'join_url meeting))
           (pass (alist-get 'passcode meeting)))
      (unless (and join (stringp join) (not (string-empty-p join)))
        (user-error "Zoom returned no join URL: %S" meeting))
      (org-entry-put (point) "URL" join)
      (when (and pass (stringp pass) (not (string-empty-p pass)))
        (org-entry-put (point) "ZOOM_PASSCODE" pass))
      (message "Zoom scheduled: %s%s" join
               (if (string-empty-p attendees) ""
                 (format "  (attendees noted — M-x ikigai-org-email-invite to send: %s)" attendees)))
      join)))

;;; --- email a calendar invite (iMIP) to the attendees -----------------------

(defcustom ikigai-organizer-email "brian@bosatsu.net"
  "The ORGANIZER address on invites this client sends."
  :type 'string :group 'ikigai)

(defcustom ikigai-organizer-name "Brian Sletten"
  "The ORGANIZER display name on invites this client sends."
  :type 'string :group 'ikigai)

(defun ikigai--ics-escape (s)
  "Escape S for an iCalendar TEXT value (RFC 5545 §3.3.11): \\ , ; and newlines."
  (replace-regexp-in-string
   "\n" "\\\\n"
   (replace-regexp-in-string "[\\,;]" "\\\\\\&" (or s ""))))

(defun ikigai--ics-fold (line)
  "Fold LINE to <=75 chars per RFC 5545 §3.1; continuations begin with a space.
Char-based, which is octet-exact for the ASCII URLs/emails/text here and never
splits a codepoint; any conformant parser rejoins the CRLF-space continuations."
  (if (<= (length line) 75) line
    (let ((parts nil) (i 0) (n (length line)) (limit 75))
      (while (< i n)
        (let ((end (min n (+ i limit))))
          (push (substring line i end) parts)
          (setq i end limit 74)))          ; continuations carry a leading space → 74 content
      (mapconcat #'identity (nreverse parts) "\r\n "))))

(defun ikigai--ics-invite (uid topic start minutes join pass attendees)
  "Build an RFC 5545 VCALENDAR (METHOD:REQUEST) for the meeting.
START is UTC ISO-8601; MINUTES the duration; ATTENDEES a list of addresses.
The Zoom link rides as both LOCATION/URL and an RFC 7986 CONFERENCE property."
  (let* ((start-time (date-to-time start))
         (fmt (lambda (tm) (format-time-string "%Y%m%dT%H%M%SZ" tm t)))
         (desc (concat "Join Zoom: " join
                       (if (and pass (not (string-empty-p pass)))
                           (concat "\nPasscode: " pass) "")))
         (lines (append
                 (list "BEGIN:VCALENDAR"
                       "VERSION:2.0"
                       "PRODID:-//ikigai-emacs//meeting//EN"
                       "METHOD:REQUEST"
                       "BEGIN:VEVENT"
                       (concat "UID:" uid)
                       (concat "DTSTAMP:" (funcall fmt nil))
                       (concat "DTSTART:" (funcall fmt start-time))
                       (concat "DTEND:" (funcall fmt (time-add start-time (* minutes 60))))
                       (concat "SUMMARY:" (ikigai--ics-escape topic))
                       (concat "DESCRIPTION:" (ikigai--ics-escape desc))
                       (concat "LOCATION:" (ikigai--ics-escape join))
                       (concat "URL:" join)
                       (concat "ORGANIZER;CN=" ikigai-organizer-name
                               ":mailto:" ikigai-organizer-email)
                       (concat "CONFERENCE;VALUE=URI;FEATURE=VIDEO;LABEL=Zoom Meeting:" join))
                 (mapcar (lambda (a)
                           (concat "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;"
                                   "RSVP=TRUE:mailto:" a))
                         attendees)
                 (list "END:VEVENT" "END:VCALENDAR"))))
    (mapconcat #'ikigai--ics-fold lines "\r\n")))

(defun ikigai--invite-body (topic start join pass)
  "Plain-text alternative body for the invite (clients that ignore the .ics)."
  (concat topic "\n\n"
          "When: " start " (UTC)\n"
          "Join Zoom: " join "\n"
          (if (and pass (not (string-empty-p pass))) (concat "Passcode: " pass "\n") "")
          "\nA calendar invitation is attached — accept it to add this to your calendar.\n"))

;;;###autoload
(defun ikigai-org-email-invite ()
  "Email the org entry's `:ATTENDEES:' an iMIP calendar invite for its Zoom.
The entry must already carry a `:URL:' join link (from
`ikigai-org-schedule-zoom'). Sends one METHOD:REQUEST invite per attendee
via `urn:email:send', carrying the VCALENDAR as the `ics' argument so a mail
client shows an add-to-calendar / RSVP card.

Delivery needs a reachable mailer: if THIS machine has no local SMTP, route
`urn:email:' to one that does via `ikigai-mounts' (same pattern as the
calendar mount). The event UID is the entry's org `:ID:' (created if absent),
so re-sending updates the same event rather than duplicating it."
  (interactive)
  (save-excursion
    (org-back-to-heading t)
    (let* ((topic (org-get-heading t t t t))
           (sched (ikigai--org-when))
           (start (car sched))
           (minutes (or (cdr sched) ikigai-default-meeting-minutes))
           (join (org-entry-get (point) "URL"))
           (pass (org-entry-get (point) "ZOOM_PASSCODE"))
           (attendees (split-string (or (org-entry-get (point) "ATTENDEES") "") "[, ]+" t)))
      (unless (and join (not (string-empty-p join)))
        (user-error "No Zoom link in :URL: — run ikigai-org-schedule-zoom first"))
      (unless attendees
        (user-error "No :ATTENDEES: on this heading to invite"))
      (when (yes-or-no-p (format "Email an invite to %d attendee(s): %s? "
                                 (length attendees) (string-join attendees ", ")))
        (let ((ics (ikigai--ics-invite (org-id-get-create) topic start minutes join pass attendees))
              (body (ikigai--invite-body topic start join pass))
              (subject (concat "Invitation: " topic)))
          (dolist (addr attendees)
            (ikigai-invoke 'sink "urn:email:send"
                           "to" addr "subject" subject "content" body "ics" ics))
          (message "Invite sent to %d attendee(s): %s"
                   (length attendees) (string-join attendees ", ")))))))

(defun ikigai--form-before-point ()
  "Read the Elisp form ending at point."
  (save-excursion
    (let ((end (point)))
      (backward-sexp)
      (car (read-from-string (buffer-substring-no-properties (point) end))))))

(defun ikigai-eval-elisp-dwim (arg)
  "Evaluate the Elisp form before point (or the region) and COPY the result.

The companion to `ikigai-eval-dwim' for the generated aliases: where that one
takes an ikigai command string, this takes a call like
`(ikigai-llm-ask \"summarise this\")'.  The result goes on the kill ring, so
\[yank] pastes it — rather than being truncated in the echo area, which is what
happens to anything long enough to be worth keeping.

With a prefix ARG, also insert the result after point.  A string result is
inserted verbatim, not as a quoted Elisp literal, since the point is usually the
text itself."
  (interactive "P")
  (let* ((form (if (use-region-p)
                   (car (read-from-string
                         (buffer-substring-no-properties
                          (region-beginning) (region-end))))
                 (ikigai--form-before-point)))
         (value (eval form t))
         (out (if (stringp value) value (format "%S" value))))
    (kill-new out)
    (when arg
      (save-excursion (insert "\n" out)))
    (message "%s%s" out (if arg "" "  (copied)"))
    out))

;;; --- generated aliases (named verbs over the manifold) ----------------------

(defcustom ikigai-aliases-file (locate-user-emacs-file "ikigai-aliases.el")
  "Where `ikigai-refresh-aliases' writes the generated alias definitions."
  :type 'file :group 'ikigai)

(defun ikigai-refresh-aliases ()
  "Regenerate elisp aliases from the kernel's manifold, then load them.

Writes one function per resource THIS capability may invoke, with the
arguments that resource declares: `(ikigai-fn-toUpper \"hi\")' rather than a
hand-written wrapper.  They are GENERATED from the live manifold, so they
cannot drift from what the kernel accepts, and a newly bound endpoint gets a
function the next time you run this.

The set reflects whichever kernel `ikigai-connect' / `ikigai-mounts' point at,
so running this after changing transport gives you that host's surface.
Requires `urn:cap:kernel:inspect': reading the manifold is inspection."
  (interactive)
  (let ((elisp (ikigai-eval "source urn:lisp:aliases as=text/x-emacs-lisp")))
    (with-temp-file ikigai-aliases-file
      (insert elisp)
      ;; A trailing newline, so the file is a well-formed text file whatever the
      ;; representation happened to end with.
      (unless (string-suffix-p "\n" elisp)
        (insert "\n")))
    (load ikigai-aliases-file nil t)
    (message "ikigai: %d aliases loaded from %s"
             (with-temp-buffer
               (insert elisp)
               (count-matches "^(defun " (point-min) (point-max)))
             (abbreviate-file-name ikigai-aliases-file))))

;;; --- interactive REPL (comint over `ikigai --plain') ------------------------

(defvar ikigai-repl-prompt-regexp "^ikigai> "
  "Regexp matching the prompt the `ikigai --plain' line REPL prints.")

(defcustom ikigai-repl-buffer-name "*ikigai*"
  "Buffer name for `ikigai-repl'."
  :type 'string :group 'ikigai)

(define-derived-mode ikigai-repl-mode comint-mode "ikigai"
  "Major mode for an interactive ikigai REPL, comint over `ikigai --plain'.
The plain line REPL reads whole lines from stdin (no in-process line editor),
so Emacs does the editing and history; `\\[comint-previous-input]' /
`\\[comint-next-input]' cycle input as usual."
  (setq-local comint-prompt-regexp ikigai-repl-prompt-regexp)
  (setq-local comint-prompt-read-only t)
  ;; Shaken out empirically (batch comint against `ikigai --plain'): Emacs's pty
  ;; does NOT echo this process's input, so comint's own inserted copy is the only
  ;; one — no double-echo. `nil` is the default; set explicitly to record that it
  ;; was checked, and because `t` here would risk clipping output that happened to
  ;; look like the input.
  (setq-local comint-process-echoes nil)
  (setq-local comint-use-prompt-regexp nil)  ; field-based prompt handling
  ;; The `clear` command and any coloured output arrive as ANSI; render it.
  (add-hook 'comint-output-filter-functions #'ansi-color-process-output nil t))

;;;###autoload
(defun ikigai-repl ()
  "Open (or switch to) an interactive ikigai REPL buffer.
Runs `ikigai --plain' under comint, inheriting any `ikigai-mounts' so the
REPL resolves the same composed resources your one-shot commands do."
  (interactive)
  (let ((buf (get-buffer-create ikigai-repl-buffer-name)))
    (unless (comint-check-proc buf)
      (apply #'make-comint-in-buffer "ikigai" buf ikigai-program nil
             (append (ikigai--transport-args) (list "--plain")))
      (with-current-buffer buf (ikigai-repl-mode)))
    (pop-to-buffer buf)))

(provide 'ikigai)
;;; ikigai.el ends here
