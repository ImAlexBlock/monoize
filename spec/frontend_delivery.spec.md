# Frontend Delivery Specification

## 1. Scope

This spec defines how the bundled frontend is built, embedded, and served by Monoize.

## 2. Build and Packaging

FD-B1. Monoize MUST embed frontend build artifacts from `frontend/dist/` into the Rust binary.

FD-B2. Build pipeline MUST run frontend install/build before Rust compile when frontend sources change.

FD-B3. If frontend build fails, Rust build MUST fail.

FD-B4. `cd frontend && bun run test` MUST execute every `tests/*.test.ts` and
`tests/*.test.tsx` file with the Bun test runner. `bun run test:ui` MUST execute the
browser fixture suite through `test:browser`. The browser suite MUST run the users-load-errors,
org-spend-limits, and playground-composer fixtures with Bun and Playwright Chromium.
Browser fixture entrypoints MUST convert file URLs with `fileURLToPath` so Windows drive
letters and escaped path characters resolve to filesystem paths.

FD-B5. The frontend dependency tree MUST pin `@swc/core` to `1.15.18` until the
compressed native addon loader supports the Windows workspace cache permissions.
The build MUST NOT require changing permissions outside the project directory.

## 3. Runtime Routing

FD-R1. `GET /` MUST return embedded `index.html`.

FD-R2. `GET /<path>` MUST return embedded asset if `<path>` exists under `frontend/dist/`.

FD-R3. Unknown UI paths MUST return embedded `index.html` (SPA fallback).

FD-R3a. Every path that FD-R1 through FD-R3 serve for `GET` MUST serve `HEAD` with the same
status and the same headers, and without a body. Returning `404` for `HEAD` on a path whose
`GET` returns `200` contradicts RFC 9110 §9.3.2 and makes any uptime monitor, link checker,
or CDN health probe that uses `HEAD` report the site as missing.

FD-R4. An unknown API path MUST return `404` for any method other than `GET` and `HEAD`.

FD-R5. Dashboard SPA routes under `/dashboard/*` (for example `/dashboard/providers`, `/dashboard/users`, `/dashboard/tokens`, `/dashboard/models`) MUST resolve to embedded `index.html` on direct browser navigation and hard refresh.

FD-R6. Dashboard API handlers MUST be exposed only under `/api/dashboard/*` and MUST NOT intercept direct browser `GET` requests for `/dashboard/*` SPA paths.

## 4. Frontend API Base

FD-A1. Frontend MUST use `/api` as backend base path in development and production.

FD-A2. Vite dev server MUST proxy `/api/*` to backend in development.

## 5. CSP Compatibility

FD-C1. The embedded frontend entry document (`frontend/dist/index.html`) MUST remain compatible with the backend Content Security Policy that sets `script-src 'self'`.

FD-C2. The entry document MUST NOT contain inline `<script>` blocks or inline event-handler attributes. Any startup logic required before React mounts (for example theme resolution) MUST be delivered through same-origin external script modules.

FD-C3. The backend MUST replace the entry document's CSP nonce placeholder with the request's fresh nonce before returning `index.html`. Hashed static assets MUST be returned without content substitution.

FD-C4. The backend MUST set `script-src 'self' 'nonce-<request-nonce>'`. The nonce MUST differ between independent requests. The frontend MAY read the nonce from a metadata element and pass it to a library that creates inline scripts, but the frontend MUST NOT create its own executable inline script in `index.html`.

FD-C5. The backend MUST set `img-src * data:`. The policy MUST allow HTTPS Channel icons
and Playground image output from any network origin. The policy MUST allow data URL image
attachments.

## 6. HTTP Caching

FD-H1. Responses serving the embedded SPA entry document (`index.html`) for `GET /` and SPA fallback routes (for example `/dashboard/*`) MUST include `Cache-Control: no-store`.

FD-H2. Responses serving hashed static frontend assets under `frontend/dist/assets/` MUST include `Cache-Control: public, max-age=31536000, immutable`.
