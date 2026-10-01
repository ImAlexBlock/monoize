# Announcements Specification

## 1. Scope

This specification defines the dashboard message-notification feature: the
announcement data model, the user-facing list/read endpoints, the admin CRUD
endpoints, the notification bell with unread badge and auto-popup panel in the
dashboard, and the admin management page at `/dashboard/announcements`.

An announcement is an admin-authored broadcast addressed to every dashboard
user. There is no per-user targeting.

## 2. Data model

AN-1. The system MUST persist announcements in table `announcements`:

- `id`: TEXT PRIMARY KEY (UUID);
- `title`: TEXT NOT NULL, 1 through 200 characters after trimming;
- `content`: TEXT NOT NULL, 1 through 5000 characters after trimming;
- `type`: TEXT NOT NULL DEFAULT `'info'` with CHECK constraint in
  (`'info'`, `'success'`, `'warning'`, `'error'`);
- `pinned`: INTEGER NOT NULL DEFAULT 0 (0 or 1);
- `enabled`: INTEGER NOT NULL DEFAULT 1 (0 or 1);
- `created_at`: TEXT NOT NULL (RFC 3339, UTC);
- `created_by`: TEXT NOT NULL (the admin user id at creation time; snapshot,
  not a foreign key).

AN-2. The system MUST persist per-user read state in table
`announcement_reads`:

- `user_id`: TEXT NOT NULL;
- `announcement_id`: TEXT NOT NULL;
- `read_at`: TEXT NOT NULL (RFC 3339, UTC);
- PRIMARY KEY (`user_id`, `announcement_id`).

Rows in `announcement_reads` whose announcement was deleted MUST be removed by
the delete transaction itself.

AN-3. Migration `m20260918_000108_announcements` MUST create both tables and
an index `idx_announcements_enabled_created` over
(`enabled`, `created_at` DESC) on SQLite and over (`enabled`, `created_at`) on
PostgreSQL.

## 3. User endpoints

AN-4. `GET /api/dashboard/announcements?limit=N` MUST require an
authenticated session. It MUST return, for the authenticated user:

- `announcements`: array of at most N (default 50, clamped 1..=200) rows with
  `enabled = 1`, ordered by `pinned` descending, then `created_at` descending,
  then `id` ascending in UTF-8 byte order. Each row: `id`, `title`, `content`,
  `type`, `pinned` (boolean), `created_at`, `is_read` (boolean; true when a
  `announcement_reads` row exists for this user and announcement).
- `unread_count`: integer, the count of `enabled = 1` announcements with no
  matching `announcement_reads` row for this user.

AN-5. `POST /api/dashboard/announcements/read` with JSON body
`{"ids": string[]}` MUST require an authenticated session. An empty `ids`
array marks every currently `enabled = 1` announcement as read for the user.
Each insert MUST be idempotent (re-marking an already-read announcement is a
no-op). The response MUST return the new `unread_count`. `ids` entries that do
not reference an existing announcement MUST be ignored silently.

## 4. Admin endpoints

AN-6. `GET /api/dashboard/announcements/admin` MUST require an admin session
(`require_admin`) and return every announcement including disabled ones,
ordered by `pinned` descending, then `created_at` descending, then `id`
ascending. Each row additionally carries `created_by` and `enabled` (boolean).

AN-7. `POST /api/dashboard/announcements` MUST require an admin session and a
JSON body `{title, content, type?, pinned?, enabled?}`. Validation: `title`
1..=200 characters after trimming, `content` 1..=5000 characters after
trimming, `type` one of the four AN-1 values (default `info`), `pinned` and
`enabled` booleans defaulting to false and true. Invalid input MUST return
HTTP 400 with code `invalid_request`. The response MUST return the created
announcement with HTTP 201.

AN-8. `PUT /api/dashboard/announcements/{id}` MUST require an admin session
and a JSON body with any subset of `{title, content, type, pinned, enabled}`
using the same validation as AN-7 for present fields. An unknown id MUST
return HTTP 404 with code `not_found`. The response MUST return the updated
announcement.

AN-9. `DELETE /api/dashboard/announcements/{id}` MUST require an admin
session, delete the announcement and all of its `announcement_reads` rows in
one transaction, and return HTTP 204. An unknown id MUST return HTTP 404 with
code `not_found`.

## 5. Frontend — notification bell and panel

AN-10. The dashboard sidebar MUST render a notification control (bell icon)
in the account area, visible to every authenticated role. While the
authenticated user has `unread_count > 0`, the control MUST show a badge with
the unread count, capped at "99+". The control MUST be reachable on both
expanded and collapsed sidebar states.

AN-11. Activating the bell MUST open a dialog listing the announcements of
AN-4 as a timeline: a colored dot per type (info = primary, success = green,
warning = amber, error = red), the title, the content as plain text (no
markdown rendering), and the publication time (relative wording for recent
items, absolute otherwise). Pinned announcements MUST render a pinned marker.

AN-12. Opening the dialog MUST mark every listed announcement as read for the
user (fire-and-forget call to AN-5 with the listed ids), and the badge MUST
update via SWR revalidation without requiring the dialog to reopen.

AN-13. On dashboard layout mount, when `unread_count > 0` and the current
Asia/Shanghai day is not snoozed, the dialog MUST open automatically. The
snooze state MUST be stored in localStorage under
`lynshen-announcement-snooze-until` holding the Beijing day id (`YYYY-MM-DD`)
until which auto-popup is suppressed. The dialog MUST offer a snooze action
that writes the current Beijing day id into that key.

AN-14. The bell data MUST be fetched with SWR using a 60-second refresh
interval while the dashboard layout is mounted.

## 6. Frontend — admin management page

AN-15. `/dashboard/announcements` MUST be reachable only through a nav item
rendered exclusively for admin sessions. Direct navigation by a non-admin
MUST show an unauthorized state without calling the admin endpoints.

AN-16. The page MUST render a table of all announcements (AN-6) with columns:
title, type badge, pinned marker, enabled state, publication time, actions
(edit, delete). Create and edit MUST use a dialog form with fields: title
(required), content (required, multiline), type select, pinned switch, enabled
switch. Delete MUST use a confirmation dialog. All mutations MUST update the
list optimistically and revalidate after the mutation settles.

## PostgreSQL Read Contract

The per-user read-state projection MUST return integer 0 or 1 on both supported
database backends. PostgreSQL BOOL values MUST NOT be decoded as Rust i32.
Read and unread filtering and counts MUST remain unchanged.
