<div align="center">

# GOODScanner

**[中文](README.md) | English**

Genshin Impact GOOD Format Scanner, based on [yas](https://github.com/wormtql/yas)

Scans in-game character, weapon, and artifact data and exports it as [GOOD v3](https://frzyc.github.io/genshin-optimizer/#/doc) JSON for use with optimizer tools like [GGArtifact](https://ggartifact.com/), [Genshin Optimizer](https://frzyc.github.io/genshin-optimizer/).

[![Build](https://github.com/Anyrainel/GOODScanner/actions/workflows/rust.yml/badge.svg)](https://github.com/Anyrainel/GOODScanner/actions)

</div>

## Features

- **Character scanning**: level, ascension, constellation, talents
- **Weapon scanning**: name, level, ascension, refinement, equipped character, lock status
- **Artifact scanning**: set, slot, main stat, substats, level, rarity, lock, astral mark, elixir crafted flag, unactivated substats
- **Dual-engine OCR**: PPOCRv4 (general) + PPOCRv5 (level-specific), automatically picks the best result
- **Substat validation**: Roll Solver verifies substat combinations against game mechanics

## Quick Start

### Download

Get the latest `GOODScanner.exe` from the [Releases](https://github.com/Anyrainel/GOODScanner/releases) page.

### Usage

1. Run `GOODScanner.exe` **as administrator**
2. On first run, you'll be prompted for custom character names (Traveler, Wanderer, etc.). Config is saved to `data/good_config.json`
3. Make sure Genshin Impact is running, then press Enter to start (the program will automatically focus the game window and open the correct screens)
4. **Right-click to abort** during scanning
5. Results are saved as `GOODv3.json` in the current directory

### Scan Targets

By default, all categories are scanned. You can also pick specific ones:

```shell
GOODScanner.exe                    # Scan all
GOODScanner.exe --characters       # Characters only
GOODScanner.exe --weapons          # Weapons only
GOODScanner.exe --artifacts        # Artifacts only
GOODScanner.exe --characters --weapons  # Combine targets
```

## Requirements

- **Administrator privileges** (required for input simulation)
- **Simplified Chinese** game client only
- **16:9 resolution** recommended (1920x1080, 2560x1440, etc.)
- Do not move the mouse during scanning
- Artifacts below 4-star are skipped by default (adjustable via `--artifact-min-rarity`)

## CLI Options

### Global Options

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Show detailed scan info |
| `--continue-on-failure` | Keep scanning when individual items fail |
| `--log-progress` | Log each item as it is scanned |
| `--output-dir <DIR>` | Output directory (default: `.`) |
| `--ocr-backend <NAME>` | Override OCR backend (ppocrv4 or ppocrv5) |
| `--dump-images` | Save OCR region screenshots to `debug_images/` |

### Scanner Config

| Flag | Description |
|------|-------------|
| `--weapon-min-rarity <N>` | Minimum weapon rarity (default: 3) |
| `--artifact-min-rarity <N>` | Minimum artifact rarity (default: 4) |
| `--char-max-count <N>` | Max characters to scan (0 = unlimited) |
| `--weapon-max-count <N>` | Max weapons to scan (0 = unlimited) |
| `--artifact-max-count <N>` | Max artifacts to scan (0 = unlimited) |
| `--weapon-skip-delay` | Skip weapon panel delay (faster but lock detection may be inaccurate) |
| `--artifact-skip-delay` | Skip artifact panel delay (faster but lock/astral detection may be inaccurate) |
| `--artifact-substat-ocr <NAME>` | Substat OCR backend (default: ppocrv4) |

### Config File

Timing parameters and character names are configured via `data/good_config.json` (no CLI flags needed):

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
  "artifact_grid_delay": 60,
  "artifact_scroll_delay": 200
}
```

## Building from Source

```shell
# Requires stable Rust toolchain
rustup default stable

# Make sure Git LFS is installed
git lfs pull

# Build
cargo build --release

# Binary is at target/release/GOODScanner.exe
```

## Acknowledgments

- [wormtql/yas](https://github.com/wormtql/yas) — Original project providing the core OCR scanning framework
- [1803233552/yas](https://github.com/1803233552/yas) — Fork that this project is based on
- [Andrewthe13th/Inventory_Kamera](https://github.com/Andrewthe13th/Inventory_Kamera) — Reference implementation for GOOD format scanning

## Feedback

- [GitHub Issues](https://github.com/Anyrainel/GOODScanner/issues)
