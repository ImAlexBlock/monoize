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

1. Product statement, Console action, API Docs action, and one API request example. The
   hero MUST NOT render an eyebrow or category label above the product statement.
2. A live low-price model section.
3. Supported API families and a statement that clients which accept a custom Base URL and
   API key can connect without changing their request protocol.
4. Four relay-service advantages: one account and connection, public Marketplace pricing,
   Group-scoped routing with fail-forward, and request-level usage and charge records.
5. Six customer tasks: call text and image models, connect compatible SDKs and coding
   tools, control API keys, use organization workspaces, purchase balance or plans, and
   inspect requests and usage.
6. Three-step connection flow: create an API key, select a Marketplace model, and send a
   request.
7. Operations explanation covering public Provider status, request records, and balance
   accounting.
8. Raw HTTP API example.
9. Final Console and public-status actions.

PS-W2. The welcome page MUST NOT display model, Provider, Channel, or Group counts.

PS-W3. The primary actions MUST link to `/dashboard` and `/apidocs`. The Marketplace
action MUST link to `/marketplace`. The final actions MUST link to `/dashboard` and
`/status`.

PS-W4. Welcome-page claims MUST describe LynShen as a hosted AI API relay service. They
MUST NOT present Monoize internals, self-hosting, Provider administration, Channel
configuration, URP, transforms, or Rust implementation details as customer benefits.

PS-W5. The welcome page MUST use section-level borders and background changes to separate
content. It MUST NOT place every advantage in an independent floating Card.

PS-W6. The low-price model section MUST load public Marketplace pages through SWR. It MUST
request pages of 50 items and follow cursors until the snapshot ends. It MUST stop and show
the failure fallback if more than 19 Marketplace requests would be required.

PS-W7. The section MUST consider only items whose input rate exists and uses the `token`
unit. It MUST keep the cheapest input-price item per model name and sort the result by
input price ascending (ties broken by Group name, then model name, both by UTF-8 byte
order). The section MUST display exactly four cells at a time and advance the window by
one model every 5 seconds, wrapping to the start; a catalog shorter than four renders
every model with no rotation. Each available item MUST show model, Group, minimum input
price, minimum output price when its unit is `token`, offer count, and the item's
`input_rate_multiplier` as an `x<decimal>` chip beside the Group name when present. The
section MUST NOT pin fixed vendors or model families.

PS-W8. The section MUST state that prices update in real time and that discounts vary with
upstream costs. It MUST state that each request is settled using the real-time discount at
request time. Displayed values use USD per one million tokens. The section MUST convert
Marketplace nano-CNY values through the response `cny_per_usd` using exact decimal
arithmetic. It MUST NOT claim a fixed discount percentage or compare with an official list
price unless the server provides that comparison. Loading MUST render four Skeleton cells.
Empty or failed loading MUST retain an action to
`/marketplace`. The rotation transition MUST be a smooth slide: the entering card slides in
from the trailing edge, the leaving card slides out at the leading edge, and the carried
cards glide to their new slots, all in one spring; under reduced motion only opacity
animates.

PS-W9. Every welcome-page section reveals its content once when it enters the viewport:
a fade with a small upward rise (about 0.55s, ease-out). Cell grids (families, advantages,
tasks, steps, operations) reveal their cells staggered by about 60ms per cell. Under
reduced motion the reveals animate opacity only, per `DESIGN_SYSTEM.md`. Reveals play once
per page load and MUST NOT re-run on scroll-up.

PS-W10. The hero choreographs on load: the headline rises with a blur-to-sharp fade, the
description and the two actions follow with increasing delays, and the terminal card rises
last with a slight rotation that settles. The terminal reveals its command line by line
(about 0.28s apart) ending in a blinking block caret; its traffic dots pulse on staggered
delays. The grid backdrop pans one cell in a slow loop and parallaxes upward at roughly
0.2x scroll speed; a blurred primary glow orb pulses behind it. Every section label
(`NN · NAME`) slides in beside an accent line that draws left-to-right. Bordered grid
cells tint their background and scale their icon on hover; client-example chips pop in
with a slight rotation and tint on hover; the primary call-to-action nudges its arrow on
hover. All of these degrade under reduced motion: loops and parallax stop, transforms
reduce to opacity fades, and CSS animations are disabled by the `prefers-reduced-motion`
media rule in `index.css`.

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

PS-V1. Public surfaces MUST use the Paper Console direction and the tokens defined by
`frontend-design-system.spec.md`. They MUST use the existing neutral background, blue
primary color, serif display font, sans-serif body font, and code font.

PS-V2. Public surfaces MUST NOT introduce another brand palette. Icons MUST come from
Lucide or the existing product icon set. Emoji MUST NOT serve as interface icons.

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

PS-L4. The login page, Console layout, welcome page, Marketplace, API Docs, and status page
MUST render the runtime `site_name`.

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
