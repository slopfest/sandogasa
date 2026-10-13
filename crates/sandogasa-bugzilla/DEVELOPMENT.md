# sandogasa-bugzilla — design notes

Rules this crate follows and the evidence behind them. For what it
does, see README.md.

## Two Bugzillas authenticate in ways that exclude each other

This client talks to Red Hat's Bugzilla and to RPM Fusion's, and they
do not accept the same credential. Not "one is stricter" — each refuses
the form the other requires, so there is no request that satisfies both
and no sensible default.

**Red Hat's** takes an `Authorization: Bearer <key>` header and nothing
else. A key sent as `Bugzilla_api_key` in a write's JSON body is a hard
`HTTP 400`, with the API naming it outright (checked 2026-10-13):

> You have attempted to access the API either using an unsupported
> method or using one or more unsupported parameters. You must use the
> 'Authorization' header to authenticate to the API … The unsupported
> parameters are: Bugzilla_login, Bugzilla_password, Bugzilla_token,
> Bugzilla_api_key.

**RPM Fusion's stock Bugzilla 5.0.4** does the opposite (checked
2026-10-12). A bearer token, or an `X-BUGZILLA-API-KEY` header, returns
code `410` ("You must log in") — the same answer an unauthenticated
request gets, so the key is not read at all. The forms it does read are
`Bugzilla_api_key` in the write body and an `api_key` query parameter;
a wrong key in either answers `306` ("The API key you specified is
invalid"), which is how we know it was read.

`with_stock_auth()` selects the second. It is opt-in because the
default has to be Red Hat's — that is the server nearly every caller
means — and because a client that guessed would fail in the direction
that matters: a write silently unauthenticated.

**The key goes in the body, never the query string.** Both forms work
on a stock Bugzilla, but a URL ends up in the server's access log and
in any proxy between here and it. A credential in a log is a
credential leaked, so the query-parameter form exists in this file only
as documentation of what the server accepts.

**Reads carry no key at all.** A GET has no body to put one in, that
version ignores one anyway, and everything this crate reads is public.

**Checking a key cannot mean making a write that could succeed.**
`api_key_accepted` asks to create a bug with no fields whatsoever.
Bugzilla checks the credential before it validates fields, so a refused
key answers 306, a missing one 410, and an accepted one gets as far as
complaining that `product` is absent — and nothing can be created,
because every required field is missing. Do not "test" a key by
performing a real change and undoing it.

## The REST prefix differs too

Red Hat's serves the API under `/rest/`; a stock Bugzilla serves it
under `/rest.cgi/`. `with_rest_path()` carries this, defaulting to
`rest`. It is a separate knob from `with_stock_auth()` on purpose —
they happen to coincide on the two servers we use, but the path is a
deployment choice and the auth form is a version difference, and
conflating them would make a third server harder to support than it
needs to be.

## A response model is `#[non_exhaustive]`

Every public struct here that derives `Deserialize` is a response
model, and Bugzilla grows fields without asking. See the workspace
DEVELOPMENT.md for the rule and `scripts/check-non-exhaustive.sh`,
which enforces it. Build fixtures in tests with `serde_json::json!`
rather than struct literals.
