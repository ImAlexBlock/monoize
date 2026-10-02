# Security Access Control Spec

## Public LynShen surfaces

SAC-P1. The public browser paths and public APIs listed in `public-site.spec.md` PS-R1 and
PS-A1 MUST NOT require a dashboard session. Their responses MUST use the explicit
allow-lists in `public-site.spec.md`, `model-marketplace.spec.md`, and
`public-provider-status.spec.md`.

SAC-P2. Public responses MUST NOT contain API keys, Base URLs, proxy URLs, extra headers,
internal Provider, Channel, or Group names or IDs, Billing Profile names, rate row IDs,
request data, user data, or topology fields not explicitly listed by those response
schemas.

SAC-P3. Public status and Marketplace names MUST come only from confirmed public-name
fields. An absent public name makes that record ineligible for a public response; the
server MUST NOT fall back to an internal name.

## Scope

This spec defines authentication for topology-bearing HTTP surfaces, API-key
expiry update validation, and authorization for administrator account updates.

## Deployment credentials

SAC-D1. Tracked deployment scripts and handoff records MUST NOT contain live database passwords. The historical PostgreSQL cutover script MUST require `MONOIZE_CUTOVER_POSTGRES_DSN` from the operator environment before it performs any action. Removing a password from the working tree does not remove it from Git history or rotate it.

## Topology-bearing HTTP surfaces

SAC-1. `GET /metrics`, `GET /presets/providers`, `GET /presets/apikeys`, and
`GET /api/dashboard/transforms/registry` MUST require an authenticated dashboard
session whose user role is `admin` or `super_admin`.

SAC-2. A request covered by SAC-1 with no dashboard session or an invalid or
expired dashboard session MUST return HTTP `401`.

SAC-3. A request covered by SAC-1 whose dashboard session belongs to a user with
role `user` MUST return HTTP `403`.

SAC-4. The dashboard session MAY be supplied as `Authorization: Bearer <token>`
or by the existing `monoize_session` cookie. The session lookup, expiry check,
user lookup, enabled-state check, and role check MUST use
`session_helpers::require_admin`.

SAC-5. SAC-1 through SAC-4 apply to `/metrics` on primary and replica nodes.

## API-key expiry updates

SAC-6. If `UserStore::update_api_key` receives a non-null `expires_at`, it MUST
parse the complete value with `chrono::DateTime::parse_from_rfc3339` before it
writes any API-key field.

SAC-7. If the parse in SAC-6 fails, the operation MUST return an error and MUST
not modify the API-key row.

SAC-8. If the parse in SAC-6 succeeds, the operation MUST store the supplied
RFC 3339 string. Subsequent API-key row decoding MUST return the same instant.

## Administrator account updates

SAC-9. A user with role `admin` MAY update its own account.

SAC-10. A user with role `admin` MUST NOT update an account whose current role
is `admin` and whose user ID differs from the acting user's ID. The endpoint
MUST return HTTP `403` before applying any requested field, including password.

SAC-11. A user with role `admin` MUST NOT update an account whose current role
is `super_admin`. The endpoint MUST return HTTP `403` before applying any
requested field.

SAC-12. A user with role `super_admin` MAY update an account whose current role
is `admin` or `super_admin`, subject to the other user-update validation rules.
