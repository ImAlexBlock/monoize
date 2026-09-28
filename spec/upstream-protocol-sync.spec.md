# Upstream Protocol Synchronization Specification

UPS-1. The protocol synchronization source is Ikaleio/monoize revision `136c8023b6622bcffd941423b39ab2e6a78fc247`.
UPS-2. The canonical protocol types and adapters MUST satisfy `urp-v2-flat-structure.spec.md`, `urp-v2-rust-core-mapping.spec.md`, `gemini-codec.spec.md`, `media-transport.spec.md`, and `protocol-conformance.spec.md`.
UPS-3. Existing downstream routes, including Codex Responses aliases and legacy Completions, MUST retain their documented authentication and routing behavior.
UPS-4. Protocol synchronization MUST NOT change wallet denomination, Store settlement, organizations, channel selection, or deployment behavior.
UPS-5. Responses SSE decoding MUST retain bounded parsing of adjacent JSON values, payload-type fallback, and a trailing `[DONE]` sentinel. Explicit non-default SSE event names take precedence. A non-null top-level error without a response envelope MAY use the implicit event name `error`.
UPS-6. Schema-backed custom-tool coercion MUST run only on cross-family requests. Same-family Responses and Messages requests MUST retain native custom-tool definitions and input bytes.
UPS-7. Streaming usage received after a terminal content delta MUST contribute to the final usage. A successful terminal response MUST NOT replace an upstream error.
UPS-8. Existing cache-breakpoint rejection retries, upstream-model observation, error sanitization, and local transform identifiers MUST remain available.
UPS-9. Trusted runtime context MUST be injected from authenticated request state. Client JSON MUST NOT populate it or cause it to be sent upstream.
UPS-10. The existing downstream WebSocket-to-HTTP bridge remains the transport for Responses WebSocket requests. This synchronization does not activate upstream WebSocket connections.

UPS-11. Chat request history with non-empty native `reasoning.text` details MUST treat scalar reasoning fields as derived aliases. Summary-only details MUST NOT suppress distinct raw reasoning text.
UPS-12. Responses assistant history MUST preserve non-empty typed citations and valid token scores. Empty or absent citations and token scores MUST be omitted from request content. Native extras MUST NOT restore deleted typed values.
