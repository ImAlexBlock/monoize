# Auto Cache Transforms Specification

## 0. Status

- Version: `1.6.0`
- Scope: Seven request-phase `cache_*` domain transforms that automatically optimize provider prompt caching by injecting Anthropic `cache_control` markers, OpenAI prompt-cache request fields and content breakpoints, and user identity fields, and by relocating per-request agent metadata out of the cacheable prompt prefix.
- Dependency: URP Transform System (see `urp-transform-system.spec.md`, TF-1 through TF-7b; historical IDs map to the canonical `cache_*` IDs through TF-17).

## 1. Shared Definitions

DEF-1. An **Anthropic cache breakpoint** is any `Node` in `req.input` whose `extra_body` contains a key `"cache_control"`.

DEF-2. The **Anthropic cache breakpoint count** of a request is the total number of Anthropic cache breakpoints across all nodes in `req.input`.

DEF-3. The **Anthropic cache slot count** is the Anthropic cache breakpoint count plus one when `req.extra_body` contains top-level `"cache_control"`. The maximum is `4`. No transform in this specification SHALL increase the slot count beyond `4`.

DEF-4. The **Monoize username** is `req.context.username`. The **Monoize API key ID** is `req.context.api_key_id`. These optional fields come from authenticated runtime state. JSON decoding MUST NOT populate them, and protocol encoders MUST NOT emit them.

DEF-5. The canonical Anthropic cache control value is `{"type": "ephemeral"}`.

DEF-6. An **OpenAI upstream request** is a request attempt whose selected upstream provider type is `responses` or `chat_completion`.

DEF-7. An **OpenAI explicit cache breakpoint** is an `extra_body` key named `"prompt_cache_breakpoint"` on either a node in `req.input` or a nested `ToolResultContent` value in a `Node::ToolResult`.

DEF-8. The **OpenAI explicit cache breakpoint count** is the total number of OpenAI explicit cache breakpoints defined by DEF-7.

DEF-9. The **OpenAI explicit cache breakpoint limit** is `4` when `req.extra_body["prompt_cache_options"]["mode"]` is the string `"explicit"`. The limit is `3` otherwise because the default implicit breakpoint consumes one of the four new cache-write slots.

DEF-10. An **OpenAI explicit-breakpoint model** is a model whose identifier contains a `/`- or `:`-delimited segment with a `gpt-<major>[.<minor>]` prefix, where `major > 5` or `major = 5` and `minor >= 6`. Missing `minor` is treated as `0`.

## 2. `cache_user_id`

### 2.1 Registration

ACUID-1. Transform type ID: `"cache_user_id"`.

ACUID-2. Phase: `Request` only.

ACUID-3. Config schema: empty object, no configuration parameters.

### 2.2 Preconditions

ACUID-4. If `req.context.username` is absent, the transform is a no-op.

ACUID-5. If no Anthropic cache breakpoint exists anywhere in `req.input` (i.e., Anthropic cache breakpoint count = 0), the transform is a no-op.

### 2.3 Behavior

ACUID-6. **Anthropic user ID injection**: The transform MUST ensure `req.extra_body["metadata"]["user_id"]` exists. If `metadata` does not exist in `extra_body`, it MUST be created as `{"user_id": <username>}`. If `metadata` exists but `user_id` is absent, `user_id` MUST be set to `<username>`. If `user_id` already exists, it MUST NOT be overwritten.

ACUID-7. **OpenAI user field injection**: If `req.user` is `None`, it MUST be set to `<username>`. If `req.user` is already `Some(...)`, it MUST NOT be overwritten.

ACUID-8. The transform MUST NOT modify `req.input`, `req.model`, or any node content.

## 3. `cache_anthropic_system`

### 3.1 Registration

ACS-1. Transform type ID: `"cache_anthropic_system"`.

ACS-2. Phase: `Request` only.

ACS-3. Config schema: empty object, no configuration parameters.

### 3.2 Preconditions

ACS-4. If the Anthropic cache slot count is `>= 4`, the transform is a no-op.

ACS-5. If `req.input` contains no node with `role == System` or `role == Developer`, the transform is a no-op.

ACS-6. If the target node (defined in ACS-7) already contains a `"cache_control"` key in its `extra_body`, the transform is a no-op.

### 3.3 Behavior

ACS-7. The **target node** is the last node in `req.input` whose role is `System` or `Developer` (searched via reverse-position scan).

ACS-8. The transform MUST insert `"cache_control": {"type": "ephemeral"}` into the `extra_body` of the target node.

ACS-9. After insertion, the Anthropic cache breakpoint count increases by exactly `1`.

ACS-10. The transform MUST NOT modify any other node, any node content (text, image data, etc.), `req.model`, or `req.user`.

## 4. `cache_anthropic_tool_use`

### 4.1 Registration

ACTU-1. Transform type ID: `"cache_anthropic_tool_use"`.

ACTU-2. Phase: `Request` only.

ACTU-3. Config schema: empty object, no configuration parameters.

### 4.2 Preconditions

ACTU-4. Let `last_node` = the last element of `req.input`. If `last_node` is not `Node::ToolResult`, the transform is a no-op.

ACTU-5. If the Anthropic cache slot count is `>= 4`, the transform is a no-op.

### 4.3 Target Resolution

ACTU-6. The target is `last_node`. A trailing run of tool results MUST be marked only at its final node.

ACTU-7. If `last_node.extra_body` already contains `"cache_control"`, the transform is a no-op.

### 4.4 Behavior

ACTU-9. The transform MUST insert `"cache_control": {"type": "ephemeral"}` into `last_node.extra_body`.

ACTU-10. After insertion, the Anthropic cache breakpoint count increases by exactly `1`.

ACTU-11. The transform MUST NOT modify any other node, any node content, `req.model`, or `req.user`.

## 4a. `cache_anthropic_auto`

ACAA-1. Transform type ID: `"cache_anthropic_auto"`. Phase: `Request` only. Supported scopes: `Provider` and `ApiKey`. Config schema: empty object.

ACAA-2. If the selected upstream provider type is not `messages`, the transform is a no-op.

ACAA-3. If `req.extra_body` already contains `"cache_control"`, the transform is a no-op.

ACAA-4. If the Anthropic cache breakpoint count is `>= 4`, the transform is a no-op.

ACAA-5. Otherwise, the transform MUST set `req.extra_body["cache_control"]` to `{"type": "ephemeral"}`. The Messages encoder MUST emit this field at the top level of the upstream request JSON.

ACAA-6. The transform MUST NOT modify `req.input`, `req.tools`, `req.model`, or `req.user`.

ACAA-7. A gateway that does not accept top-level `cache_control` may reject the upstream request. Operators MUST enable this transform only on Channels that support Anthropic automatic caching.

## 5. Context Injection Lifecycle

CTX-1. Before request-phase transforms execute, the request handler MUST set `req.context.username` to `auth.username`.

CTX-2. Before request-phase transforms execute, the request handler MUST set `req.context.api_key_id` to `auth.api_key_id`.

CTX-3. The runtime context MUST be excluded from JSON deserialization and all upstream encoding. Client keys named `__monoize_username` and `__monoize_api_key_id` MUST NOT influence transform identity or appear upstream.

CTX-4. `auth.username` is populated from `User.username` during API key authentication. If authentication does not resolve to a user record, `auth.username` is `None`.

## 6. `cache_openai_prompt`

### 6.1 Registration

ACOP-1. Transform type ID: `"cache_openai_prompt"`.

ACOP-2. Phase: `Request` only.

ACOP-3. Supported scopes are `Provider`, `Global`, and `ApiKey`.

ACOP-4. Config schema:
- `retention`: optional string, allowed values are `"24h"` and `"in_memory"`, default `"24h"`;
- `key_prefix`: optional string, default `"mzpc"`;
- `key_mode`: optional string, allowed values are `"prefix"` and `"identity"`, default `"prefix"`;
- `include_user_in_key`: optional boolean, default `false`;
- `include_full_input_in_key`: optional boolean, default `false`.

### 6.2 Preconditions

ACOP-5. If the selected upstream provider type is not `responses` and is not `chat_completion`, the transform is a no-op.

ACOP-6. If `req.extra_body["prompt_cache_key"]` exists, the transform MUST NOT overwrite it.

ACOP-7. If `req.extra_body["prompt_cache_retention"]` exists, the transform MUST NOT overwrite it.

### 6.3 Cache key material

ACOP-8. The transform MUST build a JSON object named `key_material`.

ACOP-9. If `key_mode = "prefix"`, `key_material["model"]` MUST equal `req.model` at transform execution time.

ACOP-10. If `key_mode = "prefix"`, `key_material["prefix_nodes"]` MUST be an array containing every leading node from `req.input` whose role is `System` or `Developer`, in original order. The scan MUST stop at the first node that is not a `System` or `Developer` node.

ACOP-11. If `key_mode = "prefix"`, `key_material["tools"]` MUST equal `req.tools` when `req.tools` is present. It MUST be absent when `req.tools` is absent.

ACOP-12. If `key_mode = "prefix"`, `key_material["response_format"]` MUST equal `req.response_format` when `req.response_format` is present. It MUST be absent when `req.response_format` is absent.

ACOP-13. If `key_mode = "prefix"` and `include_user_in_key = true`, `key_material["user"]` MUST equal `req.user` when `req.user` is present. If `req.user` is absent, it MUST equal `req.context.username` when that value is present. If both are absent, `key_material["user"]` MUST be absent.

ACOP-14. If `key_mode = "prefix"` and `include_full_input_in_key = true`, `key_material["input"]` MUST equal `req.input` and `key_material["prefix_nodes"]` MUST be absent.

ACOP-14a. If `key_mode = "identity"`, `key_material` MUST contain exactly:
1. `username`, equal to `req.context.username` when present, otherwise JSON null; and
2. `api_key_id`, equal to `req.context.api_key_id` when present, otherwise JSON null.

ACOP-14b. If `key_mode = "identity"`, `key_material` MUST NOT include `req.model`, `req.input`, `req.tools`, `req.response_format`, `req.user`, or any node content.

### 6.4 Behavior

ACOP-15. If `req.extra_body["prompt_cache_key"]` is absent, the transform MUST serialize `key_material` using deterministic JSON object key ordering, compute xxHash3 128-bit over the serialized bytes, format the digest as 32 lowercase hexadecimal characters, and set `req.extra_body["prompt_cache_key"]` to `<key_prefix>_<digest32>`.

ACOP-16. If `req.extra_body["prompt_cache_retention"]` is absent, the transform MUST set it to the configured `retention`.

ACOP-17. The transform MUST NOT modify `req.input`, node content, `req.tools`, `req.response_format`, `req.model`, or `req.user`.

ACOP-18. The transform is idempotent.

ACOP-19. The transform does not guarantee an OpenAI cache hit. OpenAI prompt caching requires upstream eligibility, a minimum prompt size, and exact prefix compatibility as defined by OpenAI.

## 7. `cache_openai_tool_use`

### 7.1 Registration

ACOTU-1. Transform type ID: `"cache_openai_tool_use"`.

ACOTU-2. Phase: `Request` only.

ACOTU-3. Supported scopes are `Provider`, `Global`, and `ApiKey`.

ACOTU-4. Config schema: empty object, no configuration parameters.

### 7.2 Preconditions

ACOTU-5. If the selected upstream provider type is not `responses`, the transform is a no-op.

ACOTU-6. If `req.model` is not an OpenAI explicit-breakpoint model as defined by DEF-10, the transform is a no-op.

ACOTU-7. Let `last_node` be the last element of `req.input`. If `last_node` is not `Node::ToolResult`, the transform is a no-op.

ACOTU-8. If the OpenAI explicit cache breakpoint count is greater than or equal to the limit defined by DEF-9, the transform is a no-op.

### 7.3 Target Resolution

ACOTU-9. Starting from `last_node`, scan backwards through the contiguous trailing `Node::ToolResult` entries. The first node before this trailing run MUST be `Node::ToolCall`. If this condition is not met, the transform is a no-op.

ACOTU-10. A **tool-result run** is a maximal contiguous sequence of one or more `Node::ToolResult` entries whose immediately preceding node is `Node::ToolCall`. Scan all tool-result runs in reverse request order. Within each run, scan its result nodes and their `content` entries in reverse order. The first content entry in each run that satisfies one of the following conditions is that run's candidate content block:
1. every `ToolResultContent::Text`;
2. `ToolResultContent::Image` with `ImageSource::Url` or `ImageSource::Base64`;
3. `ToolResultContent::Image` with `ImageSource::FileId` whose typed `metadata.resource` provenance is compatible with the Responses protocol;
4. `ToolResultContent::File` with `FileSource::Url` or `FileSource::Base64`; or
5. `ToolResultContent::File` with `FileSource::FileId` whose typed `metadata.resource` provenance is compatible with the Responses protocol.

`ToolResultContent::File` with `FileSource::Text` or `FileSource::Content` and every `ToolResultContent::ProviderItem` are not eligible. A run with no eligible content block produces no candidate. If no run produces a candidate, the transform is a no-op.

ACOTU-11. Let `remaining` equal the limit defined by DEF-9 minus the current OpenAI explicit cache breakpoint count. Starting with the newest candidate and proceeding towards older candidates, select each candidate whose `extra_body` does not already contain `"prompt_cache_breakpoint"` until `remaining` candidates have been selected. A candidate that already contains the key is preserved and skipped.

### 7.4 Behavior

ACOTU-12. The transform MUST insert `"prompt_cache_breakpoint": {"mode": "explicit"}` into the `extra_body` of every selected candidate content block.

ACOTU-13. After insertion, the OpenAI explicit cache breakpoint count increases by exactly the number of selected candidates and does not exceed the limit defined by DEF-9.

ACOTU-14. The transform MUST NOT set `"prompt_cache_breakpoint"` on the outer `Node::ToolResult.extra_body`.

ACOTU-15. For a Responses request, the encoder MUST emit every selected candidate as an `input_text`, `input_image`, or `input_file` block inside the corresponding `function_call_output.output` or `custom_tool_call_output.output` array. Each emitted content block MUST contain the inserted `"prompt_cache_breakpoint"` value.

ACOTU-16. The transform MUST NOT create or modify `req.extra_body["prompt_cache_options"]`. An existing request-wide mode and TTL MUST remain unchanged.

ACOTU-17. The transform MUST NOT modify any node content, `req.model`, `req.tools`, `req.response_format`, or `req.user`.

ACOTU-18. The transform is idempotent.

### 7.4 Upstream rejection fallback

DEF-10 selects models by identifier only. A relay can serve a DEF-10 model identifier from a backend that rejects the field. This section keeps such a request usable.

ACOTU-19. An **explicit-breakpoint rejection** is a non-2xx upstream response with HTTP status `400` whose structured error `param` equals `"prompt_cache_breakpoint"`, or whose structured error `message` contains the substring `"prompt_cache_breakpoint"`.

ACOTU-20. **Breakpoint stripping** removes the object key `"prompt_cache_breakpoint"` from exactly these locations of a JSON request body: each element of the top-level `input` array; each element of an `input[i].content` or `input[i].output` array; each element of the top-level `messages` array; each element of a `messages[i].content` array. No other key is removed and no other value changes. A string value that contains the substring is not changed.

ACOTU-21. When one upstream JSON POST receives an explicit-breakpoint rejection and breakpoint stripping removes at least one key from the sent body, the upstream client MUST send the stripped body once more to the same URL, with the same headers and the same timeout value, before any byte reaches the downstream client. The resend result replaces the rejection. The resend MUST NOT count as a separate routing attempt, a same-channel retry, or a channel health failure. When stripping removes no key, the rejection is returned unchanged and no resend occurs.

ACOTU-22. After ACOTU-21 sends a stripped body, the pair (upstream `base_url`, body `model` string; an absent `model` is the empty string) MUST be recorded as breakpoint-unsupported in process memory for 3600 seconds. While a pair is recorded, the upstream client MUST apply breakpoint stripping to the body before the first send, and ACOTU-21 does not apply to that send. The record is not persisted and is not shared between processes.

ACOTU-23. ACOTU-21 and ACOTU-22 apply to every JSON POST sent through the shared upstream client, streaming and non-streaming, independent of which transform or client produced the key.

ACOTU-24. A streaming upstream can answer HTTP `200` and report the rejection in the event stream instead. For a streaming JSON POST whose sent body contains at least one key that breakpoint stripping would remove, the upstream client MUST read the successful response body until one of these conditions holds, before it returns the response to the caller:

1. The buffered bytes contain a complete SSE event (terminated by an empty line) whose `event` field is not `error` and not `response.failed`, and whose `data` JSON has no top-level `error` object and a `type` other than `error` and `response.failed`;
2. The buffered bytes contain a complete SSE event that is an **in-stream breakpoint rejection**: its `event` field or `data.type` is `error` or `response.failed`, and the error object (`data.error`, else `data.response.error`, else `data`) has `param` equal to `"prompt_cache_breakpoint"` or a `message` containing `"prompt_cache_breakpoint"`;
3. The body ends, 65536 bytes are buffered, or 30 seconds pass since the response headers arrived.

In case 2 the upstream client MUST discard the buffered response and continue exactly as ACOTU-21 and ACOTU-22 specify for an HTTP rejection. In cases 1 and 3 the upstream client MUST return a response whose body yields the buffered bytes first, unchanged and in order, followed by the remaining upstream body. A request body without a strippable key MUST NOT be buffered.

## 7A. `cache_prefix_stabilize`

### 7A.1 Motivation

An upstream with implicit prefix caching matches on the exact token prefix. One changed
token invalidates the cache from that token onward, and only from that token onward.
Measured against `api.vectron.meta-stone.com` with model `ZhipuAi/GLM-5.3`, one 2,900-token
system prompt, and a five-turn growing conversation, a single line whose value changes per
request produced these cache-read rates:

| Position of the changing line | Turn 1 | 2 | 3 | 4 | 5 |
| --- | ---: | ---: | ---: | ---: | ---: |
| absent | 0% | 93.7% | 91.8% | 89.9% | 96.0% |
| after the first two lines | 0% | 0% | 0% | 0% | 0% |
| after the stable instruction text | 0% | 92.7% | 90.9% | 89.1% | 87.2% |
| moved to the end of the system prompt | 0% | 92.8% | 90.9% | 89.0% | 87.2% |

Coding agents place such content near the top of the system prompt: Claude Code emits an
`<env>` block and an `x-anthropic-billing-header` line, Codex CLI emits
`<environment_context>`, and Cursor emits `<user_info>` and `<timestamp>`.

Row 4 of the table is not a sufficient postcondition. An implicit prefix cache matches from
the first token of the serialized upstream request. A volatile block at the end of the
system prompt still sits in front of every User, Assistant, and ToolResult node. Measured
on `DeepSeek/DeepSeek-V4-Pro-0813` over `chat_completion`, a sibling session of about 40,000
input tokens on the same channel read 99% cache, while a session whose input grew past
70,000 tokens pinned `cache_read_tokens` at 2,048.

A trailing `System` node is also not sufficient. The Gemini encoder concatenates every
`System` and `Developer` node into `systemInstruction` at the top of the request. The
Responses encoder lifts the first `System` or `Developer` node into `instructions` at the
top of the request. Either path puts the volatile block back in front of the conversation.

This transform therefore moves the volatile content out of the leading system run and onto
a trailing `User` node after every existing input node. Chat Completions, Responses, and
Gemini all leave a trailing `User` node at the end of the serialized request, so the
conversation remains a cacheable prefix.

### 7A.2 Registration

ACPS-1. Transform type ID: `"cache_prefix_stabilize"`.

ACPS-2. Phase: `Request` only.

ACPS-3. Supported scopes are `Provider`, `Global`, and `ApiKey`.

ACPS-4. Config schema:
- `action`: optional string, allowed values are `"relocate"` and `"strip"`, default
  `"relocate"`;
- `blocks`: optional array of objects with exactly the required string keys `open` and
  `close`. An absent `blocks` means the built-in block set of ACPS-8. An empty array
  disables block matching;
- `line_prefixes`: optional array of strings. An absent `line_prefixes` means the built-in
  line set of ACPS-9. An empty array disables line matching;
- `stabilize_user_preamble`: optional boolean, default `true`. Extends the extraction
  region per ACPS-22.

ACPS-5. `parse_config` MUST reject a `blocks` entry whose `open` or `close` is empty or
whitespace-only, and MUST reject a `line_prefixes` entry that is empty or whitespace-only.

### 7A.3 Preconditions

ACPS-6. If the request is not an OpenAI upstream request as defined by DEF-6, the transform
is a no-op. An Anthropic upstream caches at a whole-node `cache_control` breakpoint, so
reordering text inside a node cannot move the cached boundary.

ACPS-7. The **stable prefix** is the leading run of nodes in `req.input` whose role is
`System` or `Developer`, using the same scan rule as ACOP-10. If that run is empty, the
transform is a no-op.

### 7A.4 Volatile segment matching

ACPS-8. The built-in block set is exactly, in this order:

| `open` | `close` |
| --- | --- |
| `<env>` | `</env>` |
| `<environment_context>` | `</environment_context>` |
| `<user_info>` | `</user_info>` |
| `<timestamp>` | `</timestamp>` |

ACPS-9. The built-in line set is exactly `x-anthropic-billing-header:`.

ACPS-10. Matching is line-oriented. For each `Node::Text` in the stable prefix, split its
`content` on `\n` into lines and evaluate lines in ascending order:

1. A line whose whitespace-trimmed form starts with any configured line prefix is a
   **volatile line**.
2. Otherwise, if a line's whitespace-trimmed form starts with a configured `open`, search
   forward from that line, inclusive, for the first line whose whitespace-trimmed form ends
   with the matching `close`. When such a line exists, every line from the opening line
   through that line, inclusive, is a volatile line, and evaluation resumes after it.
3. When no such closing line exists in the same node, the opening line is NOT a volatile
   line and evaluation continues at the following line.

ACPS-11. Rule 3 of ACPS-10 is required: treating an unclosed delimiter as a match would move
the remainder of a system prompt on a single malformed marker.

ACPS-12. A node that is not `Node::Text` is skipped. A node outside the stable prefix is
never read or written, so a `User` or `Assistant` node whose own content contains a listed
delimiter MUST remain byte-identical.

### 7A.5 Behavior

ACPS-13. Let `volatile` be the concatenation, in request order and then in line order, of
every volatile line found in the stable prefix. If `volatile` is empty, the transform is a
no-op.

ACPS-14. For every `Node::Text` in the stable prefix, its `content` MUST become the
remaining lines joined by a single `\n`, with leading and trailing `\n` characters removed.

ACPS-15. When `action = "relocate"`, after ACPS-14 the transform MUST append exactly one new
`Node::Text` to `req.input` with `role = User` and `content` equal to `volatile` joined by a
single `\n`. The new node MUST be the last node of `req.input`. The transform MUST NOT
reinsert `volatile` into any node that was already in `req.input`. The transform MUST NOT
use `role = System` or `role = Developer` for this trailing node: those roles are hoisted
in front of the conversation by the Gemini encoder and, for the first such node, by the
Responses encoder.

ACPS-16. When `action = "relocate"`, the multiset of non-empty lines across `req.input` MUST
be unchanged. The transform MUST NOT delete, add, or edit a line. Moving a line from a
stable-prefix node into the trailing `User` node required by ACPS-15 is a permitted
reordering.

ACPS-17. When `action = "strip"`, no volatile line is reinserted.

ACPS-18. After ACPS-15 or ACPS-17, every node in the stable prefix that is a `Node::Text`
with empty `content` MUST be removed from `req.input`. A node that was already outside the
stable prefix MUST NOT be removed.

ACPS-19. The transform MUST NOT modify `req.model`, `req.tools`, `req.response_format`,
`req.user`, any `extra_body`, or the content of any node that was already outside the stable
prefix. ACPS-15 is the only permitted insertion: one new trailing `User` node.

ACPS-20. The transform is idempotent. After a successful relocate, the trailing `User` node
sits outside the stable prefix defined by ACPS-7, so a second application finds no volatile
line in the prefix and is a no-op.

ACPS-21. The transform does not guarantee a cache hit. An upstream cache hit additionally
requires upstream eligibility, a minimum prompt size, and a stable prefix in the caller's
own content.

ACPS-22. When `stabilize_user_preamble = true` (the default) and the node immediately after
the stable prefix is a `Node::Text` with `role = User`, that one node joins the extraction
region of ACPS-13 through ACPS-19 in place of the system run alone. Volatile blocks and
lines are matched inside it with the same rules as ACPS-10, extracted, and relocated or
stripped identically. Agents such as hermes front-load per-request metadata (clock,
environment snapshot, run id) into the first user message instead of the system prompt;
without this rule the implicit prefix cache diverges at the first conversation node and
only the system region is ever read from cache. Measured on
`DeepSeek/DeepSeek-V4-Flash` over `chat_completion` via `api.vectron.meta-stone.com`
on 2026-09-19: append-only conversations of 100K-180K input tokens read only 5,888-8,192
cache tokens (3-4%), matching the system-prompt size, while a sibling client whose
preamble is stable read 94-99%.

ACPS-23. Only the FIRST User node after the stable prefix is eligible. A User node that
follows another conversation node is never read or written, so ACPS-12's protection for
ordinary conversation content is unchanged.

ACPS-24. When `stabilize_user_preamble = false`, behavior is exactly the pre-ACPS-22
transform: the extraction region is the stable prefix alone.

ACPS-25. Extraction from the user preamble shares idempotency with ACPS-20: after a
successful relocate the extracted lines sit in the trailing User node, which is not the
first User node after the stable prefix, so a second application finds nothing to
extract.

## 8. Transform Ordering Guidance

ORD-1a. `cache_anthropic_auto` consumes one Anthropic cache slot. Operators MAY combine it with at most three explicit node breakpoints.

ORD-1. `cache_anthropic_system` SHOULD be ordered before `cache_anthropic_tool_use` in the transform rule list, so that system prompt caching takes priority when approaching the 4-breakpoint limit.

ORD-2. `cache_user_id` has no ordering dependency relative to the other transforms; it does not consume cache breakpoints.

ORD-3. The per-attempt cross-protocol strip of nested `extra_body` (see provider setting `strip_cross_protocol_nested_extra`) MUST run BEFORE any request-phase transform (provider, global, and API-key scopes) within the same attempt. This guarantees that `cache_control` markers produced by `cache_anthropic_system` / `cache_anthropic_tool_use` on part-level `extra_body` survive into the encoded upstream request, even when the downstream and upstream protocol families differ (e.g. downstream OpenAI Responses → upstream Anthropic Messages).

ORD-4. As a consequence of ORD-3, request-phase transforms may be invoked more than once per request across multiple attempts. Each invocation operates on an independent clone of the originally-decoded URP request, so `cache_*` idempotency (INV-4) is sufficient to keep behavior deterministic; non-idempotent transforms MUST likewise produce the same result when applied once to a fresh clone, which is the only pattern exercised here.

ORD-5. `cache_openai_prompt` SHOULD run after transforms that modify the stable prompt prefix, tool definitions, or response format. This ensures the generated `prompt_cache_key` reflects the upstream request shape after those mutations.

ORD-6. `prompt_strip_anthropic_billing_header` SHOULD run before `cache_openai_prompt` when both transforms are enabled. This ensures the generated `prompt_cache_key` and the OpenAI upstream prompt omit Claude Code's per-request billing marker.

ORD-7. `cache_openai_tool_use` SHOULD run before `cache_openai_prompt` when `cache_openai_prompt.include_full_input_in_key = true`. This ensures the generated key material includes the inserted content breakpoint.

ORD-8. `cache_prefix_stabilize` SHOULD run before `cache_openai_prompt`. `cache_openai_prompt` builds its key material from the stable prefix nodes (ACOP-10), so running it first would hash the volatile content that `cache_prefix_stabilize` is about to move and would produce a different `prompt_cache_key` on every request.

ORD-9. `cache_prefix_stabilize` with the built-in line set makes `prompt_strip_anthropic_billing_header` redundant for an OpenAI upstream request, because the billing line is removed from the stable prefix (and, under `relocate`, appended as a trailing `User` node) instead of deleted. Enabling both is permitted: whichever runs first removes the line from the prefix, and the other then finds nothing to act on.

## 9. Invariants

INV-1. No transform in this specification SHALL increase the Anthropic cache slot count above `4`. Existing requests above the limit MUST remain unchanged by cache-marker insertion.

INV-2. No transform in this specification shall produce a request whose OpenAI explicit cache breakpoint count exceeds the limit defined by DEF-9.

INV-3. No transform in this specification shall overwrite an existing `cache_control`, `prompt_cache_breakpoint`, `metadata.user_id`, `req.user`, `prompt_cache_key`, or `prompt_cache_retention` value.

INV-4. All seven transforms are idempotent: applying the same transform twice to the same request produces the same result as applying it once.
