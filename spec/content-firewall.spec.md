# Content Firewall Specification

## 0. Scope

- Product name: Monoize.
- Subsystem: request-side content firewall for all model-forwarding HTTP
  endpoints (`src/content_firewall.rs`, `src/moderation_judge.rs`,
  enforcement in `src/handlers/`).
- Purpose: keep prohibited content (pornography, politically illegal content)
  away from upstream providers while allowing legitimate discussion of those
  topics. Detection is semantic: an LLM judge classifies the scanned request
  text; a keyword matcher provides hints and marks borderline requests but
  never blocks by itself. This replaces pure keyword blocking, which could not
  distinguish "text prohibiting pornography" from "pornography".
- Non-goals: image/audio byte scanning, output-side (response) moderation,
  per-caller configuration of exemptions.

## 1. Settings

CF-1. Rows in `system_settings` configure the firewall:

| key | type | default |
|-----|------|---------|
| `moderation_enabled` | bool | `true` |
| `moderation_blocked_words` | text (newline-separated keyword hints) | the built-in list of CF-3 |
| `moderation_judge_enabled` | bool | `false` |
| `moderation_judge_base_url` | text | empty |
| `moderation_judge_api_key` | text | empty |
| `moderation_judge_model` | text | empty |
| `moderation_judge_timeout_ms` | u64 | `8000` |

CF-2. `moderation_blocked_words` is stored verbatim as received from the admin
API. Canonicalization happens at compile time only (CF-4) and never rewrites
the stored value. Keyword matches are hints for the judge and the mark
trigger (CF-30); they MUST NOT cause a rejection by themselves.

CF-3. The built-in default keyword list is ordered by weight: NSFW/CSAM
terms first (the enforcement priority), political terms after. The exact
terms live in `DEFAULT_BLOCKED_WORDS` (`src/content_firewall.rs`). The list
MUST contain only high-precision stems: explicit sexual nouns (`色情`,
`色图`, `黄片`, `成人小说`, `裸聊`, `nsfw`, `hentai`, `r18`, …),
zero-tolerance child-sexual-abuse terms, and political terms
(`颠覆国家政权`, `煽动分裂`, `恐怖主义`, …). It MUST NOT contain
single-character CJK stems (`淫`, `骚`) or everyday homographs whose
common non-sexual reading would send ordinary chat to the judge
(`开车`, `本子`, `调教`, `露骨`, `萝莉`, `正太`). Administrators
extend or narrow the list through the settings page at any time.
A process start MUST replace a stored `moderation_blocked_words` value
that still equals a previous built-in default with the current
`DEFAULT_BLOCKED_WORDS`; an operator-edited list MUST be left unchanged.

CF-4. Term canonicalization: split the raw value on `\n`, trim leading and
trailing Unicode whitespace from each line, drop empty lines, lowercase each
term with Unicode simple case folding (`str::to_lowercase`), drop duplicate
terms preserving first occurrence.

CF-5. The compiled matcher is an Aho-Corasick automaton over the canonicalized
terms. Matching is leftmost substring matching against the Unicode-lowercased
scanned string. Word boundaries are NOT required: a term matches inside any
surrounding text. When the canonicalized term list is empty, no automaton
exists and no scanning occurs.

CF-5a. Keyword extraction MUST collect every distinct canonicalized term that
matches any scanned string of the request, not only the first hit. The hit
list is ordered by compiled-list position (so the default NSFW-first weights
put the strongest signal first), is passed in full to the judge as the hint
line, appears in full in the client-facing rejection message (CF-15), and is
joined into the event `term` field (CF-21) so the audit trail records the
complete signal.

## 2. Scanned text set

CF-7. For `POST /v1/chat/completions` (and its `/chat/completions` alias), the
WebSocket responses path, and `POST /v1/completions` (translated to a chat
completion before dispatch): the scanned strings are, of the decoded
`urp::UrpRequest` input, every `Node::Text.content`, `Node::Refusal.content`,
`Node::Reasoning.content`, `Node::Reasoning.summary`, and
`Node::ToolCall.arguments` value.

CF-8. For `POST /v1/responses` (and its `/responses` alias): the scanned
strings are the same node-derived set as CF-7.

CF-9. For `POST /v1/messages` (and its `/messages` alias): the scanned
strings are the same node-derived set as CF-7.

CF-10. For `POST /v1/responses/compact`: the scanned strings are the same
node-derived set as CF-7 applied to the decoded routing request.

CF-11. For `POST /v1/embeddings`: the scanned strings are the `input` string,
or every string element when `input` is an array of strings.

CF-12. For `POST /v1/images/generations`: the scanned string is the `prompt`
field. For `POST /v1/images/edits`: the scanned string is the `prompt`
multipart field.

CF-13. Binary node payloads (`Node::Image`, `Node::Audio`, `Node::File`
sources) are not decoded and not scanned. Model names, tool names, and JSON
field names outside the CF-7..CF-12 strings are not scanned.

## 3. Enforcement

CF-PG-1. PostgreSQL MUST store `created_at_unix_ms` as BIGINT.
When upgrading the historical INTEGER schema, recover milliseconds from the
RFC3339 `created_at` using floor(epoch * 1000) in the same transaction as widening.
Malformed timestamps MUST abort the transaction. Preserve event IDs and payloads.
The repair MUST NOT narrow this column during rollback.

CF-14. The firewall check runs in every handler of CF-7..CF-12 after tenant
authentication succeeds, after model redirects and the model allowlist check,
and before any of: attempt building, the balance/quota gate, request-log
admission, and any upstream network I/O. A rejected request performs zero
upstream network calls and consumes no billing.

CF-15. A rejection returns HTTP `403 Forbidden` with the standard OpenAI-style
error envelope:

```json
{"error": {"message": "触发网站风控违禁词，无法调用模型：内容命中网关内容防火墙规则[关键词：a、b]，已被拦截。请修改内容后重试。", "type": "content_policy_violation", "param": null, "code": "content_blocked"}}
```

The message lists every canonicalized keyword hit (CF-5a) joined with `、`
inside the brackets; by CF-33 every rejection has at least one. On
`/v1/messages` the same status, code, type, and message are wrapped in the
Anthropic error envelope by the existing handler wrapper. On the responses
WebSocket the same fields are delivered through the existing WebSocket error
event path.

CF-16. The firewall applies to every authenticated caller except the
exemptions in this clause. Exemptions: (a) the judge-loop guard CF-29, which
external callers cannot forge; (b) the caller whose authenticated `user_role`
is `admin` or `super_admin` (CF-16a) — every check of CF-33 is skipped, so
the exempted caller produces no judge call and no event row for any request.
No request field can disable or weaken the check for a non-exempt caller.

CF-16a. Role exemption source of truth: the `user_role` field of the
authenticated `AuthResult` (set from the user row at token validation for
API keys and from the session user for internal sources such as the
playground). The role exemption is a pure read of that field; it performs no
database lookup, uses no request header, and applies uniformly across all
endpoints of CF-7..CF-12. A role change takes effect on requests
authenticated after the change is persisted.

CF-17. Each rejection emits exactly one `tracing::warn` record containing the
authenticated user id (when present), the API key id (when present), and the
category or term. Each marked request (CF-30) emits exactly one
`tracing::info` record. Moderation rejections do not create request-log rows,
matching the existing pre-forward rejection behavior (for example
`model_not_allowed`).

## 4. Runtime propagation

CF-18. The compiled keyword automaton and the judge configuration live in the
shared runtime snapshot (`MonoizeRuntimeConfig`). They are rebuilt: at process
start from stored settings, on the primary after a successful
`PUT /api/dashboard/settings` that changes any firewall setting, and on
replicas when the config epoch changes. Updates take effect on requests
received after the rebuild, without restart.

## 5. Admin UI

CF-19. The admin settings page exposes the firewall settings in one dedicated
category `moderation` (see `system-settings-ui.spec.md` SSU-1): the
`moderation_enabled` switch, the judge configuration (`moderation_judge_enabled`
switch, `moderation_judge_base_url`, `moderation_judge_api_key`,
`moderation_judge_model`, `moderation_judge_timeout_ms`), and the
`moderation_blocked_words` multi-line textarea relabeled as keyword hints.

## 6. Event persistence

CF-20. Every rejection and every marked request persists exactly one row into
the `firewall_events` table with columns: `id` (uuid text, primary key),
`user_id` (text, null when absent), `username` (text, null when absent),
`api_key_id` (text, null when absent), `api_key_name` (text, null when
absent), `endpoint` (text), `model` (text), `term` (text), `content` (text),
`action` (text, `blocked` or `marked`), `reason` (text, the judge's stated
evidence; empty string for events without a judge verdict), `created_at`
(RFC 3339 UTC text), `created_at_unix_ms` (integer).

CF-21. `endpoint` is one of: `chat_completions`, `responses`, `messages`,
`responses_compact`, `embeddings`, `images_generations`, `images_edits`. The
WebSocket responses path records `responses`. `content` is the scanned string
that triggered the event, truncated to at most 8000 Unicode scalar values,
unchanged otherwise. For a blocked event `term` is the judge category
(CF-28); for a marked event `term` is every canonicalized keyword hit joined
with `、` (CF-30, CF-5a). The events list API returns `content` in full; the dashboard
shows a truncated summary in the table and the full text in a click-open
dialog.

CF-22. A persistence failure MUST NOT change the rejection outcome: the client
still receives the CF-15 response and the failure is logged as a warning.

CF-23. The table retains at most the 50,000 newest rows per process-wide table
state; after each insert the system deletes rows beyond that cap ordered by
`created_at_unix_ms` ascending.

## 7. Firewall dashboard page and APIs

CF-24. `GET /api/dashboard/firewall/stats` (admin session required) returns
JSON: `total` (blocked rows), `marked` (marked rows), `last_24h`, `last_7d`
(blocked rows only), `distinct_users` (over blocked rows), `judge_active`
(boolean: `moderation_enabled` AND `moderation_judge_enabled` AND judge
endpoint fully configured in the current runtime snapshot), `daily` (exactly
14 entries, oldest first, each `{date: "YYYY-MM-DD", count}` of blocked rows
over UTC days ending with the current UTC day), and `top_terms` (at most 10
`{term, count}` entries over blocked rows, descending count, ties broken by
ascending term). All aggregation is computed server-side from retained rows.

CF-25. `GET /api/dashboard/firewall/events` (admin session required) returns
`{data: [rows], total, limit, offset}` ordered by `created_at_unix_ms`
descending over the whole deployment, with query parameters `limit` (default
50, clamped to 1..200), `offset` (default 0), `term` (optional substring
filter), `action` (optional, `blocked` or `marked`), `since_ms` and
`until_ms` (optional inclusive `created_at_unix_ms` bounds). Each row
includes the `reason` field of CF-20, and the dashboard's click-open detail
dialog displays it. Non-admin callers receive HTTP 403.

CF-26. The dashboard renders an admin-only page at `/dashboard/firewall`
(labeled by i18n key `nav.firewall`) containing, in order: a judge-status
banner when `judge_active` is false (the firewall is then inert and blocks
nothing); a stat-tile row (`last_24h`, `last_7d`, `total`, `distinct_users`);
a 14-day bar chart of `daily`; a `top_terms` list; and the events table of
CF-25 with a term search box, an action filter (`all`, `blocked`, `marked`),
and a time-range selector (`24h`, `7d`, `30d`, all), paginated through
`limit`/`offset`. Marked rows are visually distinguished from blocked rows.
Every data region has a skeleton loading state and an empty state; navigation
and data fetching follow SWR per `dashboard-ui-layout.spec.md`.

CF-26a. The events query MUST capture its time-range reference timestamp on page
mount and when the user changes an applied term, action, or range filter. Pagination
and unrelated renders MUST retain that timestamp. The query MUST omit `since_ms`
for the all-time range.

CF-27. The page and both APIs expose no mutation surface: firewall
configuration changes only through the CF-19 settings category.

## 8. Semantic judge (LLM)

CF-33. Judge invocation gate: the judge is invoked if and only if
`moderation_enabled` and `moderation_judge_enabled` are true, the judge is
fully configured (CF-28), and the keyword matcher (CF-5) matched at least one
scanned string. A request with no keyword match is forwarded directly with no
judge call, no added latency, and no event row. Every request with a keyword
hit is judged: there is no session window, sampling, or other mechanism that
skips or defers the judge call for a keyword-bearing request. The keyword
list therefore determines the judge's coverage; the judge alone decides
rejections.

CF-28. When invoked, the firewall sends one isolated judge request: an
OpenAI-compatible `POST {moderation_judge_base_url}/chat/completions` (a
`/v1` suffix on the base URL is stripped before appending
`/chat/completions`) with `model = moderation_judge_model`,
`temperature = 0`, `max_tokens = 512`, `store = false`, and a unique
per-call `user` field of the form `moderation-judge-{uuid}`. The body
MUST contain exactly two messages (`system` then `user`) and MUST NOT
include `conversation_id`, `previous_response_id`, `session_id`, or any
other field that would attach the call to a prior judge turn. The system
prompt instructs the
judge to analyze what the text is trying to accomplish before answering,
to end its reply with exactly one JSON object, and to classify the request
into exactly one category of `{"category": "porn" | "political" | "benign" |
"uncertain", "reason": "<one to three sentences>"}`: `porn` is producing,
continuing, or roleplaying sexually explicit content (zero tolerance for
anything sexualizing minors); `political` is producing politically illegal
content per operator policy (subverting state power, inciting separatism,
extremist propaganda); `benign` is everything else, including news,
education, law-enforcement, academic, technical, and moderation-policy
discussion that merely mentions or prohibits these topics, and agent or
tool system prompts and defensive security policy text; `uncertain` is the
mandatory answer whenever the judge cannot decide, so that a borderline
text is never forced into a blocking category. A keyword hit is a hint
only: the judge MUST NOT treat the presence of a listed term as sufficient
evidence of `porn` or `political`. The `reason` field must
state the concrete evidence for the verdict, written in Simplified
Chinese. The user message contains the
keyword matches (CF-5) followed by the CF-7..CF-12 scanned strings joined
with newlines and truncated to at most 6000 Unicode scalar values in
total. The request carries the loop-guard header of CF-29.

CF-28a. The judge response is parsed by taking the LAST JSON object in the
assistant content that parses and whose `category` is one of the four
values above; its `reason` string (at most 500 Unicode scalar values) is
kept alongside the category. A response that yields no such object, a
non-2xx HTTP status, or a timeout (`moderation_judge_timeout_ms`) is a
judge failure.

CF-29. Judge-loop guard: the process generates a random UUID bypass token at
startup. Judge requests carry the header
`x-monoize-moderation-bypass: <token>`. The firewall skips all checks for an
incoming request bearing this exact header value, so judge traffic routed
back through the gateway cannot recurse. The token is process-local memory
only and is never persisted, logged, or exposed.

CF-30. Marked requests: when the judge verdict is `benign` or `uncertain`
(which by CF-33 implies at least one keyword match), the request is
forwarded normally and exactly one `firewall_events` row with
`action = 'marked'` is persisted (CF-20, CF-21), carrying the judge's
`reason`.

CF-31. Fail-open: if `moderation_enabled` is false, the judge is not enabled
or not fully configured, or a judge failure occurs (CF-28a), the firewall
allows the request. A judge failure on a keyword hit is allowed with a
`tracing::warn` record and no event row. Only `porn` and `political`
verdicts reject; `benign` and `uncertain` always allow. The keyword list
therefore never blocks by itself; the judge is the only decision-maker for
rejections.

CF-32. Judge rejections and marked requests use the judge category and the
first keyword hit respectively as the event `term` and reason, per CF-21 and
CF-15.

CF-JUDGE-KEY. When `MONOIZE_MODERATION_JUDGE_API_KEY` is set to a non-empty value, it MUST override the stored judge credential. The stored value MUST remain the fallback for a deployment that supplies no environment value. A deployment MUST be able to leave the stored credential empty and keep the key only in a mounted secret file, so the credential is never persisted in the database or a database backup.
