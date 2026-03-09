# Yas — Genshin Impact Scanner

## Overview

Yas (Yet Another Scanner) is a Rust application that scans Genshin Impact in-game data (characters, weapons, artifacts) using OCR and exports it in **GOOD v3** (Genshin Open Object Description) format for use with optimizer tools.

## Architecture

### Workspace Crates

- **`yas`** (`yas_core`) — Platform-agnostic core library: screen capture, OCR (PaddlePaddle ONNX models), system control (mouse/keyboard), game window detection, positioning/scaling utilities.
- **`yas-genshin`** (`yas_scanner_genshin`) — Genshin-specific scanner logic: GOOD v3 scanners for characters, weapons, and artifacts. Handles in-game navigation, panel OCR, and name matching via remote mappings.
- **`yas-application`** — Binary crate. Single target: `yas.exe`.

### Key Modules (yas-genshin)

```
src/
├── application/
│   └── good_scanner.rs       # CLI entry point, orchestrates all scanning
├── scanner/
│   ├── good_common/           # Shared scanner infrastructure
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
│   ├── good_character_scanner/ # Character panel OCR
│   ├── good_weapon_scanner/    # Weapon panel OCR
│   └── good_artifact_scanner/  # Artifact panel OCR
```

### How Scanning Works

1. User opens Genshin Impact and navigates to the appropriate screen
2. `GenshinGameController` captures the game window and provides scaled coordinates
3. `BackpackScanner` navigates the grid inventory (weapons/artifacts)
4. Individual scanners OCR each panel's fields (name, level, stats, etc.)
5. OCR results are fuzzy-matched against `MappingManager` data (fetched from ggartifact.com)
6. Results are exported as GOOD v3 JSON

### Config File

On first run, `good_config.json` is created next to the exe. Users fill in custom in-game names for Traveler/Wanderer/Manekin/Manekina (renameable characters).

## Build & Run

```bash
# Stable Rust toolchain
rustup default stable

# Build
cargo build --release

# The binary is at target/release/yas.exe
# Run with default (scan artifacts):
yas.exe

# Scan everything:
yas.exe --good-scan-all

# Scan specific categories:
yas.exe --good-scan-characters --good-scan-weapons --good-scan-artifacts
```

Requires administrator privileges on Windows (for input simulation).

## CLI Flags

All flags are prefixed with `--good-*` for the main scanner config, plus per-scanner flags (see `--help`).

Key flags:
- `--good-scan-all` / `--good-scan-characters` / `--good-scan-weapons` / `--good-scan-artifacts`
- `--good-output-dir <DIR>` — output directory (default: `.`)
- `--good-traveler-name` / `--good-wanderer-name` — override config file names
- `--good-ocr-backend <ppocrv3|ppocrv4|ppocrv5>` — OCR model (default: ppocrv5)
- `--good-debug-compare <PATH>` — compare output against groundtruth JSON
- `--good-debug-timing` — show per-field OCR timing

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
