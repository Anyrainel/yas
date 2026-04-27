# HDR pixel calibration

GOODScanner currently assumes SDR-like captured RGB. HDR can remap brightness
and color channels, so every detector that compares raw RGB or brightness needs
validation from HDR dumps.

## HDR-sensitive magic values

| Area | File | Current magic values | How to validate |
| --- | --- | --- | --- |
| Artifact/weapon rarity stars | `genshin/src/scanner/common/pixel_utils.rs` | SDR star yellow: `R + G - B >= 252`; HDR star yellow: `B < 134`; rightmost-star cutoffs `1470`, `1430`, `1400` are geometry and should not change for HDR | Targeted probe reports final artifact-rarity errors and best linear classifier over `[-1, 0, 1]` |
| Artifact panel lock/astral | `genshin/src/scanner/common/pixel_utils.rs` | SDR: dark `< 128`, animation gates `116/208`; HDR: dark `< 210`, animation gates `188/233`; selected by `hdr_mode` | `pixel_eval --section panel` reports locked/unlocked and marked/unmarked brightness ranges |
| Weapon panel lock | `genshin/src/scanner/common/pixel_utils.rs` | Same SDR/HDR brightness profile as artifact panel icons | `pixel_eval --section panel` reports lock brightness using the SDR index-matched export as labels |
| Artifact panel elixir banner | `genshin/src/scanner/artifact/scanner.rs` | 2 of 3 probes at `(1440,430)`, `(1462,430)`, `(1490,432)`; SDR `R + G < 314`; HDR `R + G < 412` | Targeted banner search reports best linear `aR + bG + cB` classifier over coefficients in `[-1, 0, 1]` |
| Artifact/weapon grid icons | `genshin/src/scanner/common/grid_icon_detector.rs` | SDR artifact lock `R >= 185.1`, weapon lock `R + G >= 281.7`, astral `R + G >= 390.4`; HDR uses profile-specific in-icon sample offsets: artifact lock `R + G < 386.0`, astral `G - B >= 195.2`, grid elixir `G < 158.5`, weapon lock `G >= 224.2` | `pixel_eval --section grid` samples calibrated page positions and production rules |
| Inactive artifact substats | `genshin/src/scanner/common/pixel_utils.rs` | Profile-explicit values currently equal: bright text `> 200`; mid text `> 130`; inactive if `mid_pct > 20 && bright_pct < 78` | Use `pixel_eval --section substat`, after a clean HDR dump is captured with the elixir Y-shift fix |
| Character constellation pixels | `genshin/src/scanner/common/constants.rs` | SDR thresholds `[85.8, 86.9, 86.1, 88.1, 93.3, 97.5]`; HDR thresholds `[132.1, 138.5, 139.1, 144.5, 151.1, 152.8]`; selected by `hdr_mode` | `pixel_eval --section constellation` reports active/locked ring brightness by constellation node |
| Five-star artifact filter | `genshin/src/scanner/common/pixel_utils.rs` | active filter uses the same brightness `< 128` dark test at `ARTIFACT_FIVE_STAR_FILTER_POS` | Requires a filter-check dump or live probe; not enough labeled examples in normal per-item dumps |
| Return-to-main-world check | `genshin/src/scanner/common/game_controller.rs` | Paimon icon brightness `> 160` at 3 of 5 probes | Requires live navigation screenshots; not covered by item OCR dumps |
| Manager artifact-selection helpers | `genshin/src/manager/ui_actions.rs` | selection rarity star yellow `R > 150 && G > 100 && B < 100`; selection OCR binarization brightness `> 160` | Requires manager/equip dumps, not normal scan dumps |

Geometry constants such as icon coordinates, grid spacing, crop sizes, and star
X cutoffs are not HDR-specific unless HDR capture also changes scaling or the
captured window content is shifted.

## One-scan HDR workflow

1. Run one HDR scan with dumps enabled:

   ```powershell
   cargo run --release --bin GOODScanner -- --all --dump-images --output-dir target/release
   ```

2. Run the pixel-only evaluator against the HDR dump and the SDR/capture export
   that has the same scan order:

   ```powershell
   cargo run -p genshin_scanner --bin pixel_eval -- --hdr-mode --hdr-dump target/release/debug_images_hdr --label-export target/release/good_export_sdr.json --hdr-export target/release/good_export_hdr.json
   ```

3. For each detector, look at:

   - `fp` and `fn` under the current rule.
   - positive vs negative `min/p05/med/p95/max`.
   - the max-margin threshold: `(closest_positive_edge + closest_negative_edge) / 2`.
   - the room of error: half of the edge gap. Negative or zero means a shared
     threshold is not safe. The report should include best combined, best SDR,
     and best HDR values; if combined margin is negative, production should use
     the profile-specific SDR/HDR constants.

## Current calibrated margins

| Detector | Best combined | Combined room | Best SDR | SDR room | Best HDR | HDR room | Production choice |
| --- | --- | ---: | --- | ---: | --- | ---: | --- |
| Artifact rarity star best linear | `R - B >= 102.5` | 72.1 | `R + G - B >= 252.25` | 90.2 | `B < 134.25` | 120.3 | Profile-specific star yellow |
| Panel elixir best linear RGB | `R + G - B < 154.5` | 54.6 | `R + G < 314` | 106.8 | `R + G < 412.5` | 68.9 | Profile-specific elixir |
| Panel elixir `B - R` reference | `B - R > 25.5` | 25.1 | `B - R > 15.5` | 25.1 | `B - R > 30` | 21.2 | Not used; lower margin than best linear |
| Constellation C1-C3 brightness | C1 `110.8`, C2 `118.3`, C3 `118.9` | 10.0, 4.7, 3.8 | C1 `85.8`, C2 `86.9`, C3 `86.1` | 34.9, 36.0, 36.6 | C1 `132.1`, C2 `138.5`, C3 `139.1` | 31.3, 24.9, 24.0 | Profile-specific because C4-C6 fail combined |
| Constellation C4-C6 brightness | no safe combined threshold | negative | C4 `88.1`, C5 `93.3`, C6 `97.5` | 35.3, 32.7, 28.4 | C4 `144.5`, C5 `151.1`, C6 `152.8` | 19.6, 12.9, 7.8 | Profile-specific |
| Grid artifact lock | combined not used | n/a | `R >= 185.1`, 0 errors | 12.9 | sample offset `(+6,-9)`, `R + G < 386.0`, 0 errors | 14.7 | Profile-specific sample point and scalar |
| Grid artifact astral | combined not used | n/a | `R + G >= 390.4`, 0 errors | 44.3 | sample offset `(-1,-2)`, `G - B >= 195.2`, 0 errors | 37.8 | Profile-specific sample point and scalar |
| Grid weapon lock | combined not used | n/a | `R + G >= 281.7`, 0 errors | 16.7 | `G >= 224.2`, 0 errors | 4.9 | Weapon HDR grid Y alias is normalized before sampling |
| Grid elixir | not used for exported value | n/a | best SDR offset had only 1.2 margin, so SDR grid elixir remains diagnostic | n/a | sample offset `(-6,-7)`, `G < 158.5`, 0 errors | 12.1 | Panel elixir remains authoritative for `elixir_crafted` |

4. Patch the production constants, rebuild `pixel_eval`, and rerun the same command
   against the same HDR dump. No second game scan is needed unless a detector is
   not represented in the normal item dumps.

5. After the report shows clean separation, run a real HDR scan again to verify
   that early-stop decisions, lock/astral/elixir output, and OCR accuracy all
   hold together in the live scanner path.
