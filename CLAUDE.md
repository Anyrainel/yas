# Yas — Genshin Impact Scanner

## Overview

Yas (Yet Another Scanner) is a Rust application that scans Genshin Impact in-game data (characters, weapons, artifacts) using OCR and exports it in **GOOD v3** (Genshin Open Object Description) format for use with optimizer tools.

## Architecture

### Workspace Crates

- **`yas`** (`yas_core`) — Platform-agnostic core library: screen capture, OCR (PaddlePaddle ONNX models), system control (mouse/keyboard), game window detection, positioning/scaling utilities.
- **`genshin`** (`yas_scanner_genshin`) — Genshin-specific scanner logic: GOOD v3 scanners for characters, weapons, and artifacts. Handles in-game navigation, panel OCR, and name matching via remote mappings.
- **`application`** — Binary crate. Single target: `GOODScanner.exe`.

### Key Modules (genshin)

```
src/
├── cli.rs                     # CLI entry point, orchestrates all scanning
├── scanner/
│   ├── common/                # Shared scanner infrastructure
│   │   ├── game_controller.rs # Mouse/keyboard/capture control
│   │   ├── backpack_scanner.rs# Grid-based inventory navigation
│   │   ├── mappings.rs        # Remote name→GOOD key mappings (from ggartifact.com)
│   │   ├── coord_scaler.rs    # Resolution-independent coordinate scaling (base: 1920x1080)
│   │   ├── models.rs          # GOOD v3 data models (GoodExport, GoodCharacter, etc.)
│   │   ├── stat_parser.rs     # Artifact stat string parsing
│   │   ├── diff.rs            # Groundtruth comparison tooling
│   │   ├── constants.rs       # Grid positions, UI coordinates
│   │   ├── ocr_factory.rs     # OCR backend selection (ppocrv3/v4/v5)
│   │   ├── pixel_utils.rs     # Color/pixel analysis helpers
│   │   ├── fuzzy_match.rs     # Fuzzy string matching for OCR results
│   │   └── navigation.rs      # Tab/page navigation helpers
│   ├── character/              # Character panel OCR
│   ├── weapon/                 # Weapon panel OCR
│   └── artifact/               # Artifact panel OCR
```

### How Scanning Works

1. User opens Genshin Impact and navigates to the appropriate screen
2. `GenshinGameController` captures the game window and provides scaled coordinates
3. `BackpackScanner` navigates the grid inventory (weapons/artifacts)
4. Individual scanners OCR each panel's fields (name, level, stats, etc.)
5. OCR results are fuzzy-matched against `MappingManager` data (fetched from ggartifact.com)
6. Results are exported as GOOD v3 JSON

### Config File (`good_config.json`)

On first run, a bilingual prompt asks for custom in-game names for Traveler/Wanderer/Manekin/Manekina (renameable characters). The JSON file is created next to the exe with these names plus all timing/delay defaults:

```json
{
  "traveler_name": "",
  "wanderer_name": "",
  "manekin_name": "",
  "manekina_name": "",
  "char_tab_delay": 400,
  "char_open_delay": 1200,
  "weapon_grid_delay": 60,
  "weapon_scroll_delay": 200,
  "weapon_tab_delay": 400,
  "weapon_open_delay": 1200,
  "artifact_grid_delay": 60,
  "artifact_scroll_delay": 200,
  "artifact_tab_delay": 400,
  "artifact_open_delay": 1200
}
```

Existing config files without delay fields are loaded correctly via `#[serde(default)]` and re-saved with new defaults.

## Build & Run

```bash
# Stable Rust toolchain
rustup default stable

# Build
cargo build --release

# The binary is at target/release/GOODScanner.exe
# Run with default (scan artifacts):
GOODScanner.exe

# Scan everything:
GOODScanner.exe --all

# Scan specific categories:
GOODScanner.exe --characters --weapons --artifacts
```

Requires administrator privileges on Windows (for input simulation).

## CLI Flags

All help text is bilingual (Chinese + English). Flags are grouped into four sections:

### Scan Targets
- `--characters` / `--weapons` / `--artifacts` / `--all`

### Global Options
- `-v, --verbose` — detailed scan info
- `--continue-on-failure` — keep scanning when individual items fail
- `--log-progress` — log each scanned item
- `--output-dir <DIR>` — output directory (default: `.`)
- `--ocr-backend <NAME>` — override OCR backend globally (ppocrv4 or ppocrv5)
- `--dump-images` — save OCR region screenshots to `debug_images/`

### Scanner Config
- `--weapon-min-rarity <N>` — min weapon rarity (default: 3)
- `--artifact-min-rarity <N>` — min artifact rarity (default: 4)
- `--char-max-count <N>` / `--weapon-max-count <N>` / `--artifact-max-count <N>` — max items (0 = unlimited)
- `--weapon-skip-delay` / `--artifact-skip-delay` — skip panel delay (faster but less reliable lock/astral detection)
- `--artifact-substat-ocr <NAME>` — substat/general OCR backend (default: ppocrv4)

### Debug
- `--debug-compare <PATH>` — groundtruth JSON comparison
- `--debug-actual <PATH>` — offline diff (no scanning)
- `--debug-start-at <N>` — skip to item index
- `--debug-char-index <N>` — jump to character index
- `--debug-timing` — per-field OCR timing
- `--debug-rescan-pos <R,C>` — re-scan a grid position
- `--debug-rescan-type <TYPE>` — scanner type for re-scan (default: weapon)
- `--debug-rescan-count <N>` — re-scan iterations (0 = infinite until RMB)

### Architecture Notes
- Character names are set via first-run prompt → `good_config.json` only (no CLI flags)
- Timing/delay settings live in `good_config.json` only (no CLI flags)
- Per-scanner verbose/dump/continue/log flags consolidated into global flags
- Per-scanner configs are plain structs (no clap derives); the orchestrator (`cli.rs`) populates them from global CLI flags + JSON config

## Dependencies & Platform

- **OCR**: ONNX Runtime (`ort` crate) with PaddleOCR models (embedded via `include_bytes!`)
- **Screen capture**: `screenshots` crate + `windows-capture` on Windows
- **Input simulation**: `enigo` crate
- **Remote mappings**: `reqwest` (blocking HTTP to ggartifact.com)
- **Windows only**: Requires admin, uses Win32 APIs for window detection

## Conventions

- All UI coordinates use 1920x1080 as base resolution, scaled at runtime via `CoordScaler`
- Chinese (zh_CN) game client only — OCR models trained on Chinese game text
- GOOD v3 format spec: keys use PascalCase (e.g., `"SkywardHarp"`, `"Furina"`)
- The `data/` directory (gitignored) caches remote mapping files

## Fuzzy Matching (`fuzzy_match.rs`)

5-tier fallback for matching OCR text against name→key maps:

1. **OCR confusion substitution** — char-by-char replacement of known misreads (e.g., 稚→薙, 拉→菈). Tries each pair individually, then applies ALL applicable substitutions simultaneously (needed when OCR garbles multiple chars, e.g. 菈乌玛→拉鸟玛 requires both 拉→菈 and 鸟→乌).
2. **Exact match** on cleaned/normalized text
3. **Substring match** (both directions: OCR added noise, or OCR truncated)
4. **Levenshtein distance** (30% threshold, char-level for CJK)
5. **LCS uniqueness fallback** (≥2 shared CJK chars, unique to one candidate)

### Adding OCR Confusion Pairs

In `OCR_CONFUSIONS` array. Rules:
- Only add `(wrong, correct)` where `wrong` does NOT appear as a standalone char in any legitimate name — otherwise exact match on that name would never be reached (the substitution would mangle it). Even if the substitution doesn't match, it wastes a lookup. Chars with collisions (菈↔莱, 鹮↔鹤/环) rely on Tier 4/5 instead.
- All current pairs are single-char to single-char. The combined pass assumes this.
- The combined pass applies all substitutions in one char-by-char sweep, avoiding cascading issues with bidirectional pairs (e.g., 茲↔兹).

## Artifact Scanner Details

### Dual-Engine OCR Pipeline

The artifact scanner uses two OCR backends (based on systematic eval — v4 dominates all fields except level):
- **Level engine** (ppocrv5, `--ocr-backend`): Only used for artifact level OCR ("+20" style text). v5 is 100% vs v4's 39.4% on level.
- **General engine** (ppocrv4, `--artifact-substat-ocr`): Used for everything else — name, main stat, set, equip, substats. v4 is strictly better on all these fields.

Level uses dual-engine (tries both, takes max valid). Substats use only the general engine (v4). Results are collected as `OcrCandidate` lists per line, then validated by the roll solver.

Weapon and character scanners use a single engine (v4 by default).

### Roll Solver (`roll_solver.rs`)

Validates substat combinations against game mechanics:
- Uses pre-computed **rollTable** lookup (from `rollTable.json` via `roll_table.rs`) — NOT brute-force f64 enumeration
- Each entry is `(display_value×10: i32, roll_count_bitmask: u8)`, binary searched
- Validates total roll count = init_count + level/4
- **Init preference**: Level 0 → prefer higher init first (lines = init count); Level > 0 → prefer lower init (better accuracy)
- Outputs `totalRolls`, `initialValue` per substat, and `inactive` flag
- The solver treats inactive (待激活) substats identically to active ones — their values are real roll values

### Elixir Crafted Detection

Elixir artifacts display a purple banner ("祝圣之霜定义") that shifts all content down by 40px (`ELIXIR_SHIFT`).
- Detection: 3 pixels at (1510, 1520, 1530), y=423 — checks for purple (blue > 230 && blue > green + 40)
- **Do NOT move to x=1683** — that hits the lock icon and causes massive false positives
- When detected, all subsequent OCR regions are Y-shifted by 40px

### Substat Crop Regions

- Lines 0–2: width 255px (calibrated to avoid OCR noise from wider crops)
- Line 3: width 355px (wider to capture "(待激活)" text on unactivated substats)
- All start at x=1356

### Unactivated Substats (待激活)

- Appear on level-0 artifacts as the 4th substat line with muted font and "(待激活)" appended
- The stat key and value are real (not zero) — it's the value that WILL be added on first level-up
- `stat_parser.rs` detects "(待激活)" text and sets `ParsedStat.inactive = true`, keeping the real value
- `OcrCandidate.inactive` propagates through the solver to `SolvedSubstat.inactive`
- Scanner splits solver results into `substats` (active) and `unactivated_substats` (inactive) in the output

### Pixel-Based Detection (highly reliable)

- **Rarity**: Star pixel color at fixed Y positions
- **Lock**: Pixel color at `ARTIFACT_LOCK_POS1` (1683, 428)
- **Elixir**: Purple banner check at (1510–1530, 423)
- **Astral mark**: Pixel at `ARTIFACT_ASTRAL_POS1`

### Parallelization

- `OcrPool`: Channel-based pool of N OCR model instances
- `scan_worker`: Generic parallel worker for backpack grid items
- **ALWAYS create separate pools** for main and substat OCR (sharing causes deadlock: N tasks each hold 1 instance, all waiting for a 2nd)

## Testing & Validation

### Groundtruth

- `genshin_export.json`: Exported via third-party tool, contains complete artifact/character/weapon data
- Note: GT uses typo `elixerCrafted` (not `elixirCrafted`) — diff report handles both

### Diff Report (`diff_report.py`)

- Compares scan output against groundtruth with Hungarian algorithm matching
- Groups by `(setKey, slotKey, rarity, lock)` — rarity and lock are hard matching requirements (pixel-based, very reliable)
- Three-tier categorization: non-stat diffs, stat-key diffs, stat-value-only diffs
- Always run scans with `--dump-images` so dump images match the scan output
- Use `python diff_report.py <scan.json> <gt.json>` to generate `diff_report.md`

### Other Scripts

- `test_solver.py`: Validates roll solver against groundtruth (expects ~99.7% totalRolls accuracy)
- `gen_roll_table.py`: Generates `roll_table.rs` from `rollTable.json`

### Key Calibration Values

| Parameter | Value | Notes |
|-----------|-------|-------|
| Substat width (lines 0–2) | 255px | Wider causes OCR failures |
| Substat width (line 3) | 355px | Captures "(待激活)" text |
| delay_after_panel | 100ms | Lock/astral mark animation |
| Talent overview width | 90px | Supports 2-digit levels |
| ELIXIR_SHIFT | 40px | Purple banner height |
| Elixir pixel positions | (1510–1530, 423) | Do NOT use x=1683 |
