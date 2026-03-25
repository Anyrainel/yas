# Artifact Manager HTTP API

Server: `http://127.0.0.1:{port}` (default 8765)

## Endpoints

### `GET /health`

```json
{"status":"ok","enabled":true,"busy":false}
```

- `enabled: false` — manager paused, `/manage` returns 503
- `busy: true` — a job is running, `/manage` returns 409

### `POST /manage` (async)

Submit a batch of instructions. Returns immediately — poll `GET /status` for progress.

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
| 409 | Another job running | `{"error": "..."}` |
| 503 | Manager paused | `{"error": "..."}` |
| 400 | Bad JSON or empty instructions | `{"error": "..."}` |

### `GET /status`

Poll job state. Returns immediately.

#### When idle (no job, or after completed result is consumed)

```json
{"state": "idle"}
```

#### When running

```json
{
  "state": "running",
  "jobId": "abc-123",
  "progress": {
    "completed": 5,
    "total": 20,
    "currentId": "instr-6",
    "phase": "Phase 1: Lock changes"
  }
}
```

#### When completed (persists until next `POST /manage`)

```json
{
  "state": "completed",
  "jobId": "abc-123",
  "result": {
    "results": [
      {"id": "instr-1", "status": "success", "detail": null},
      {"id": "instr-2", "status": "not_found", "detail": "..."}
    ],
    "summary": {
      "total": 2,
      "success": 1,
      "already_correct": 0,
      "not_found": 1,
      "errors": 0,
      "aborted": 0
    }
  }
}
```

## Status Values

| Status | Meaning |
|--------|---------|
| `success` | Applied |
| `already_correct` | Already in desired state |
| `not_found` | No matching artifact found |
| `invalid_input` | Bad data (empty keys, out-of-range values) |
| `ocr_error` | OCR identification failed |
| `ui_error` | Game UI interaction failed |
| `aborted` | User cancelled (right-click) |
| `skipped` | Skipped (earlier failure or abort) |

## Client Flow

```
1. GET /health → check enabled && !busy
2. POST /manage → get jobId (202)
3. Poll GET /status every 500ms–1s
   → "running": update progress UI
   → "completed": read result, done
4. If 409: another job is running, wait or show error
```

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
