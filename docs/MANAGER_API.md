# GOODScanner HTTP API

Server: `http://127.0.0.1:{port}` (default 8765)

## Security

**Origin-based CORS**: The server only accepts requests from allowed origins.

| Origin | Allowed |
|--------|---------|
| `https://ggartifact.com` | Yes (production) |
| `http://localhost[:port]` | Yes (development) |
| `http://127.0.0.1[:port]` | Yes (development) |
| Any other origin | Rejected (403) |

Non-browser clients (curl, Postman) that don't send an `Origin` header are allowed — CORS is a browser-enforced mechanism.

The server binds to `127.0.0.1` only (not `0.0.0.0`), so it is not reachable from the network.

Request body size limit: 5 MB.

## Endpoints

### `GET /health`

```json
{"status":"ok","enabled":true,"busy":false,"gameAlive":true}
```

- `enabled: false` — manager paused, `/manage` returns 503
- `busy: true` — a job is running, `/manage` returns 409
- `gameAlive: false` — game window not found (Genshin not running)

### `POST /manage` (async)

Submit a batch of lock/unlock requests. Returns immediately — poll `GET /status` for progress.

After accepting a job, the server waits 1 second before focusing the game window and starting execution. This lets the client see the state transition.

#### Request

Two lists of artifacts in **GOOD v3 format**. Each artifact represents the client's view of its **current state**. Which list it appears in determines the desired action:

- `lock` — these artifacts should be **locked** after execution
- `unlock` — these artifacts should be **unlocked** after execution

The artifact's own `lock` field is ignored for determining intention — only list membership matters. This means stale data still expresses the correct intention (e.g., client thinks artifact is unlocked but it's already locked — if it's in the `lock` list, the server reports `already_correct` instead of toggling).

```json
{
  "lock": [
    {
      "setKey": "EmblemOfSeveredFate",
      "slotKey": "sands",
      "rarity": 5,
      "level": 20,
      "mainStatKey": "enerRech_",
      "substats": [
        {"key": "critRate_", "value": 10.5},
        {"key": "critDMG_", "value": 19.4},
        {"key": "atk_", "value": 5.8},
        {"key": "hp", "value": 508}
      ],
      "location": "RaidenShogun",
      "lock": false
    }
  ],
  "unlock": [
    {
      "setKey": "GladiatorsFinale",
      "slotKey": "flower",
      "rarity": 5,
      "level": 20,
      "mainStatKey": "hp",
      "substats": [
        {"key": "critRate_", "value": 3.9},
        {"key": "critDMG_", "value": 7.8}
      ],
      "location": "",
      "lock": true
    }
  ]
}
```

Each list item is a full (or partial) `GoodArtifact` object — the same format returned by `GET /artifacts` and the scanner export.

#### Matching

Artifacts are matched against the in-game backpack using these identity fields:

| Field | Used for | Notes |
|-------|----------|-------|
| `setKey` | Hard match | GOOD v3 PascalCase (e.g. `"GladiatorsFinale"`) |
| `slotKey` | Hard match | `flower` `plume` `sands` `goblet` `circlet` |
| `rarity` | Hard match | 4–5 (only 4★ and 5★ artifacts are supported) |
| `level` | Hard match | 0–20 |
| `mainStatKey` | Hard match | GOOD v3 stat key (e.g. `"hp"`, `"atk_"`) |
| `substats` | Hard match | `[{key, value}]`, order-independent. All keys must match exactly; each value must be within ±0.1 (OCR rounding tolerance). |
| `unactivatedSubstats` | Hard match | Same format and rules. Level-0 artifacts may have one unactivated substat. |

Other fields (`location`, `lock`, `astralMark`, `elixirCrafted`, `totalRolls`) are accepted but ignored during matching.

#### Result IDs

Since artifacts don't carry client-assigned IDs, results use positional IDs:
- `"lock:0"`, `"lock:1"`, ... for items in the `lock` list
- `"unlock:0"`, `"unlock:1"`, ... for items in the `unlock` list

#### Responses

| Code | When | Body |
|------|------|------|
| 202 | Job accepted | `{"jobId": "<uuid>", "total": N}` |
| 400 | Bad JSON, both lists empty, or any entry invalid (empty keys, rarity outside 4–5, level outside 0–20) | `{"error": "..."}` |
| 403 | Disallowed origin | `{"error": "Origin not allowed"}` |
| 409 | Another job running | `{"error": "..."}` |
| 413 | Body too large (>5 MB) | `{"error": "..."}` |
| 503 | Manager paused | `{"error": "..."}` |

### `POST /equip` (async)

Submit a batch of equip/unequip instructions. Returns immediately — poll `GET /status` for progress.

Unlike `POST /manage`, this endpoint does **not** perform a full backpack scan. It navigates directly to each target character's equipment screen to equip or unequip artifacts.

After accepting a job, the server waits 1 second before focusing the game window and starting execution (same as `POST /manage`).

#### Request

A flat list of equip instructions. Each instruction pairs an artifact (GOOD v3 format, representing the client's view of its **current state**) with a target `location` (GOOD v3 character key).

- To **equip** an artifact to a character: set `location` to the character key (e.g. `"RaidenShogun"`)
- To **unequip** an artifact from its current owner: set `location` to `""` (empty string)

```json
{
  "equip": [
    {
      "artifact": {
        "setKey": "EmblemOfSeveredFate",
        "slotKey": "sands",
        "rarity": 5,
        "level": 20,
        "mainStatKey": "enerRech_",
        "substats": [
          {"key": "critRate_", "value": 10.5},
          {"key": "critDMG_", "value": 19.4},
          {"key": "atk_", "value": 5.8},
          {"key": "hp", "value": 508}
        ],
        "location": "RaidenShogun",
        "lock": true
      },
      "location": "Furina"
    },
    {
      "artifact": {
        "setKey": "GladiatorsFinale",
        "slotKey": "flower",
        "rarity": 5,
        "level": 20,
        "mainStatKey": "hp",
        "substats": [
          {"key": "critRate_", "value": 3.9},
          {"key": "critDMG_", "value": 7.8}
        ],
        "location": "Furina",
        "lock": true
      },
      "location": ""
    }
  ]
}
```

Each item in the `equip` list has two fields:

| Field | Type | Description |
|-------|------|-------------|
| `artifact` | `GoodArtifact` | The artifact to equip/unequip, in GOOD v3 format (current state as the client knows it) |
| `location` | `string` | Target character key (e.g. `"Furina"`), or `""` to unequip |

The artifact's own `location` field describes where the client believes it currently is — this is informational and not used for matching. The top-level `location` field on each instruction is the **desired** destination.

#### Execution

Unequip instructions are processed first (all artifacts with `location: ""`), then equip instructions grouped by target character. This minimizes character screen transitions.

For each equip instruction, the server:

1. Opens the target character's equipment screen (press C, then cycle through the roster with OCR name matching)
2. Clicks the artifact slot matching the artifact's `slotKey`
3. Applies a **set filter** in the artifact selection grid to narrow results to the target `setKey`
4. Iterates through the filtered grid, matching by **level** (OCR of "+20" badge) then **first substat value** (OCR)
5. On match, clicks "替换" (Replace) to equip

The combination of set filter + slot tab + level + substat value is almost always unique. The grid is scrolled page-by-page (up to 20 pages) if needed.

For unequip, the server navigates to the artifact's current owner (from `artifact.location`), opens the slot, and clicks the unequip button.

#### Result IDs

Positional IDs based on order in the `equip` list:
- `"equip:0"`, `"equip:1"`, `"equip:2"`, ...

#### Game Swap Behavior

When equipping an artifact that is currently equipped on another character, the game **automatically swaps** — the target character receives the artifact, and the previous owner loses it (the slot becomes empty). The client can assume this swap occurred on success and update both characters' state accordingly.

#### Responses

| Code | When | Body |
|------|------|------|
| 202 | Job accepted | `{"jobId": "<uuid>", "total": N}` |
| 400 | Bad JSON, `equip` list empty, or any entry invalid (empty keys, rarity outside 4–5, level outside 0–20) | `{"error": "..."}` |
| 403 | Disallowed origin | `{"error": "Origin not allowed"}` |
| 409 | Another job running (manage or equip) | `{"error": "..."}` |
| 413 | Body too large (>5 MB) | `{"error": "..."}` |
| 503 | Manager paused | `{"error": "..."}` |

**Notes:**
- Equip jobs share the same job queue as manage/scan jobs — only one job of any type can run at a time. `GET /status` and `GET /result` work identically for all job types.
- Equip does **not** produce an artifact snapshot. `GET /artifacts` is not updated after an equip job.
- Invalid entries (empty `setKey`, rarity outside 4–5, level outside 0–20) reject the entire request with 400.
- Equip jobs invalidate the artifact cache — any previously cached artifact data is cleared.

### `POST /scan` (async)

Initiate a remote OCR scan of characters, weapons, and/or artifacts. Uses the same scanner pipeline as the CLI/GUI scanner. Returns immediately — poll `GET /status` for progress, then fetch results from the per-type data endpoints.

After accepting a job, the server waits 1 second before focusing the game window and starting execution (same as other job types).

#### Request

```json
{
  "characters": true,
  "weapons": true,
  "artifacts": true
}
```

At least one target must be `true`. All fields default to `false` if omitted.

The user must navigate to the appropriate in-game screen before submitting:
- **Characters**: open character screen (press C)
- **Weapons**: open backpack → weapon tab
- **Artifacts**: open backpack → artifact tab (if also scanning weapons, the scanner navigates from weapon tab automatically)

When scanning multiple targets, they execute in order: characters → weapons → artifacts.

#### Progress

Scan jobs use `scanProgress` on `GET /status` (not the linear `progress` field used by manage/equip). Each requested category gets its own slot; unrequested categories are omitted. See [`GET /status`](#get-status) below for the full shape.

#### Result

`GET /result?jobId=xxx` returns a `ManageResult` where each requested phase is one entry. `status` is `success` when the category finished scanning in full, `aborted` when it did not (user RMB, error, or the scan stopped before reaching it). Phases the client didn't request are omitted.

```json
{
  "results": [
    {"id": "characters", "status": "success"},
    {"id": "weapons",    "status": "aborted"},
    {"id": "artifacts",  "status": "aborted"}
  ],
  "summary": {"total": 3, "success": 1, "aborted": 2, ...}
}
```

#### Fetching scan data

After the job completes, fetch results from the per-type data endpoints:

- `GET /characters?jobId=xxx` → `Vec<GoodCharacter>`
- `GET /weapons?jobId=xxx` → `Vec<GoodWeapon>`
- `GET /artifacts?jobId=xxx` → `Vec<GoodArtifact>`

**All-or-nothing per category.** A category is only cached with `(jobId, data)` if it completes *in full* during the run. Categories that aborted mid-scan (or never started because an earlier phase aborted) are remembered as "incomplete for this jobId" — queries for that jobId return **503**, not stale data from a previous scan. This matches the manager's all-or-nothing philosophy.

Each cache stores only the latest completed jobId for its type. A characters-only scan updates only the character cache — weapon and artifact caches retain data from their most recent respective scans, queryable under their original jobIds.

#### Responses

| Code | When | Body |
|------|------|------|
| 202 | Job accepted | `{"jobId": "<uuid>", "targets": {"characters": true, "weapons": true, "artifacts": true}}` |
| 400 | Bad JSON, or no targets enabled | `{"error": "..."}` |
| 403 | Disallowed origin | `{"error": "Origin not allowed"}` |
| 409 | Another job running | `{"error": "..."}` |
| 503 | Manager paused | `{"error": "..."}` |

**Notes:**
- Scanning is read-only — it does not modify in-game state or invalidate existing caches.
- Scan jobs share the same job queue as manage/equip — one job at a time.
- If a scan fails (e.g., game window not found), previously cached data from earlier scans is preserved.

### `GET /status`

Lightweight poll — no result payload. Poll every 1 second. The shape varies by phase and by job type.

#### When idle

```json
{"state": "idle"}
```

#### When running — manage / equip

Linear progress: one `(completed, total)` pair, plus the id of the instruction currently being worked on and a human-readable phase label.

```json
{
  "state": "running",
  "jobId": "abc-123",
  "progress": {
    "completed": 47,
    "total": 120,
    "currentId": "lock:3",
    "phase": "锁定变更 / Lock changes"
  }
}
```

Notes:

- For `POST /manage`, `total` is the **backpack item count** (not the lock/unlock target count), and `completed` ticks per backpack item as the scan walks through it. The bar reflects actual scan progress through the inventory.
- For `POST /equip`, `total` is the equip instruction count. `completed` ticks as instructions resolve (per unequip target, per character visit during the roster scan).
- `currentId` is the result id of the instruction currently in flight (e.g. `"lock:3"`, `"equip:2"`) or `""` when no specific item is being worked on.
- `phase` is a bilingual label (e.g. `"锁定变更 / Lock changes"`, `"装备变更 / Equip changes"`), purely for human display.

#### When running — scan

Scan jobs do **not** use the linear `progress` field. They use `scanProgress`, which carries one slot per requested category — unrequested categories are omitted. This is what lets clients render independent progress bars for characters, weapons, and artifacts.

```json
{
  "state": "running",
  "jobId": "abc-123",
  "scanProgress": {
    "characters": {"completed": 12, "total": 12, "state": "running"},
    "weapons":    {"completed": 0,  "total": 0,  "state": "pending"},
    "artifacts":  {"completed": 0,  "total": 0,  "state": "pending"}
  }
}
```

Per-category slot fields:

| Field | Type | Meaning |
|-------|------|---------|
| `completed` | `usize` | Items scanned so far in this category. |
| `total` | `usize` | Target total for the bar. For weapons and artifacts, this is the backpack item count (known once the bag has been opened). **For characters, the game does not expose a total**, so `total` stays equal to `completed` and grows as characters are scanned — clients should render this as an indeterminate "N scanned" counter, not a percentage. |
| `state` | `"pending" \| "running" \| "complete" \| "aborted"` | Lifecycle. Only `pending` and `running` are seen on `/status`; `complete` / `aborted` appear in `GET /result` once the job finishes. |

Categories the client didn't request are omitted from the `scanProgress` object entirely (not present as `null`). Example — `POST /scan {"characters": true}`:

```json
{"state": "running", "jobId": "abc-123",
 "scanProgress": {"characters": {"completed": 7, "total": 7, "state": "running"}}}
```

#### When completed

Both `progress` and `scanProgress` are cleared. The `summary` is aggregated across all instructions.

```json
{
  "state": "completed",
  "jobId": "abc-123",
  "summary": {
    "total": 20,
    "success": 15,
    "already_correct": 3,
    "not_found": 1,
    "errors": 1,
    "aborted": 0
  }
}
```

Per-category final status for a scan is in `GET /result`, not `/status` — each category becomes a `{"id": "characters|weapons|artifacts", "status": "success|aborted"}` entry.

### `GET /result?jobId=<id>`

Full execution result. Requires the `jobId` returned by `POST /manage`, `POST /equip`, or `POST /scan`. Idempotent — can be called multiple times. Result is available until the next job replaces it.

#### 200 OK (completed)

```json
{
  "results": [
    {"id": "lock:0", "status": "success"},
    {"id": "lock:1", "status": "not_found"},
    {"id": "unlock:0", "status": "already_correct"}
  ],
  "summary": {
    "total": 3,
    "success": 1,
    "already_correct": 1,
    "not_found": 1,
    "errors": 0,
    "aborted": 0
  }
}
```

Each result contains only `id` and `status`. No human-readable detail — i18n is the client's responsibility.

#### Other responses

| Code | When |
|------|------|
| 400 | Missing `jobId` query parameter |
| 404 | Job not found (wrong jobId, or replaced by a newer job) |
| 409 | Job still running |

## Status Values

| Status | Meaning |
|--------|---------|
| `success` | Applied |
| `already_correct` | Already in desired state |
| `not_found` | No matching artifact found |
| `invalid_input` | Bad data (empty keys, out-of-range values) |
| `ocr_error` | OCR identification failed |
| `ui_error` | Game UI interaction failed |
| `aborted` | User cancelled (right-click in game or GUI) |
| `skipped` | Skipped (earlier failure or abort) |

### `GET /characters?jobId=<id>`

Character scan data from the latest scan that produced character results.

#### 200 OK

```json
[
  {
    "key": "Furina",
    "level": 90,
    "constellation": 1,
    "ascension": 6,
    "talent": {"auto": 1, "skill": 9, "burst": 10}
  }
]
```

#### Other responses

| Code | When | Body |
|------|------|------|
| 400 | Missing `jobId` query parameter | `{"error": "missing required query parameter: jobId"}` |
| 404 | Unknown `jobId` — never seen, or overwritten by a later scan | `{"error": "no characters data for this jobId"}` |
| 503 | The supplied `jobId` attempted to scan characters but did not finish (user aborted, error, or the job stopped before reaching this category) | `{"error": "characters scan incomplete for this jobId"}` |

### `GET /weapons?jobId=<id>`

Weapon scan data from the latest scan that produced weapon results.

#### 200 OK

```json
[
  {
    "key": "SkywardHarp",
    "level": 90,
    "ascension": 6,
    "refinement": 1,
    "location": "Furina",
    "lock": true
  }
]
```

#### Other responses

| Code | When | Body |
|------|------|------|
| 400 | Missing `jobId` query parameter | `{"error": "missing required query parameter: jobId"}` |
| 404 | Unknown `jobId` | `{"error": "no weapons data for this jobId"}` |
| 503 | The supplied `jobId` attempted to scan weapons but did not finish | `{"error": "weapons scan incomplete for this jobId"}` |

### `GET /artifacts[?jobId=<id>]`

Artifact data. Works for both scan results and manage snapshots — whichever last updated the artifact cache.

The `jobId` parameter is **optional** for backwards compatibility:
- **With `jobId`**: returns data only if the jobId matches the latest artifact data. Returns 404 on mismatch.
- **Without `jobId`**: returns the latest artifact data regardless of which job produced it.

#### 200 OK

```json
[
  {
    "setKey": "GladiatorsFinale",
    "slotKey": "flower",
    "level": 20,
    "rarity": 5,
    "mainStatKey": "hp",
    "substats": [
      {"key": "critRate_", "value": 3.9, "initialValue": 3.9},
      {"key": "critDMG_", "value": 7.8}
    ],
    "unactivatedSubstats": [],
    "location": "",
    "lock": true,
    "astralMark": false,
    "elixirCrafted": false,
    "totalRolls": 8
  }
]
```

#### Other responses

| Code | When | Body |
|------|------|------|
| 404 | No artifact data cached, or `jobId` was provided but doesn't match | `{"error": "no artifacts data for this jobId"}` / `{"error": "没有可用的圣遗物数据 / No artifact data available"}` |
| 503 | The supplied `jobId` attempted to populate the artifact cache (via `POST /scan` with `artifacts: true`) but did not finish | `{"error": "artifacts scan incomplete for this jobId"}` |

**Notes on artifact cache sources:**
- **Scan jobs** (`POST /scan` with `artifacts: true`): populates the artifact cache with the full scan results.
- **Manage jobs** (`POST /manage`): populates the artifact cache with a post-toggle snapshot when the backpack scan completes fully (no abort, no early stop, no OCR/solver failures).
- **Equip jobs** (`POST /equip`): invalidate the artifact cache (in-game equipment state changed) but do not produce new data.
- Each source writes its own `jobId` — use the `jobId` from the job that produced the data.
- Lock states in manage snapshots reflect post-toggle state for successfully changed artifacts.
- All cached data persists in memory for the server's lifetime. It is not written to disk.

## Client Flow

### Manage / Equip

```
1. GET /health → check enabled && gameAlive && !busy
2. POST /manage (or POST /equip) → get jobId (202)
3. Poll GET /status every 1s
   → "running": read `progress.{completed, total, currentId, phase}` and render a single bar
   → "completed": proceed to step 4
   → no response: server crashed or game interrupted
4. GET /result?jobId=<id> → full per-instruction results (idempotent)
5. GET /artifacts?jobId=<id> → updated artifact inventory (optional, manage only)
6. Done. Next job will replace the stored result.
```

### Scan

```
1. GET /health → check enabled && gameAlive && !busy
2. POST /scan → get jobId (202)
3. Poll GET /status every 1s
   → "running": read `scanProgress.{characters,weapons,artifacts}`, render one bar per
                requested category. Characters has no upfront total — render as an
                indeterminate "N scanned" counter.
   → "completed": proceed to step 4
4. GET /result?jobId=<id> → per-phase results. `status: "success"` → category finished;
                             `status: "aborted"` → category did not finish.
5. For each phase where status == "success":
     GET /characters?jobId=<id> → character data
     GET /weapons?jobId=<id>    → weapon data
     GET /artifacts?jobId=<id>  → artifact data
   For aborted phases, these endpoints return 503 with the same jobId — do not treat
   that as an error to retry, the data is genuinely unavailable for this run.
6. Done. Each data cache retains its jobId independently.
```

## Cancellation

Cancellation is local only — there is no cancel endpoint.
The user cancels by right-clicking in the game or stopping via the GOODScanner GUI.
The client just keeps polling; eventually `/status` will show `"completed"` with
aborted instructions reflected in the results.

## Examples

Lock a single artifact:

```json
{
  "lock": [{
    "setKey": "EmblemOfSeveredFate",
    "slotKey": "sands",
    "rarity": 5,
    "level": 20,
    "mainStatKey": "enerRech_",
    "substats": [
      {"key": "critRate_", "value": 10.5},
      {"key": "critDMG_", "value": 19.4},
      {"key": "atk_", "value": 5.8},
      {"key": "hp", "value": 508}
    ],
    "location": "RaidenShogun",
    "lock": false
  }]
}
```

Batch lock + unlock:

```json
{
  "lock": [
    {"setKey": "EmblemOfSeveredFate", "slotKey": "sands", "rarity": 5, "level": 20, "mainStatKey": "enerRech_", "substats": [...], "location": "", "lock": false},
    {"setKey": "GladiatorsFinale", "slotKey": "flower", "rarity": 5, "level": 20, "mainStatKey": "hp", "substats": [...], "location": "", "lock": false}
  ],
  "unlock": [
    {"setKey": "WanderersTroupe", "slotKey": "circlet", "rarity": 5, "level": 16, "mainStatKey": "critRate_", "substats": [...], "location": "Furina", "lock": true}
  ]
}
```

Level-0 artifact with unactivated substat:

```json
{
  "lock": [{
    "setKey": "GladiatorsFinale",
    "slotKey": "flower",
    "rarity": 5,
    "level": 0,
    "mainStatKey": "hp",
    "substats": [
      {"key": "critRate_", "value": 3.9},
      {"key": "critDMG_", "value": 7.8},
      {"key": "atk_", "value": 5.8}
    ],
    "unactivatedSubstats": [
      {"key": "def", "value": 23.0}
    ],
    "location": "",
    "lock": false
  }]
}
```

All targets execute in a single backpack scan pass. Invalid entries (empty keys, rarity outside 4–5, level outside 0–20) reject the entire request with 400 — fix all entries before resubmitting.

## Changelog

### 2026-04-23

- **BREAKING: scan progress is per-category, not per-job.** `POST /scan` jobs now report progress via a new `scanProgress` object on `GET /status` instead of the linear `progress` field. Each requested category has its own `{completed, total, state}` slot (state is `pending` / `running` / `complete` / `aborted`). Unrequested categories are omitted. Clients that were reading `/status.progress.completed` during a scan must switch to `/status.scanProgress.{characters|weapons|artifacts}.completed`.
- **Manage/equip progress gains `currentId` and `phase`.** The existing `progress` field on `/status` now serializes all four fields (`completed`, `total`, `currentId`, `phase`) — previously `currentId` and `phase` were stored server-side but stripped from the wire. Clients that already ignored the absent fields are unaffected.
- **Manage progress is now real-time.** `LockManager::execute` emits a progress tick per backpack item as it walks the inventory. Previously all ticks fired in a burst at the end of the scan (the bar sat at 0 for the whole scan then jumped to 100%). `total` on manage is the backpack item count.
- **Equip gets intermediate progress.** `EquipManager::execute` now ticks per unequip target and per character visit during the roster scan. Previously only start/end ticks were emitted.
- **All-or-nothing per scan category.** `GET /characters` / `GET /weapons` / `GET /artifacts` now return **503** when the supplied `jobId` attempted to populate that category but didn't finish (user aborted, error, or the scan stopped before reaching it). Previously such cases fell through to 404. 404 is now reserved for "unknown jobId". A partially-aborted scan no longer leaves stale data from a prior run accessible under the new jobId.
- **`GET /result` for scan jobs uses `success` / `aborted` per phase.** Entries for categories that finished in full report `status: "success"`; categories that aborted or didn't run report `status: "aborted"`. Unrequested phases are omitted from `results`.
- **Init failure no longer poisons the server.** Previously, if the executor's first-job init (game window detection, OCR model load) failed, every subsequent job would panic because the init closure had been consumed. Now the init is retried on each subsequent job until it succeeds — useful when the user hasn't opened the game yet.
- **`focus_game_window` no longer mouse-moves on failure (Windows).** If the game window isn't found, the scanner logs an error and returns without moving the mouse. Previously it would move the mouse to where the game window *would have been*, causing clicks to land in whatever unrelated window was actually focused. Non-Windows platforms still do the mouse hint.

### 2026-04-22

- **`POST /scan` implemented** — New endpoint for initiating OCR scans remotely. Supports scanning characters, weapons, and/or artifacts in any combination. Uses the same async job model as manage/equip (202 → poll → result). Scan data is fetched from per-type endpoints.
- **`GET /characters?jobId=xxx`** — New endpoint for fetching character scan results.
- **`GET /weapons?jobId=xxx`** — New endpoint for fetching weapon scan results.
- **`GET /artifacts` now accepts optional `jobId`** — `GET /artifacts?jobId=<id>` validates the jobId matches; `GET /artifacts` (no jobId) returns the latest data for backwards compatibility. `/characters` and `/weapons` require `jobId`.
- **Data caching redesign** — Replaced the single `ArtifactCache` enum (Empty/Complete/Incomplete) with per-type `ScanDataCache<T>` structs. Each stores the `jobId` that produced it. Scan jobs update only the caches they scanned. Manage jobs update only the artifact cache. Previous data for unscanned types is preserved across partial scans.

### 2026-03-31

- **`POST /equip` implemented** — Previously documented as "not yet implemented" (501). Now fully functional. Navigates to character equipment screens, applies set filters in the artifact selection grid, and matches artifacts by level + substat value OCR. Unequip instructions execute first, then equip instructions grouped by target character. Equip jobs invalidate the artifact cache.

### 2026-03-30 (v2)

- **BREAKING: Validation rejects entire request** — Any invalid entry (empty keys, rarity outside 4–5, level outside 0–20) now returns 400 for the whole request. Previously, invalid entries were filtered and reported individually while valid entries still ran.
- **BREAKING: `GET /result` requires `jobId`** — `GET /result?jobId=<id>`. Returns 400 without it. Returns 404 if the jobId doesn't match. This prevents accidentally reading a stale job's result.
- **`GET /result` is idempotent** — Can be called multiple times. Result persists until the next job replaces it.
- **Removed `detail` from results** — `InstructionResult` no longer includes a human-readable `detail` field. The `status` enum uniquely identifies each scenario; i18n is the client's responsibility.
- **Substats are hard match** — `substats` and `unactivatedSubstats` are now hard-match fields (previously scoring). All keys must match exactly; each value within ±0.1 tolerance.

### 2026-03-30

- **`POST /equip` documented** — New endpoint for equipping/unequipping artifacts to characters. Uses a flat `equip` list of `{artifact, location}` instructions. Same async job model as `POST /manage`. Shares the job queue (one job at a time across both endpoints). Does not produce an artifact snapshot.
- **BREAKING: `POST /manage` redesigned** — Replaced instruction-based format (`instructions` array with `id`/`target`/`changes`) with GOOD-format lock/unlock lists (`lock` and `unlock` arrays of `GoodArtifact`). Lock intention is determined by list membership, not by a `changes.lock` field. Result IDs are positional (`lock:0`, `unlock:1`, etc.).
- **Equip removed** — The `changes.location` field and equip/unequip functionality have been removed from this endpoint. Equip will be a separate API in the future.
- **Unactivated substats** — `unactivatedSubstats` is now included in artifact matching (scoring), and the `GET /artifacts` response includes it for level-0 artifacts.
- **Rarity restriction** — Only 4★ and 5★ artifacts are accepted (rarity must be 4 or 5). The backpack scan stops early when it encounters artifacts below this threshold.
- **Rarity early-stop in scanner** — Both the artifact scanner and lock manager now stop scanning when artifacts drop below `min_rarity`, using a shared helper. The scanner previously used hardcoded thresholds (`≤3` for artifacts, `≤2` for weapons); both now use the configured `min_rarity`.

### 2026-03-29

- **`GET /artifacts`**: New endpoint — returns the latest complete artifact inventory as a flat JSON array of GOOD v3 artifacts. Updated after each manage job that completes a full backpack scan without interruption. Lock states reflect post-toggle values. Returns 404 if no scan has been performed yet.
- **Lock manager**: OCR is now pipelined (async) — captured images are dispatched to rayon workers immediately, running in parallel with subsequent grid captures. Results are collected at page boundaries before applying lock toggles.
