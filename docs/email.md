# XyScript — Email (`--features email`)

Send and receive email from XyScript by shelling out to `curl`, behind the `email` cargo feature.
As of 2026-07-13 the standard `dist/xyscript.exe` is **built with `email`**, and email is a **first-class
feature**: the verbs and `$`-specials are in `BUILTIN_VERBS`/`SPECIAL_NAMES`, documented in
`docs/reference.typ` (both command listings + the special-variables section) and `docs/manual.typ` (the
_Email_ chapter), and reachable through `HELP` and the "did you mean" suggester. The `email` cargo
feature still exists, so a clean build without email is a one-flag change (drop `email` from the build
command); a build without it lists the verbs but reports "email support is not compiled in this build"
if you run one. (The names live in the shared lists unconditionally — they pull in no email code — so a
slim build documents them honestly while the implementation stays cfg-gated.) The menu bar has an
**Email** submenu under **Tools** (list / save / delete / send / attach / verbose-logging).

## Build

```
cargo build --release --features lint,email --manifest-path xyscript-gui/Cargo.toml
```

Copy the result to `dist/xyscript.exe`. (Omit `email` from the feature list for a mail-free build.)

**Prerequisite:** `curl` must be on your `PATH` (ship-standard on Windows 10+, macOS, and Linux).

## Configure your account (session-only)

Four setters; each bare form reports the current value (the password is only ever reported as
`set`/`not set`, never echoed):

| Verb | Sets | Read back |
|------|------|-----------|
| `SMTPURL <url>` | the SMTP submission URL, e.g. `smtps://smtp.gmail.com:465` | `$SMTPURL` |
| `MAILUSER <user>` | SMTP auth username (usually your address) | `$MAILUSER` |
| `MAILFROM <addr>` | the `From:` address (defaults to `MAILUSER`); a display-name form is fine — `MAILFROM "Carl Distefano" <me@example.com>` | `$MAILFROM` |
| `MAILPASS <pw>` | SMTP password / app password (session only) | `$MAILPASS` → `set`/`not set` |
| `IMAPURL <url>` | the IMAP base URL for receiving, e.g. `imaps://imap.fastmail.com:993` | `$IMAPURL` |
| `MAILBOX <name>` | mailbox/folder to read (default `INBOX`) | `$MAILBOX` |

Receiving (see **Receive** below) reuses `MAILUSER` / `MAILPASS` for authentication — you only add
`IMAPURL` (and optionally `MAILBOX`). Nothing here is persisted. Put the non-secret three in your `startup.xpl`:

```
SMTPURL smtps://smtp.fastmail.com:465
MAILUSER me@fastmail.com
MAILFROM me@fastmail.com
```

For the password, either run `MAILPASS <pw>` each session, or set the environment variable
`XYSCRIPT_MAILPASS` before launching — if `MAILPASS` is unset, `SENDMAIL` falls back to it. The
password is fed to curl through a config on **stdin** (`curl -K -`), so it never appears in the process
command line.

### Provider notes

- **Gmail / Google Workspace:** basic passwords are disabled — create an **app password** (Account →
  Security → 2-Step Verification → App passwords) and use it as `MAILPASS`. `SMTPURL
  smtps://smtp.gmail.com:465`.
- **Outlook / Microsoft 365:** needs OAuth2 in most tenants; app passwords work only where legacy auth
  is still allowed. Easiest to test against a provider that allows app passwords.
- **Fastmail / mailbox.org / most ISP & corporate IMAP:** an app password over `smtps://…:465` (or
  `smtp://…:587` with STARTTLS) works directly.

## Send

The **active document is the body.** Two ways to supply headers:

**1. Arguments** — recipient then an optional quoted subject; the whole buffer is the body:

```
SENDMAIL alice@example.com "Lunch Thursday?"
```

**2. A header block** — with no arguments, `SENDMAIL` reads leading `To:` / `Cc:` / `Bcc:` /
`Subject:` / `From:` lines from the top of the buffer, up to the first blank line; everything after the
blank line is the body:

```
To: alice@example.com, bob@example.com
Cc: carol@example.com
Subject: Lunch Thursday?

Anyone free around noon?
```

Then just `SENDMAIL`.

Multiple recipients are comma-separated. `Bcc:` recipients receive the mail but are not written into
the message headers.

Each recipient may be a bare address (`john@everyman.org`) or the display-name form
(`"John Q. Public" <john@everyman.org>`) — the full form is kept in the visible `To:`/`Cc:` header,
while the SMTP envelope uses just the address inside `<…>`. A comma inside a quoted display name
(`"Public, John" <j@x.org>`) does not split the address.

### Custom headers

Any *other* valid header line in the block is passed straight through into the message — `Reply-To`,
`In-Reply-To`, `References`, `Importance`, `X-…`, and so on:

```
To: alice@example.com
Subject: Re: your note
Reply-To: team@example.com
In-Reply-To: <abc123@example.com>
X-Campaign: spring

Thanks — replying to the thread.
```

Rules and guardrails:

- The header block runs to the **first blank line** (or the first line that isn't a valid header). A
  valid header field name is a single token — no spaces — before the colon. So `Reply-To: x` is a
  header, but a prose line like `Note: see below` with a space in front of the colon isn't. (A
  single-word line like `Note: hi` as the *first body line* would be read as a header, so keep the
  blank line before the body — the normal email convention.)
- **`Content-Type`, `Content-Transfer-Encoding`, and `MIME-Version` can't be set** — XyScript computes
  these to match the body and any attachments; user copies are dropped to avoid a broken message.
- **`Date` and `Message-ID`, if you supply them, replace** XyScript's generated ones (rather than
  producing a duplicate). Everything else is appended in the order you wrote it.

**3. From a file** — `SENDMAIL/F <file> [<to> ["subject"]]` reads the message from `<file>` instead of
the active document, so you can send without opening it:

```
SENDMAIL/F "C:\notes\draft.txt" alice@example.com "Draft"
```

The file is the message source exactly like the active buffer would be — so it can be a plain body
(with `<to>`/`"subject"` supplied on the line) or carry its own `To:`/`Subject:`/`Attach:` header block.
Quote the path if it contains spaces. (The file path is an argument, not a `:`-value, because the
command parser folds and splits the verb token — a path can't survive there.)

## Attachments

Stage files with `ATTACH`, then send — they ride along on the next `SENDMAIL` (in either the argument
or header-block mode):

```
ATTACH C:\reports\q3.pdf
ATTACH C:\images\chart.png
SENDMAIL alice@example.com "Q3 numbers"
```

- `ATTACH <path>` stages one file (resolved against `$CWD`; a missing file errors and stages nothing).
- Bare `ATTACH` lists what's staged; `$ATTACH` reads the same list (comma-separated filenames).
- `CLEARATTACH` empties the list.
- The staged list is **consumed by the send** — after `SENDMAIL` fires it's cleared automatically, so
  the next message starts clean (no accidental carry-over).

**Or attach right in the compose document.** In header-block mode, add `Attach:` lines to the header
block (one file each; `Attachment:` also works) and they're attached when you `SENDMAIL`:

```
To: alice@example.com
Subject: Q3 numbers
Attach: C:\reports\q3.pdf
Attach: C:\images\chart.png

Here are the figures.
```

Paths resolve against `$CWD`, and a file that can't be read makes the whole send fail with a clear
error. `Attach:` header files and `ATTACH`-staged files are combined (staged first), so you can mix
both. (`Attach:` only applies to the header-block mode — with `SENDMAIL <to> "subject"` args, use
`ATTACH`.)

With attachments the message becomes `multipart/mixed`: your document is the text part, each file a
base64 part with a `Content-Type` guessed from its extension (`application/octet-stream` if unknown).
There's no artificial size cap — but the SMTP server's message-size limit applies, and curl will report
a non-zero `$RC` if the server rejects an over-large message.

## Result

`SENDMAIL` runs curl as a **background job** (it never blocks the editor):
- Progress and any curl error stream into the **\*Shell Output\*** tab.
- On completion, **`$RC`** is curl's exit code — `0` means sent. `$XO` holds curl's output.
- `$LJ` is the job id, so a script can `WAIT/J $LJ` and then check `$RC`.

## Receive

Receiving is deliberately **minimal and raw** — three verbs over IMAP (via curl), no MIME decoding.
All three run as background curl jobs streaming to the **\*Shell Output\*** tab, authenticate with
`MAILUSER` / `MAILPASS`, and read the mailbox named by `MAILBOX` (default `INBOX`) on the server at
`IMAPURL`. Set those first:

```
IMAPURL imaps://imap.fastmail.com:993
MAILUSER me@fastmail.com
MAILPASS <app-password>     ; or set XYSCRIPT_MAILPASS in the environment
```

**Provider notes:** use `imaps://…:993` (implicit TLS, the norm). The same app-password advice as
sending applies (Gmail/Workspace need an app password; Outlook usually needs OAuth2).

### `LISTMAIL` — list message headers

Fetches each message's Date/From/Subject and prints a clean numbered listing to **\*Shell Output\***, so
you can see who sent what and pick the numbers you want. Message numbers are IMAP sequence numbers
(`1` = oldest). No arguments.

```
LISTMAIL
```

```
2 message(s) in INBOX:

[1] From: Carl Distefano <cldistefano@att.net>
    Date: Mon, 13 Jul 2026 07:26:07 +0000 (UTC)
    Subj: Fw: Crash with five injured in New York
[2] From: …
```

With **`MAILDEBUG on`**, `LISTMAIL` instead streams the raw verbose IMAP exchange (the full server
dialogue) rather than the parsed listing — useful if the listing comes back empty and you need to see
why. (Internally `LISTMAIL` always runs curl verbose, because a custom IMAP `FETCH` surfaces only on
curl's protocol stream, never stdout; the parsed listing is built from that trace.)

### `SAVEMAIL <range> [<path>]` — download raw messages

Downloads the **raw** RFC-822 source of each message in `<range>` to disk, one message per file, named
`MSG001.TXT`, `MSG002.TXT`, … by message number (so `SAVEMAIL 5` writes `MSG005.TXT`). No decoding —
exactly what the server sent. The optional `<path>` is a directory (created if needed); without it the
files land in `$CWD`.

```
SAVEMAIL 1-3,6-8,10,12  C:\mail\archive
SAVEMAIL all  C:\mail\archive
```

`<range>` is a comma list of numbers and `first-last` spans (e.g. `1-3,6-8,10`), **or** the word
`all` to download the whole mailbox. `all` is handled in two steps — XyScript first counts the mailbox
(you'll see `counting messages…` then `found N message(s); downloading…` in *Shell Output*), then saves
`MSG001.TXT … MSG<N>.TXT`. Quote `<path>` if it contains spaces.

### `DELMAIL <range>` — delete messages

Marks each message in `<range>` `\Deleted` and expunges the mailbox. `<range>` is the same comma/span
form as `SAVEMAIL`, and here `all` **is** accepted (maps to IMAP `1:*`).

```
DELMAIL 1-3,6-8,10,12
DELMAIL all
```

> **Numbering caveat.** IMAP sequence numbers are renumbered after an expunge. Do a `LISTMAIL` to
> re-read current numbers before a second `SAVEMAIL`/`DELMAIL`, and delete highest-first if you're
> working from one listing.

As with sending, `$RC` is curl's exit code (`0` = success), `$XO` its output, and `$LJ` the job id for
`WAIT/J`.

## Troubleshooting

**`curl: (35) schannel: … SEC_E_INVALID_TOKEN` (exit 35).** This is a TLS-handshake failure — almost
always a **port/scheme mismatch**, i.e. curl tried an immediate TLS handshake against a port that
speaks plain SMTP first (so the server's plaintext greeting looks like garbage to the TLS layer). Match
the scheme to the port:

| Port | What it speaks | Correct `SMTPURL` |
|------|----------------|-------------------|
| **465** | Implicit TLS (TLS from the first byte) | `smtps://smtp.example.com:465` |
| **587** | Plain, then upgrades via STARTTLS | `smtp://smtp.example.com:587` |
| **25** | Usually blocked / not for submission | avoid |

`SENDMAIL` always passes curl `--ssl-reqd`, so with `smtp://…:587` it performs the STARTTLS upgrade for
you — you do **not** write `smtps://` for 587. Reserve `smtps://` for 465. So: if you have
`SEC_E_INVALID_TOKEN` on 587, switch the scheme from `smtps://` to `smtp://`; on 465, keep `smtps://`.

**`MAIL failed: 553`.** The connection and login worked, but the server rejected the **envelope
sender** (the `MAIL FROM` address). Providers almost always require it to equal the account you
authenticated as (or a verified "send-as" alias). Checklist:

- `PR $MAILUSER` and `PR $MAILFROM` — `MAILFROM` must be the **exact, full email address** of that
  account (e.g. `me@gmail.com`, not a bare username, not a different domain, not an unverified alias).
  If `MAILFROM` is unset it defaults to `MAILUSER`, so make sure `MAILUSER` is the full address.
- A display-name form is fine — `MAILFROM "Me" <me@example.com>` — because the envelope `MAIL FROM`
  uses just the address inside `<…>` (the display name stays in the visible `From:` header). What still
  matters is that the **address** equals the authenticated account.
- The server appends the real reason after `553` (e.g. "Sender address rejected: not owned by user").
  Read the full line in the **\*Shell Output\*** tab — it names the exact policy you're tripping.

**Exit 0 but the mail never appears in the inbox.** Curl succeeded and the server accepted the message,
so it's a delivery/filing issue, not a send issue. In order:

1. **Turn on the SMTP dialogue:** `MAILDEBUG on`, then `SENDMAIL …` again. curl now runs with `-v`, so
   the full conversation streams to the **\*Shell Output\*** tab — including the server's final line,
   usually `250 2.0.0 OK … queued as <id>` or similar. If you see that, the server *definitely*
   accepted and queued it, and the message is somewhere in your account. (`MAILDEBUG off` to quiet it;
   `$MAILDEBUG` reads the state.)
2. **Look beyond the Inbox:** check **Spam/Junk**, **All Mail/Archive**, and **Sent**. Sending to
   *yourself* is the usual culprit — many providers (Gmail especially) recognize you as the sender and
   file the copy under All Mail/Sent rather than duplicating it into the Inbox.
3. **The decisive test — send to a *different* address** (a second account you can check). If it
   arrives there, sending works perfectly and the "not in my inbox" is just your provider's
   self-addressed filing/dedup behavior, not a bug.
4. If it's consistently filed as Spam, that's your provider's filter scoring the message (a plain
   self-sent note with no history), not XyScript dropping it.

To isolate XyScript from curl/TLS, run the equivalent by hand in a terminal:

```
curl -v --ssl-reqd --url "smtps://smtp.example.com:465" \
  --user "me@example.com:APP_PASSWORD" \
  --mail-from "me@example.com" --mail-rcpt "you@example.com" \
  --upload-file message.eml
```

If that fails too, it's a curl/credential/provider issue, not XyScript. `curl -V` shows which TLS
backend your curl uses (Schannel on stock Windows curl); a mismatch there is a curl/provider matter.

## What this does NOT do yet

- Plain-text body (`text/plain; utf-8`) — no HTML body. **Attachments are supported** (see above).
- **Receive is raw only** — `LISTMAIL`/`SAVEMAIL`/`DELMAIL` list, download, and delete, but do **no**
  MIME decoding: `SAVEMAIL` writes the untouched RFC-822 source. No search, no reply, no folder
  management beyond reading one `MAILBOX`.

(The message includes proper `Date:` — in UTC, `+0000` — `Message-ID:`, `X-Mailer`, and
`Content-Transfer-Encoding` headers.)

## How the feature flag works

`email` is a cargo feature threaded `gui → xyscript-app → xyscript-xpl`. The *implementation* — the verb
handlers in `vm.rs`, the `EditorOp` variants, the `$`-special values, and the host logic (curl, IMAP,
SMTP) — is all behind `#[cfg(feature = "email")]`. But the *names* are unconditional: the verbs are in
`BUILTIN_VERBS` and the specials in `SPECIAL_NAMES` regardless of the feature, because names are just
metadata and pull in no email code. That is what lets `docs/reference.typ` document them (its
no-omission test checks `#cmd`/`#sv` markers against those lists in the default, no-email test build)
while the code stays optional. In a build compiled *without* `email`, a `cfg(not(feature = "email"))`
arm makes each verb report "email support is not compiled in this build" and each special read empty,
rather than "unknown command". Build/test the email code with `--features email`.
