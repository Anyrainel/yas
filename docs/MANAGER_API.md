# Artifact Manager HTTP API

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

Submit a batch of instructions. Returns immediately — poll `GET /status` for progress.

After accepting a job, the server waits 1 second before focusing the game window and starting execution. This lets the client see the state transition.

#### Request

```json
{
  "instructions": [
    {
      "id": "client-tracking-id",
      "target": {
        "setKey": "GladiatorsFinale",
        "slotKey": "flower",
        "rarity": 5,
        "level": 20,
        "mainStatKey": "hp",
        "substats": [
          {"key": "critRate_", "value": 3.9},
          {"key": "critDMG_", "value": 7.8}
        ]
      },
      "changes": {
        "lock": true,
        "location": "Furina"
      }
    }
  ]
}
```

#### Fields

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `id` | string | yes | Client-assigned ID, returned in results |
| `target.setKey` | string | yes | GOOD v3 PascalCase (e.g. `"GladiatorsFinale"`) |
| `target.slotKey` | string | yes | `flower` `plume` `sands` `goblet` `circlet` |
| `target.rarity` | int | yes | 1–5 |
| `target.level` | int | yes | 0–20 |
| `target.mainStatKey` | string | yes | GOOD v3 stat key (e.g. `"hp"`, `"atk_"`) |
| `target.substats` | array | yes | `[{key, value}]`, order-independent matching |
| `changes.lock` | bool? | no | Set lock state. Omit or `null` to skip. |
| `changes.location` | string? | no | GOOD character key to equip to. `""` = unequip. Omit or `null` to skip. |

At least one of `lock` or `location` must be present.

#### Responses

| Code | When | Body |
|------|------|------|
| 202 | Job accepted | `{"jobId": "<uuid>", "total": N}` |
| 400 | Bad JSON or empty instructions | `{"error": "..."}` |
| 403 | Disallowed origin | `{"error": "Origin not allowed"}` |
| 409 | Another job running | `{"error": "..."}` |
| 413 | Body too large (>5 MB) | `{"error": "..."}` |
| 503 | Manager paused | `{"error": "..."}` |

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

### `GET /result`

Full execution result — only available after job completes. Call once after `GET /status` returns `"completed"`.

#### 200 OK (completed)

```json
{
  "results": [
    {"id": "instr-1", "status": "success"},
    {"id": "instr-2", "status": "not_found", "detail": "背包中未找到匹配圣遗物 / ..."},
    {"id": "instr-3", "status": "already_correct", "detail": "锁定状态已正确 / ..."}
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

#### Other responses

| Code | When |
|------|------|
| 404 | No completed job (idle) |
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

## Client Flow

```
1. GET /health → check enabled && gameAlive && !busy
2. POST /manage → get jobId (202)
3. Poll GET /status every 1s
   → "running": show progress (completed/total)
   → "completed": proceed to step 4
   → no response: server crashed or game interrupted
4. GET /result → full per-instruction results
5. Done. Next POST /manage will reset state.
```

## Cancellation

Cancellation is local only — there is no cancel endpoint.
The user cancels by right-clicking in the game or stopping via the GOODScanner GUI.
The client just keeps polling; eventually `/status` will show `"completed"` with
aborted instructions reflected in the results.

## Examples

Lock + equip:

```json
{
  "instructions": [{
    "id": "1",
    "target": {
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
      ]
    },
    "changes": {"lock": true, "location": "RaidenShogun"}
  }]
}
```

Batch:

```json
{
  "instructions": [
    {"id": "a", "target": {...}, "changes": {"lock": true}},
    {"id": "b", "target": {...}, "changes": {"lock": false}},
    {"id": "c", "target": {...}, "changes": {"location": "Nahida"}}
  ]
}
```

All instructions execute sequentially. Invalid ones are filtered and reported individually; valid ones still run.
