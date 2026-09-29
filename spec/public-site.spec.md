# LynShen Public Site Specification

## 0. Scope

PS-0.1. This specification defines the public browser routes, the public site-settings API,
the shared public API controls, the welcome page, the API documentation page, localization,
and LynShen branding.

PS-0.2. `model-marketplace.spec.md` defines Marketplace payloads and pagination.
`public-provider-status.spec.md` defines Provider status payloads and aggregation.

## 1. Browser routes

PS-R1. The application MUST register these exact routes:

| Path | Surface | Dashboard session required |
| --- | --- | --- |
| `/` | Welcome page | No |
| `/login` | Login and registration | No |
| `/apidocs` | API documentation | No |
| `/status` | Group and Provider status | No |
| `/marketplace` | Public Model Marketplace | No |
| `/usage-ranking` | Public anonymous usage ranking | No |
| `/dashboard` and every `/dashboard/*` path | Console | Yes |
| `/settings` | User settings | Yes |

PS-R2. `/marketplace` MUST be an explicit public top-level route. The separate
`/dashboard/marketplace` route MUST be a protected child of `/dashboard` and render inside
`DashboardLayout`.

PS-R3. `/` MUST remain public after authentication. The router MUST NOT redirect an
authenticated visitor from `/` to `/dashboard`.

PS-R4. `/`, `/apidocs`, `/status`, `/marketplace`, and `/usage-ranking` MUST use one
`PublicLayout`.

PS-R5. `PublicLayout` MUST render Home, Model Marketplace, Usage Ranking, API Docs, Status,
Console, and Login actions. Console MUST target `/dashboard`. Login MUST target `/login`.

PS-R6. The public gateway MUST return HTTP `308` for every request to
`https://www.lynshen.org`. The `Location` value MUST use `https://lynshen.org` and MUST
preserve the request path and query. The gateway MUST proxy `https://lynshen.org` to the
single public application process.

## 2. Public site settings

PS-S1. `GET /api/public/site` MUST require no dashboard session and MUST return exactly:

```text
site_name: string
site_description: string
api_base_url: string
```

PS-S2. The endpoint MUST read only `site_name`, `site_description`, and `api_base_url`.
A missing row MUST resolve to that setting's built-in default.

PS-S3. The endpoint MUST NOT return registration, CAPTCHA, authentication, transform,
redirect, pricing, Provider, Channel, Group, suffix-map, or other settings.

PS-S4. Every public browser surface MUST fetch PS-S1 through SWR. Initial loading MUST
render Skeletons that match the title, description, and Base URL layout. Public surfaces
MUST NOT mutate server state.

PS-S5. The login page MUST continue to use the existing login-settings endpoint for
registration and CAPTCHA behavior. It MUST NOT use PS-S1 as an authentication policy.

PS-S6. A dashboard session transition MUST clear every non-public SWR cache entry and MUST
preserve every string cache key that starts with `/api/public/`. An HTTP `401` from
`GET /api/dashboard/auth/me` MUST NOT delete a public response that completed before,
during, or after that authentication request.

## 3. Shared public API controls

PS-A1. The public API set is exactly:

- `GET /api/public/site`;
- `GET /api/public/marketplace`;
- `GET /api/public/marketplace/offers`;
- `GET /api/public/status`.
- `GET /api/public/usage-ranking`.

PS-A2. Each endpoint MUST serialize an explicit allow-list. It MUST NOT serialize a
complete database entity or runtime configuration object.

PS-A3. Public Provider, Channel, and Group names MUST come from approved public-name
fields. An endpoint MUST NOT fall back to an internal name.

PS-A4. One process-local token bucket MUST cover all PS-A1 paths. The key MUST be the
canonical client IP produced by the trusted-proxy contract. The application MUST NOT trust
an arbitrary forwarded-IP header.

PS-A5. Each bucket MUST refill continuously at one token per second, hold at most 20
tokens, and consume one token per request. A request without one token MUST return HTTP
`429` with code `rate_limited`.

PS-A6. The bucket map MUST contain at most 10,000 entries. An entry is idle after 120
seconds without a request. At capacity, a request from a new IP MUST evict the
least-recently-seen idle entry. If no idle entry exists, the request MUST return PS-A5 and
MUST NOT add a bucket. Existing IP entries remain usable at capacity.

PS-A7. Production deployment MUST expose the public API through exactly one application
process behind the public gateway. A topology with two or more public application
processes MUST fail deployment preflight unless the gateway or one shared atomic store
enforces one equivalent 60-per-minute, burst-20 bucket per canonical client IP.

PS-A8. Site and Marketplace responses MUST send
`Cache-Control: public, max-age=15, stale-while-revalidate=30`. Status MUST send
`Cache-Control: public, max-age=15`.

PS-A9. A public snapshot ETag MUST equal `W/"<sha256>"`, where `<sha256>` is the lowercase
SHA-256 hexadecimal digest of the exact uncompressed JSON bytes. The response MUST send
`Vary: Accept-Encoding` and MUST NOT send `Vary: Cookie`.

PS-A10. A public cache key MUST include the canonical path and canonical query parameters.
The Site snapshot MUST remain byte-identical until one of the three PS-S1 settings changes.

PS-A11. The server MUST parse `If-None-Match` as an HTTP entity-tag list. It MUST return
HTTP `304` with no body when the list weakly matches the current ETag or contains `*`.
A malformed header MUST be ignored. Rate limiting MUST run before conditional evaluation.

PS-A12. Every public response MUST send `X-Content-Type-Options` and the existing Content
Security Policy protections.

## 4. Welcome page

PS-W1. The welcome page MUST render these sections in this order:

1. Product statement and two actions.
2. A supported-model marquee and one API request example.
3. Model cards.
4. A capability list.
5. A summary statistics strip.
6. A frequently-asked-questions list.
7. Final actions and footer navigation.

PS-W2. The welcome page MUST NOT display Provider, Channel, or Group counts. The summary
statistics strip MAY display aggregate catalog figures.

PS-W3. The primary actions MUST link to Model Marketplace and API Docs. The status action
MUST link to `/status`.

## 5. API documentation page

PS-D1. `/apidocs` MUST document OpenAI Responses, OpenAI Chat Completions, Anthropic
Messages, Gemini Generate Content, image generation, streaming, authentication, and errors.

PS-D2. Every request family MUST contain raw HTTP examples in cURL, Python, JavaScript,
and Go. A reader MUST NOT need one specific SDK.

PS-D3. The displayed Base URL MUST come from PS-S1. If `api_base_url` is empty and the
browser origin uses HTTPS, the page MUST use `<browser-origin>/v1`. If the setting is empty
and the origin is not HTTPS, the page MUST show a configuration error and disable copy
actions.

PS-D4. The page MUST document HTTP `403 model_pricing_required`, authentication failures, the
PS-A4 through PS-A7 rate limit, and streaming termination.

PS-D5. The page MUST state that Marketplace per-unit prices are informational. Billing
sums integer nano-USD line items, applies one exact decimal multiplier to the aggregate,
and truncates once at final scaling.

## 6. Visual and accessibility contract

PS-V1. Public surfaces MUST use the public minimal direction defined by `design.md`:
a near-monochrome palette (ink on paper, paper on ink), pill actions, sans-serif display
and body type, and monospace code. The public tokens are scoped to `PublicLayout` and MUST
NOT change the Console tokens defined by `frontend-design-system.spec.md`.

PS-V2. Public surfaces MUST NOT introduce a brand palette beyond the public tokens in
`design.md`. Icons MUST come from Lucide or the existing product icon set. Emoji MUST NOT
serve as interface icons.

PS-V3. Motion MUST animate only `transform` and `opacity`. Interaction duration MUST be
from 150 through 300 milliseconds. `prefers-reduced-motion: reduce` MUST disable positional
and repeating motion.

PS-V4. The 375, 768, 1024, and 1440 CSS-pixel viewports MUST have no page-level horizontal
scrolling. Body text MUST be at least 16 CSS pixels at 375 pixels. Prose SHOULD use 65 to
75 characters per line when the viewport permits it.

PS-V5. Text contrast MUST meet WCAG AA. Each public page MUST expose a keyboard-visible
skip link, semantic landmarks, ordered headings, visible three-pixel focus rings,
accessible names for icon-only controls, and touch targets of at least 44 by 44 CSS pixels.

PS-V6. Hover, focus, active, and selected states MUST NOT shift layout.

## 7. Localization and branding

PS-L1. Public UI MUST support exactly `en`, `zh`, `zh-TW`, and `ja` through the existing
i18next catalog. Public browser paths MUST NOT gain locale prefixes.

PS-L2. Every public user-visible string MUST exist in all four catalogs. Canonical product
nouns, endpoint paths, and environment-variable names MUST remain English in every locale.

PS-L3. The built-in `site_name` default MUST be `LynShen Console`. The static HTML fallback
title MUST be `LynShen Console`.

PS-L4. The login page and Console layout MUST render the runtime `site_name`. The welcome
page, Marketplace, API Docs, usage-ranking page, and status page MUST render the public
brand name. The public brand name equals `site_name` with one trailing ` Console` suffix
removed (ASCII case-insensitive, surrounding whitespace ignored). When removal produces an
empty string, the runtime `site_name` MUST be used unchanged.

PS-L6. On every route the browser tab title MUST equal the runtime `site_name` once the
public site settings response arrives. The static HTML `<title>` element is the
pre-hydration fallback only. When the settings request fails the fallback title MUST
remain unchanged.

PS-L5. The migration MUST replace a stored site name only when it exactly equals an old
built-in default. It MUST preserve every administrator-defined value.

## 8. Verification

PS-T1. Route tests MUST verify every PS-R1 authentication boundary for authenticated and
unauthenticated browsers.

PS-T2. Serialization tests MUST assert the exact PS-S1 keys and reject every known secret
or internal field name.

PS-T3. Token-bucket tests MUST cover refill, burst, shared-path consumption, map capacity,
idle eviction, no-idle rejection, and trusted-proxy IP selection.

PS-T4. Cache tests MUST cover stable bytes, identity and compressed encodings, weak ETag
lists, wildcard validators, malformed validators, and HTTP `304` with no body.

PS-T5. Browser tests MUST cover keyboard navigation, skip links, focus visibility, reduced
motion, and PS-V4 viewports.

PS-T6. Locale validation MUST fail when one PS-L1 catalog lacks a public key.

PS-T7. A cache test MUST store one `/api/public/` key and one dashboard key, execute the
dashboard session cache clear, and assert that only the public value remains.
