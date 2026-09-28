# Upstream Protocol Synchronization Implementation Plan

> **For agentic workers:** Use executing-plans for implementation and verification-before-completion for integration.

**Goal:** Synchronize upstream protocol conversion at `136c8023` while preserving local service behavior.
**Architecture:** Import the upstream typed URP codecs. Adapt local handlers and transforms to the fixed upstream type interfaces. Preserve local routing, billing, deployment, retries, and regression fixtures.
**Tech Stack:** Rust, Tokio, serde, Bun, Astro.
**Spec:** `spec/upstream-protocol-sync.spec.md` and its referenced protocol specifications.

## Global Constraints

- Ordinary writes remain inside the project root.
- Existing CNY work remains in stash `1105237e` until protocol integration passes.
- Production deployment is excluded.
- Do not remove local transform identifiers or legacy routes.

## Review Focus

- Same-family custom tools preserve their schemas and exact input bytes.
- Split SSE JSON, explicit event names, terminal usage, and errors preserve local behavior.
- Typed field deletion prevents replay metadata from restoring removed values.
- Trusted context cannot be supplied by the client or leak upstream.
- Tool-result images retain MIME and mask semantics after compression.

### Task 1: Canonical codecs and handler integration

**Files:** `src/urp/**`, `src/handlers/**`, `src/error.rs`, `tests/urp_upstream_sync.rs`.
**Interfaces:** Consume the upstream typed URP API at the pinned revision. Produce adapted local request preparation, stream accumulation, and dispatch.

- [ ] Add JSON-boundary regression tests for Gemini sampling and untrusted context; run `cargo test --test urp_upstream_sync` before importing implementation.
- [ ] Import typed URP, retaining local SSE parser constraints and HTTP transport.
- [ ] Adapt handler typed constructors, accumulation, tool transport preparation, and runtime context.
- [ ] Run codec and API regression suites. Preserve security and billing behavior in existing tests.

### Task 2: Transforms and docs

**Files:** `src/transforms/**`, relevant transform registry, `spec/auto-cache-transforms.spec.md`, `spec/urp-transform-system.spec.md`, matching docs in four locales.
**Interfaces:** Consume upstream typed URP fields; retain local transform IDs and semantics.

- [ ] Synchronize Anthropic automatic caching and progressing tool-result breakpoints.
- [ ] Synchronize image compression for tool-result images and complete function arguments.
- [ ] Adapt existing transforms to typed fields without removing local regression coverage.
- [ ] Update corresponding specs and four locale docs; run focused transform tests and docs build.

### Task 3: Integration review and push

- [ ] Run `cargo test`, frontend tests/lint/build when frontend changes, and docs build.
- [ ] Review the entire change for local regressions and spec alignment.
- [ ] Commit verified changes and push to `origin/Monoize-Claude`.
- [ ] Preserve the CNY stash and record the exact imported source revision and remaining work.
