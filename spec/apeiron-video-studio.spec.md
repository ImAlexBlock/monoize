# Apeiron Video Studio Specification

## 0. Status

- **Subsystem:** Apeiron — the standalone AI video creation website (Jimeng/libtv
  style consumer surface, ComfyUI-style node workbench, MoneyPrinterTurbo-style
  one-click pipeline).
- **Scope:** `apeiron/server` (Rust), `apeiron/worker` (Go), `apeiron/web` (React SPA).
- **Dependency:** `studio-bridge.spec.md` (auth handoff + wallet settlement),
  `frontend-design-system.spec.md` (visual tokens; reused verbatim).

## 1. Topology

AP-1. Apeiron is a separate deployable on a separate origin. It owns its own
database, asset store, and provider registry. The ONLY couplings to the platform are:

1. Inbound redirect: platform `GET /studio-entry` → `GET {APEIRON_ORIGIN}/handoff?token=<t>`.
2. Outbound bridge calls: `POST /api/studio-bridge/exchange`,
   `GET /api/studio-bridge/balance`, `POST /api/studio-bridge/debit|refund`
   (service bearer `APEIRON_BRIDGE_SERVICE_TOKEN`, platform base
   `APEIRON_PLATFORM_URL`).

AP-2. The platform never calls Apeiron. Apeiron never reads the platform database.

AP-3. Components:

| Component | Language | Role |
|---|---|---|
| `apeiron/server` | Rust (axum, sqlx) | SPA host, sessions, projects/graphs, run engine, provider adapters, worker queue, SSE |
| `apeiron/worker` | Go | Claimed jobs: TTS synthesis, stock material fetch, subtitle build, ffmpeg assembly |
| `apeiron/web` | React 19 + Vite | All pages, the node canvas (`@xyflow/react`) |

AP-4. `apeiron/web` MUST expose `typecheck` and `build` scripts that use its declared
TypeScript and Vite dependencies. It MUST NOT expose a lint script unless the package
declares the linter dependency and contains its configuration.

## 2. Authentication

AP-A1. `POST /api/auth/exchange` body `{"token": <handoff token>}` → server calls the
platform bridge exchange. On success the server upserts the mirror row in `users`,
mints an opaque session token (`apeiron_session_<uuid-nodashes>`), stores its SHA-256
hash in `sessions` with TTL 7 days, and sets cookie `apeiron_session`
(`HttpOnly; SameSite=Lax; Secure when request scheme is https; Path=/`). Responds
`{"user": {...}, "balance_nano_usd": string}`. Exchange failures propagate their
bridge status (400/409/410/401).

AP-A2. All `/api/*` user endpoints (except `/api/auth/exchange`, `/api/healthz`) require
the session cookie or `Authorization: Bearer <session token>`; lookup is by SHA-256
hash with lazy expiry deletion. Unauthorized → HTTP 401 `{"error":{"code":"unauthorized"}}`.

AP-A3. `GET /api/me` returns `{user, balance_nano_usd, balance_unlimited}`; the balance
is the platform value mirrored with max age 30 s (re-fetched through the bridge on
staleness or on `?refresh=1`).

AP-A4. `POST /api/auth/logout` deletes the session row and clears the cookie.
Platform admins (mirror `role` satisfying `can_manage_users`) are also Apeiron
operators (see §8 admin surface).

AP-A5. `GET /handoff?token=...` serves the SPA entry; the SPA reads `token` from the
query, calls `/api/auth/exchange`, then routes to `/` on success or renders the
handoff failure state with a "back to platform" link (`APEIRON_PLATFORM_URL`).

## 3. Data model (SQLite, file `apeiron/server/migrations/0001_init.sql`)

AP-D1. Tables:

| Table | Key columns |
|---|---|
| `sessions` | `id` PK, `user_id`, `token_hash` UNIQUE, `created_at`, `expires_at` (RFC3339) |
| `users` (mirror) | `id` PK, `username`, `display_name`, `role`, `balance_nano_usd` TEXT, `balance_synced_at` |
| `projects` | `id` PK (uuid), `user_id`, `title`, `graph_json` TEXT, `version` INT, `template_id` NULL, `created_at`, `updated_at` |
| `templates` | `id` TEXT PK, `source` (`builtin`\|`custom`), `name`, `description`, `graph_json`, `params_json`, `enabled` INT, `sort` INT, `created_at`, `updated_at` |
| `runs` | `id` PK, `user_id`, `project_id` NULL, `kind` (`graph`\|`oneclick`), `status`, `error` NULL, `params_json`, `created_at`, `updated_at`, `finished_at` NULL |
| `steps` | `id` PK, `run_id`, `node_id`, `kind`, `status`, `payload_json`, `result_json` NULL, `error` NULL, `attempts` INT, `charge_nano_usd` TEXT, `refund_nano_usd` TEXT, `upstream_kind` NULL, `upstream_ref` NULL, `created_at`, `updated_at`, `finished_at` NULL |
| `assets` | `id` PK, `user_id`, `run_id` NULL, `step_id` NULL, `kind` (`image`\|`video`\|`audio`\|`subtitle`), `file_path`, `mime_type`, `bytes` INT, `meta_json`, `created_at` |
| `providers` | `id` TEXT PK, `kind` (`llm`\|`image`\|`video`\|`tts`\|`material`), `upstream_kind` (`openai`\|`openai_video`\|`fal_video`\|`replicate`\|`openai_tts`\|`pexels`), `name`, `base_url`, `api_key`, `model` NULL, `params_json`, `enabled` INT, `weight` INT, `created_at`, `updated_at` |
| `jobs` | `id` PK, `step_id`, `run_id`, `kind` (`tts`\|`material`\|`subtitle`\|`assemble`), `status` (`queued`\|`claimed`\|`running`\|`succeeded`\|`failed`), `payload_json`, `result_json` NULL, `error` NULL, `worker_id` NULL, `attempts` INT, `heartbeat_at` NULL, `created_at`, `updated_at` |
| `settings` | `key` PK, `value` TEXT, `updated_at` |
| `node_packs` | `id` TEXT PK, `name`, `version`, `description`, `nodes_json`, `enabled` INT, `source` (`builtin`\|`custom`) |

AP-D2. `projects.version` starts at 1, increments by exactly 1 per accepted graph
mutation; a save carrying `version <= current` is rejected with HTTP 409 and the
current graph in the body.

AP-D3. Timestamps are RFC3339 UTC strings. Monetary amounts are i64 nano-USD
(i128 internally), persisted as decimal strings.

## 4. Graph model

AP-G1. Graph JSON: `{"version": int, "nodes": [...], "edges": [...]}`. Node:
`{"id": string, "kind": string, "x": f64, "y": f64, "params": object}`. Edge:
`{"id": string, "source": string, "source_port": string, "target": string,
"target_port": string}`.

AP-G2. Node kinds, ports, and semantics:

| Kind | Input ports | Output port | Execution |
|---|---|---|---|
| `note` | — | — | free; rendered as dashed note, never executed |
| `script` | — | `text` | LLM chat completion of `params.prompt` (+`params.system`); output text |
| `storyboard` | `text` | `shots` | LLM returns JSON array `[{key, description, narration, keywords, duration_secs}]`, count from `params.count` (1..=24, default 6); downstream per-shot nodes fan out once per element |
| `image` | `text` \| `shots` | `image` | text→image via an `image` provider; fan-out per shot when fed `shots`; `params.size` default `1280x720` |
| `video` | `text` (+`image`) | `video` | text(±image)→video via a `video` provider; fan-out per shot when fed `shots`; `params.seconds` default `"5"`, `params.size` default `1280x720` |
| `tts` | `text` \| `shots` | `audio` | one audio track for the full narration (shot narrations joined with `params.joiner`, default `"\n\n"`); `params.voice` default `alloy`, `params.speed` default 1.0 |
| `subtitle` | `text`(+`audio`) | `subtitle` | SRT built from sentences; per-line duration proportional to character weight over `audio.duration_secs` (or 3 s/line when no audio) |
| `material` | `text` \| `shots` | `video` | stock clip per shot by `keywords` via a `material` provider (Pexels) or `params.url`; fan-out per shot |
| `assemble` | `clips` (repeated), `audio`?, `subtitle`? | `video` | ffmpeg assembly (§6) |

AP-G3. Validation on save: node `kind` ∈ AP-G2 table; edge endpoints exist and
ports match the table (input side `text` accepts `text` or `shots` producers);
graph is acyclic. Violations → HTTP 400 `invalid_graph`.

AP-G4. The canvas MAY contain multiple independent subgraphs; a run executes
only nodes reachable from the run's selected root node (`params.root`) — default
root selection: the `assemble` node if present, else every sink node.

## 5. Run engine (Rust)

AP-E1. Run statuses: `queued`, `running`, `succeeded`, `failed`, `partial`,
`canceled`. Step statuses: `pending`, `running`, `succeeded`, `failed`,
`canceled`, `skipped`.

AP-E2. On run creation the engine materializes steps: one per reachable
non-`note` node, EXCEPT per-shot fan-out nodes which create one step per shot at
first dispatch (payload carries `shot_index` and the shot object). Per-shot
steps are independent: a failed shot records `failed` without blocking
siblings; the run ends `partial` when shots have mixed outcomes.

AP-E3. Execution order: topological over edges; a step dispatches when all its
upstream non-skipped steps are `succeeded` and it has no in-flight dependency.

AP-E4. Step timeouts: `llm` 120 s, `image` 300 s, `video` 1800 s, `tts` 300 s,
`material` 300 s, `subtitle` 60 s, `assemble` 1800 s. Overridable via
`APEIRON_<KIND>_TIMEOUT_MS` (positive integers).

AP-E5. Admission: per-user non-terminal runs ≤ `APEIRON_USER_ACTIVE_RUNS`
(default 2); process-wide ≤ `APEIRON_GLOBAL_ACTIVE_RUNS` (default 8). Exceeding
→ HTTP 429 `studio_busy` at submission.

AP-E6. Scheduler: 1 s tick. With zero dispatchable steps and zero running
steps the tick issues no upstream request and sets no timer beyond the tick
itself. Upstream polling backoff: 3 s growing 1.5× capped at 15 s.

AP-E7. Upstream adapters (`openai_video`, `fal_video`, `replicate`) reuse the
wire contracts of `openai-video-upstream.spec.md`, `fal-video-upstream.spec.md`,
`replicate-upstream.spec.md` §10 against Apeiron's own `providers` rows.
`openai` image steps use the OpenAI Images shape; `openai` LLM steps use chat
completions. Provider selection: enabled rows of the matching `kind`, ordered
by `weight DESC`, tried in order, max 2 submit attempts per step; after an
upstream accepts (job id exists) no automatic resubmission occurs.

AP-E8. Cancellation: `pending`/`queued` steps cancel locally; `running` video
steps trigger best-effort upstream cancel without waiting; worker jobs are
abandoned (result rejected after cancel).

## 6. Worker protocol (Go)

AP-W1. Auth: `Authorization: Bearer <APEIRON_WORKER_TOKEN>` on every call. The
server rejects with 401 otherwise. Worker base URL `APEIRON_SERVER_URL`.

AP-W2. Endpoints (worker namespace):

- `POST /api/worker/claim` body `{"worker_id", "kinds": [...]}` →
  `{"job": {id, kind, payload, step_id, run_id} | null}`. Claim = atomic
  `queued→claimed` (or reclaim of `running` whose `heartbeat_at` is older than
  120 s and `attempts < 2`, incrementing `attempts`).
- `POST /api/worker/jobs/{id}/heartbeat` → marks `running`, sets `heartbeat_at`.
- `POST /api/worker/jobs/{id}/result` multipart `result` (JSON string field) +
  optional `file` part → job `succeeded`; a provided `file` is stored as a new
  asset row bound to the job's step.
- `POST /api/worker/jobs/{id}/fail` body `{"error"}` → job `failed`.
- `GET /api/worker/files/{asset_id}` → streams a previously stored asset
  (worker fetches inputs for assembly).

AP-W3. Heartbeat cadence: every 30 s while working. A job with no heartbeat for
120 s and `attempts >= 2` transitions `failed` with error `worker_lease_lost`.

AP-W4. Job payloads and outputs:

| Kind | Payload | Output |
|---|---|---|
| `tts` | `{text, voice, speed, provider: {base_url, api_key, model}}` | audio file (OpenAI `/v1/audio/speech` shape, mp3) |
| `material` | `{query, url?, provider: {api_key, source}}` | video file (Pexels search top hit by `query`, or direct `url` download; ≤ 60 s clip) |
| `subtitle` | `{lines: [{text, weight}], total_duration_secs}` | `result.subtitle_srt` (string) stored as `subtitle` asset |
| `assemble` | `{clips: [{asset_id, kind: image\|video, duration_secs}], audio_asset_id?, subtitle_asset_id?, resolution, fps}` | final mp4 |

AP-W5. ffmpeg assembly contract (Go worker): image clips become still segments
(`-loop 1 -t <duration>`); video clips pass through (re-encoded to the common
`resolution`/`fps`, H.264 + AAC); the audio track, when present, is
normalized to clip total duration (`-shortest` on the mix); subtitles are
burned with `subtitles=` filter when a subtitle asset exists. Failure of ffmpeg
(non-zero exit) fails the job with the last 2 KiB of stderr in the error.

## 7. Billing

AP-B1. Prices are i64 nano-USD from `settings` (operator-editable), defaults:

| Key | Default | Applies to |
|---|---|---|
| `price_image_nano` | 4_000_000 ($0.04) | per image step |
| `price_video_nano` | 500_000_000 ($0.50) | per video step |
| `price_tts_nano` | 20_000_000 ($0.02) | per tts step |
| `price_assemble_nano` | 10_000_000 ($0.01) | per assemble step |
| `price_material_nano` | 0 | per material step |
| `price_subtitle_nano` | 0 | per subtitle step |

AP-B2. LLM steps (`script`, `storyboard`) charge by usage: `input_tokens ×
provider.params.in_price_nano_per_mtok + output_tokens ×
provider.params.out_price_nano_per_mtok` (i64 nano, ceil), post-charged on
completion. All other billable kinds pre-charge on `pending→running`.

AP-B3. Every charge/refund goes through the platform bridge with
`idempotency_key = "apeiron_step_<step_id>"` (pre-charge), `"apeiron_step_llm_<step_id>"`
(settlement), and refunds `"apeiron_refund_<step_id>"`. Steps are refunded at
most once in full when ending `failed` or `canceled` after a charge.

AP-B4. Insufficient balance fails the step before dispatch with
`insufficient_balance` (no charge, no ledger row).

AP-B5. Mirror accounting: after each successful bridge debit/refund the server
updates `users.balance_nano_usd` from the bridge-returned balance.

## 8. HTTP API surface

AP-H1. User endpoints (session auth, prefix `/api`):

- `POST /auth/exchange`, `POST /auth/logout`, `GET /me?refresh=`
- `GET|POST /projects`, `GET|DELETE /projects/{id}`, `PUT /projects/{id}/graph`
  (body `{"version": int, "graph": {...}}`)
- `POST /projects/{id}/run` body `{"root"?: string}` → creates a `graph` run
- `POST /runs/oneclick` body `{"topic", "style", "voice", "duration_target",
  "material_mode": "ai_image"|"stock", "subtitle": bool, "title"?}` → creates
  the project from builtin template `oneclick-standard` and starts a `oneclick` run
- `GET /runs`, `GET /runs/{id}` (embeds steps), `POST /runs/{id}/cancel`
- `GET /assets?kind=`, `GET /assets/{id}/content` (streamed), `DELETE /assets/{id}`
- `GET /templates`
- `GET /events` — SSE (`run_update`, `step_update`, `job_update`, scoped to the session user)
- `GET /node-packs`, `GET /catalog/nodes` — node schema for the canvas palette

AP-H2. Operator endpoints (admin role): `GET|POST /admin/providers`,
`PUT|DELETE /admin/providers/{id}`, `GET|PUT /admin/settings` (price table +
defaults), `GET /admin/runs`, `POST /admin/runs/{id}/cancel`,
`GET|POST /admin/templates`, `PUT|DELETE /admin/templates/{id}`.

AP-H3. Errors use `{"error": {"code", "message"}}`; codes include `unauthorized`,
`not_found`, `invalid_graph`, `version_conflict`, `insufficient_balance`,
`studio_busy`, `no_provider`, `bridge_unavailable`.

AP-H4. `GET /api/healthz` returns `{"ok": true}` unauthenticated.

## 9. Pages (SPA)

AP-U1. Routes: `/` landing, `/handoff`, `/create` (one-click), `/projects`,
`/canvas/:projectId`, `/templates`, `/assets`, `/runs`, `/manager`, `/admin`
(operators), `/settings`. Shell: sticky top bar (`border-b bg-background/92
backdrop-blur-xl`) with logo tile, nav pills, balance chip (mono font, refreshes
from `/api/me`), top-up button linking to `{APEIRON_PLATFORM_URL}/dashboard/wallet`
in a new tab, theme toggle, user menu with logout.

AP-U2. Visual language: the tokens, fonts, focus ring, grid texture, numbered
mono kickers, motion presets, and component rules of
`frontend-design-system.spec.md` are reused verbatim; no new hues, no new fonts.
Dark mode via `class` strategy with pre-paint init. All copy through i18next with
locales `en`, `zh`, `zh-TW`, `ja`; product nouns stay canonical English
(Provider, Channel, node kind ids, env names).

AP-U3. Landing `/`: hero on the drifting grid texture with `font-display`
headline, numbered mono kickers (`01 · CREATE` …), template showcase band
(hairline cell grid), feature band, final CTA to `/create`.

AP-U4. `/create` (one-click): a form (topic textarea, style select, voice,
duration, material mode, subtitle switch) with a live cost estimate computed
from the price table (`estimate = image_price × shot_count + tts + assemble`,
shot count = `duration_target / 5` rounded, clamped 1..=12); submit → project +
run; a stepper renders stage progress (script → storyboard → shots → audio →
assembly) from run SSE; success shows the player with download and "open in canvas".

AP-U5. `/canvas/:id`: full-height React Flow workbench — dotted background,
minimap, controls; node palette grouped by node pack; custom node cards
(`Card` surfaces with icon, status border color: queued `border-muted-foreground/30`,
running `border-primary/60`, succeeded `border-success/60`, failed
`border-destructive/60`); per-node param editors (double-click opens a Dialog);
ports on edges per AP-G2; run button executes AP-G4 root selection; live status
via SSE; outputs preview inline (image thumb, video/audio player). Autosave on
mutation with debounce 800 ms via `PUT /projects/{id}/graph` handling 409 by
reloading server state.

AP-U6. `/templates`: builtin + custom template cards; "use" creates a project
from the template graph; import from pasted graph JSON (validated client-side
then server-side per AP-G3).

AP-U7. `/assets` and `/runs`: card grid with kind filter; runs list with status,
cost (nano-USD → display USD), per-step drill-in, cancel button.

AP-U8. `/manager`: node packs list (enable state, version, contained node kinds)
in the ComfyUI-Manager spirit, plus system status (worker heartbeat recency,
provider enable states, engine tick). `/admin`: provider CRUD (kind, upstream
kind, base URL, key, model, params JSON, weight), price table editor, custom
templates.

AP-U9. Every data-fetching surface uses SWR with skeleton fallbacks; mutations
are optimistic where reversible (project rename, node move autosave, asset delete).

## 10. Assets

AP-X1. Asset files live under `APEIRON_ASSETS_DIR` (default `./data/assets`) as
`<yyyy>/<mm>/<asset_id>.<ext>`; the id-token path is unguessable; listing is
per-user scoped; `GET /assets/{id}/content` streams with correct MIME and
`Cache-Control: public, max-age=31536000, immutable`. Uploads (none in v1 user
surface) capped at `APEIRON_ASSET_MAX_BYTES` (default 209715200).

## 11. Configuration (Apeiron)

| Variable | Default | Meaning |
|---|---|---|
| `APEIRON_LISTEN` | `127.0.0.1:8090` | HTTP listen address |
| `APEIRON_DATABASE_DSN` | `sqlite://./data/apeiron.db` | SQLite DSN (v1: SQLite only) |
| `APEIRON_PLATFORM_URL` | `http://127.0.0.1:8080` | platform origin for user-facing links |
| `APEIRON_BRIDGE_URL` | = `APEIRON_PLATFORM_URL` | base URL for server-to-server bridge calls (set to the platform loopback when the public origin fronts a CDN) |
| `APEIRON_BRIDGE_SERVICE_TOKEN` | unset → user runs fail at first billing call | service bearer for bridge |
| `APEIRON_WORKER_TOKEN` | unset → worker endpoints 404 | worker bearer |
| `APEIRON_ASSETS_DIR` | `./data/assets` | asset storage root |
| `APEIRON_USER_ACTIVE_RUNS` | 2 | per-user admission |
| `APEIRON_GLOBAL_ACTIVE_RUNS` | 8 | process admission |
| `APEIRON_LLM_TIMEOUT_MS` … `APEIRON_ASSEMBLE_TIMEOUT_MS` | per AP-E4 | step timeouts |
| `APEIRON_SSE_MAX_CONNECTIONS_PER_USER` | 5 | SSE cap |

Worker env: `APEIRON_SERVER_URL`, `APEIRON_WORKER_TOKEN`, `FFMPEG_PATH`
(default `ffmpeg`), `FFPROBE_PATH` (default `ffprobe`), `WORKER_CONCURRENCY`
(default 1), `WORKER_ID` (default `worker-<hostname>`).

## 12. Deployment

AP-Z1. Apeiron deploys as its own process group — plain processes, PM2
entries, or containers `apeiron-server` / `apeiron-worker` (the repository
ships a docker-based `apeiron/deploy.sh`) — on any host; the worker host
requires `ffmpeg` + `ffprobe` on PATH (or `FFMPEG_PATH`). The server binary
embeds `apeiron/web/dist` at release build, same convention as the platform
frontend.

AP-Z2. The platform dashboard nav exposes Apeiron through `/studio-entry`
(SB-6); Apeiron links back only through `APEIRON_PLATFORM_URL`.

## 13. Agent roadmap (researched direction, non-normative)

Grounded in the 2026 landscape: LTX Studio's linear stage workspace
(script → storyboard → generation → timeline), VideoAgent's material-aware
Storyboard Agent, MoneyPrinterTurbo's topic-to-film pipeline (implemented
here as the one-click flow), and ComfyUI-R1-style agents whose output IS
the workflow graph.

AP-AG1. **Stage-gated one-click.** `POST /runs/oneclick` gains
`stop_after: "storyboard" | null`. When set, the run terminal state is
`partial` after script+storyboard; the create page exposes per-shot editing
(description/keywords/duration, re-roll one shot) and a continue action
that starts the media+assembly stages from the edited shots. (LTX stage
model.)

AP-AG2. **Conversational graph agent.** `POST /api/projects/{id}/agent`
body `{"message": string}` runs an LLM tool loop (bounded 12 iterations)
with tools `update_script` `{prompt, system}`, `create_shots`
`{count}` (adds a storyboard node fed by the script), `update_shot`
`{index, description?, keywords?}` (writes `shot_overrides` on the
storyboard node), `set_param` `{node_id, key, value}`, `run_graph` `{}`.
Every mutation bumps the project version and broadcasts an SSE
`graph_patch` `{project_id, version}`. The response carries the final
assistant message and the list of applied tool calls. The canvas gets a
right-side chat panel that submits messages and reloads the graph on each
`graph_patch`. Agent LLM steps bill by token usage with the idempotency
key `apeiron_step_agent_<run uuid>`. (ComfyUI-R1 direction: natural
language compiles to the workflow.)

AP-AG3. **Material-aware storyboard.** Before finalizing shot keywords the
storyboard step queries the enabled material provider and records the top
candidate count per keyword in the shot payload; the create page surfaces a
candidate re-roll per shot when a keyword yields no candidates. (VideoAgent:
plan against the available asset bank.)

AP-AG4. **Timeline strip.** The create page renders a bottom timeline of
shot durations (from storyboard) + narration audio blocks; shots reorder by
drag and the order persists into assembly. (LTX timeline editor, minimal
form.)

AP-AG5. **BGM node.** `bgm` node kind: worker mixes a user-selected or
stock audio track under the narration at a configured gain in the assemble
pass.

Later: canvas multiplayer, signed third-party node packs, Postgres DSN,
publish + share pages.
