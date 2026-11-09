# Celsus server: security design

The server edition runs on a home server or a Raspberry Pi and is meant to be reachable from the internet behind an
HTTPS reverse proxy. This document describes what it protects, against whom, and how. Every rule here has a test in
`crates/celsus-server/tests`.

## What is protected

- Accounts: passwords, second-factor secrets, sessions and API keys.
- Books and translations: each library belongs to its members; nobody reads or changes what they were not given.
- Provider keys (OpenAI, NVIDIA, ...): stored encrypted, never sent back to a browser, never logged.
- The server itself: no file access outside the library, no requests to internal networks on a user's behalf, no
  resource exhaustion from one client.

## Who we defend against

| Attacker | Examples |
|---|---|
| Anonymous internet | Password guessing, claiming a fresh install, scanning for admin pages, malformed requests |
| Another website | Cross-site request forgery, clickjacking, reading responses cross-origin |
| A signed-in member | Reading other members' data, raising their own role, abusing provider settings to reach the LAN |
| Someone with a stolen copy of the database | Recovering passwords, session tokens, API keys, second-factor secrets or provider keys |
| Someone holding a stolen session or key | Keeping access after logout, password change or revocation |

## Accounts

- **First run.** A new server has no accounts. It prints a one-time setup code to its log; creating the owner account
  requires that code, so nobody who merely finds the address can claim the server. The code is gone once used.
- **Joining.** Other people join through single-use invitation links made by an admin. Links expire after 7 days and
  are stored only as hashes.
- **Roles.** `owner` (one, cannot be removed or demoted), `admin` (manages members) and `member`. Role checks happen on
  the server for every request; the interface only hides what is not allowed.
- **Passwords** are hashed with Argon2id (19 MiB, 2 passes, 1 lane, the OWASP baseline that also suits a Pi). Length
  10 to 256 characters; the email address and the most common passwords are refused. Hash parameters are stored with
  each hash, so they can be raised later and old hashes are upgraded at the next sign-in.
- **Unknown accounts** take the same time as known ones: a dummy hash is checked so response time does not reveal
  which addresses exist, and the error message is the same.

## Signing in

- **Throttling.** Failed attempts are counted per account and per client address in the database (so a restart does
  not reset them). After 5 failures within 15 minutes an account needs a growing wait before the next try (up to
  15 minutes); one address is limited to 30 attempts per 15 minutes across all accounts. The account is never fully
  locked, so an attacker cannot lock the owner out.
- **Two-step sign-in (TOTP).** RFC 6238, 6 digits, 30-second steps, one step of clock drift allowed. A code is
  accepted once only (the last used step is stored). Secrets are encrypted at rest with XChaCha20-Poly1305 under the
  server key. Ten single-use backup codes are shown once and stored as hashes. Turning two-step sign-in off needs the
  password and a current code.
- **Between steps** the password alone gives a short-lived (5 minute) pending session that can do nothing except
  submit the code, and it is throttled like a password.

## Sessions

- 256-bit random tokens in a `__Host-celsus_session` cookie: `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`.
- Only the SHA-256 of the token is stored, so a copied database gives no usable session.
- Sessions end after 7 days without use or 30 days in total, whichever is first. Signing in issues a new token.
- Signing out ends the session; changing the password, turning two-step sign-in on or off, or disabling an account
  ends every other session of that account.

## Cross-site protection

- `SameSite=Strict` cookies, plus a per-session CSRF token that must come back in the `X-CSRF-Token` header on every
  request that changes something. Requests whose `Origin` is present and not the server's own are refused.
- API keys are not cookies, so requests carrying them need no CSRF token and cannot be forged by another site.

## API keys

- For scripts and command-line use. Format `celsus_<id>_<secret>`; the secret is 256 bits and stored as a hash.
- Each key has scopes (`read`, `translate`, `admin`), an optional expiry and a last-used time. A key can never do more
  than its owner can. Keys are shown once, can be revoked at any time and are revoked with the account.

## Chat apps (MCP and OAuth)

- `POST /mcp` takes only bearer credentials: an API key or an OAuth access token. A browser's session cookie is not
  accepted there, and requests from other sites' pages are refused like any other cross-site write.
- Apps such as ChatGPT and Claude register themselves (`/oauth/register`) and send the person to the web interface,
  which shows the app's name, where it will return and what it may do. Nothing is granted until the person allows it
  while signed in; an API key cannot approve on anyone's behalf.
- Redirect addresses must be https, or http only to this computer, and must match the registration exactly. PKCE
  with S256 is required. A code works once and for ten minutes; a failed attempt spends it.
- Access tokens last an hour and refresh tokens 60 days; refresh tokens rotate, so a stolen one stops working at the
  next use. Codes and tokens are stored only as SHA-256 hashes. Tokens carry the scopes granted (`read`,
  `translate`), never `admin`, and stop working when the person disconnects the app or the account is disabled.
- Each person sees their connected apps under **Chat apps** and can disconnect them; registrations, approvals and
  disconnections are in the audit log.

## Audit log

Sign-ins (successful and failed), sign-outs, two-step changes, password changes, invitations, role and account
changes, API key creation and revocation, and setup are recorded with time, account, client address and user agent.
The log is append-only for the application and visible to admins. It never contains passwords, codes or tokens.

## Secrets at rest

- A 256-bit server key lives in `server.key` next to the data (created on first run, readable only by the server
  user). It encrypts second-factor secrets and provider keys. A database dump without that file reveals none of them.
- Passwords, session tokens, API keys, invitation tokens and backup codes are stored only as hashes.

## Responses and requests

- Headers on every response: a strict `Content-Security-Policy` (`default-src 'self'`, no inline scripts,
  `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
  `Cross-Origin-Opener-Policy: same-origin`, `Permissions-Policy` turning off camera, microphone and location, and
  `Strict-Transport-Security` when the server is told it is served over HTTPS.
- API responses are `Cache-Control: no-store`.
- Request bodies are limited (1 MiB for JSON, a configurable size for uploads), and slow clients are cut off.
- Error responses say what the client did wrong, never how the server failed inside.
- All database access uses bound parameters; no SQL is built from input.
- The client address comes from `X-Forwarded-For` only when the request arrives from a proxy listed in
  `CELSUS_TRUSTED_PROXIES`; otherwise it is the connecting address, so it cannot be forged to dodge throttling.

## The library

- **Who sees a book.** A book is private to whoever added it unless they share it with all members. A book someone
  may not see answers "not found", exactly like one that does not exist, so ids reveal nothing; ids are 128 random
  bits. Readers of a shared book may translate it; only its owner changes who sees it, and the owner or an admin may
  edit or delete it. Admins do not see other people's private books.
- **Files.** Every path on disk is built from a server-made id and a checked language code, never from a name a
  client sent; uploaded file names are kept only as text. The web files are served by a file server that refuses
  paths leaving its folder.
- **Uploads.** Only PDFs are accepted (checked by content, not by name), up to `CELSUS_MAX_UPLOAD_MB`. Reading one runs
  off the request threads with a time limit, and no stream may expand past 256 MiB, so a small crafted file cannot
  exhaust memory.
- **Jobs.** Page lists are checked against the book; one person can queue at most 20 jobs. Only the person who
  started a job, the book's owner or an admin can stop it.

## Model connections

- Only admins add connections or choose models; members use them.
- Keys are encrypted with the server key, bound to their connection, and never sent back or logged.
- Addresses must be http or https without a user name in them. Unless a connection is marked as on the local
  network on purpose, every address its name resolves to must be on the public internet: loopback, private, link-local
  (including the cloud metadata address), CGNAT, unique-local and IPv4-mapped forms are refused. This is checked when
  the connection is saved and again before every job, and the client never follows redirects.

## The web interface

- No inline scripts or styles anywhere, so the Content-Security-Policy needs no exceptions; PDF pages are drawn by
  pdf.js with evaluation of code from documents turned off.
- Everything from the server is escaped before it becomes HTML; book titles such as `<img onerror=...>` show as text.
- The session cookie is invisible to scripts; every change carries the CSRF token.

## Still to come

- Per-user storage quotas.
