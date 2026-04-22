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

`GET /status` reports `total` = number of enabled targets (1–3). `completed` increments after each phase finishes.

#### Result

`GET /result?jobId=xxx` returns a `ManageResult` where each phase is one entry:

```json
{
  "results": [
    {"id": "characters", "status": "success"},
    {"id": "weapons", "status": "success"},
    {"id": "artifacts", "status": "success"}
  ],
  "summary": {"total": 3, "success": 3, ...}
}
```

#### Fetching scan data

After the job completes, fetch results from the per-type data endpoints:

- `GET /characters?jobId=xxx` → `Vec<GoodCharacter>`
- `GET /weapons?jobId=xxx` → `Vec<GoodWeapon>`
- `GET /artifacts?jobId=xxx` → `Vec<GoodArtifact>`

Each cache stores only the latest jobId that produced data for that type. A characters-only scan updates only the character cache — weapon and artifact caches retain data from their most recent respective scans.

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

Lightweight poll — no result payload. Poll every 1 second.

#### When idle

```json
{"state": "idle"}
```

#### When running

```json
{
  "state": "running",
  "jobId": "abc-123",
  "progress": {"completed": 5, "total": 20}
}
```

#### When completed

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

| Code | When |
|------|------|
| 400 | Missing `jobId` query parameter |
| 404 | No character data, or `jobId` doesn't match the latest scan that produced characters |

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

| Code | When |
|------|------|
| 400 | Missing `jobId` query parameter |
| 404 | No weapon data, or `jobId` doesn't match the latest scan that produced weapons |

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

| Code | When |
|------|------|
| 404 | No artifact data available, or `jobId` doesn't match (when provided) |

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
   → "running": show progress (completed/total)
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
   → "running": show progress (completed/total phases)
   → "completed": proceed to step 4
4. GET /result?jobId=<id> → per-phase results (characters/weapons/artifacts)
5. GET /characters?jobId=<id> → character data (if scanned)
   GET /weapons?jobId=<id>    → weapon data (if scanned)
   GET /artifacts?jobId=<id>  → artifact data (if scanned)
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
