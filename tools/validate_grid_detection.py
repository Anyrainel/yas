#!/usr/bin/env python3
"""Validate artifact grid lock/astral detection against a GOOD ground truth.

This runs the same geometry as genshin/src/scanner/common/grid_icon_detector.rs
against dumped debug_images/artifacts/*/full.png files. It matches scanned GOOD
artifacts to the expected GOOD file by immutable artifact identity, then uses
scanner/debug order to validate every visible grid cell in every full.png.
"""

from __future__ import annotations

import argparse
import json
import os
from collections import Counter
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
from typing import Any

import cv2
import numpy as np

# Mirrors genshin/src/scanner/common/grid_icon_detector.rs for offline diagnostics.
# Keep these constants in sync when intentionally changing production grid geometry.
GRID_CX = 179.8
GRID_CY = 252.65
GRID_OX = 146.4
GRID_OY = 175.2
LOCK_DX = -48.65
LOCK_DY = -59.25
SLOT_SPACING = 22.65
CROP_HALF = 4.0
CARD_W = 123.5
CARD_H = 153.0
EDGE_W = 10.0
SEARCH_R = 30.0
COLS = 8
ROWS = 5
MAX_POSITIVE_ARTIFACT_GRID_OFF_Y = 8.0


def load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def artifact_identity(artifact: dict[str, Any]) -> str:
    def subs(name: str) -> list[tuple[str | None, float]]:
        return [
            (s.get("key"), round(float(s.get("value", 0.0)), 1))
            for s in artifact.get(name) or []
        ]

    return json.dumps(
        [
            artifact.get("setKey"),
            artifact.get("slotKey"),
            artifact.get("rarity"),
            artifact.get("level"),
            artifact.get("mainStatKey"),
            subs("substats"),
            subs("unactivatedSubstats"),
            artifact.get("elixirCrafted", artifact.get("elixerCrafted", False)),
        ],
        separators=(",", ":"),
    )


def build_expected_labels(actual: list[dict[str, Any]], expected: list[dict[str, Any]]) -> list[tuple[bool, bool]]:
    by_identity: dict[str, list[int]] = {}
    for idx, artifact in enumerate(expected):
        by_identity.setdefault(artifact_identity(artifact), []).append(idx)

    labels: list[tuple[bool, bool]] = []
    missing: list[int] = []
    for idx, artifact in enumerate(actual):
        matches = by_identity.get(artifact_identity(artifact))
        if not matches:
            missing.append(idx)
            labels.append((False, False))
            continue
        gt = expected[matches.pop(0)]
        labels.append((bool(gt.get("lock", False)), bool(gt.get("astralMark", False))))

    if missing:
        sample = ", ".join(str(i) for i in missing[:20])
        raise RuntimeError(f"{len(missing)} scanned artifacts did not match expected identity: {sample}")
    return labels


def load_rgb(path: Path) -> np.ndarray:
    bgr = cv2.imread(str(path), cv2.IMREAD_COLOR)
    if bgr is None:
        raise FileNotFoundError(path)
    return cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)


def rect_sum(integral: np.ndarray, x1: float, y1: float, x2: float, y2: float) -> float:
    h = integral.shape[0] - 1
    w = integral.shape[1] - 1
    ix1 = max(0, min(w, int(x1)))
    ix2 = max(0, min(w, int(x2)))
    iy1 = max(0, min(h, int(y1)))
    iy2 = max(0, min(h, int(y2)))
    if ix1 >= ix2 or iy1 >= iy2:
        return 0.0
    return float(integral[iy2, ix2] - integral[iy1, ix2] - integral[iy2, ix1] + integral[iy1, ix1])


def calibrate_grid(arr: np.ndarray) -> tuple[float, float]:
    h, w = arr.shape[:2]
    sx = w / 1920.0
    sy = h / 1080.0

    x0 = max(0, int((GRID_CX - CARD_W / 2.0 - 10.0) * sx))
    x1 = min(w, int((GRID_CX + (COLS - 1) * GRID_OX + CARD_W / 2.0 + 10.0) * sx))
    y0 = max(0, int((GRID_CY - CARD_H / 2.0 - SEARCH_R - 20.0) * sy))
    y1 = min(h, int((GRID_CY + (ROWS - 1) * GRID_OY + CARD_H / 2.0 + SEARCH_R + 40.0) * sy))

    crop = arr[y0:y1, x0:x1].astype(np.float32)
    light = (crop.max(axis=2) + crop.min(axis=2)) / (2.0 * 255.0)
    integral = cv2.integral(light, sdepth=cv2.CV_64F)

    grid_cx = GRID_CX * sx
    grid_cy = GRID_CY * sy
    grid_ox = GRID_OX * sx
    grid_oy = GRID_OY * sy
    card_w = CARD_W * sx
    card_h = CARD_H * sy
    edge_w = EDGE_W * sy
    gap_h = grid_oy - card_h

    best_score = float("-inf")
    best_dy = 0
    radius = int(max(sy, 1.0) * SEARCH_R)
    for dy in range(-radius, radius + 1):
        gy = grid_cy + dy
        score = 0.0
        for row in range(ROWS - 1):
            y_bot = gy + row * grid_oy + card_h / 2.0
            for col in range(COLS):
                cx = grid_cx + col * grid_ox
                xl = cx - card_w / 2.0 - x0
                xr = cx + card_w / 2.0 - x0
                yb = y_bot - y0
                score += rect_sum(integral, xl, yb - edge_w, xr, yb)
                score -= rect_sum(integral, xl, yb, xr, yb + gap_h)
        if score > best_score:
            best_score = score
            best_dy = dy

    raw_off_y = best_dy / sy
    fixed_off_y = raw_off_y - SLOT_SPACING if raw_off_y > MAX_POSITIVE_ARTIFACT_GRID_OFF_Y else raw_off_y
    return raw_off_y, fixed_off_y


def detect_cells(arr: np.ndarray, off_y: float) -> list[tuple[bool, bool]]:
    h, w = arr.shape[:2]
    sx = w / 1920.0
    sy = h / 1080.0
    out: list[tuple[bool, bool]] = []

    def mean_color(x: float, y: float) -> np.ndarray:
        hw = CROP_HALF * sx
        hh = CROP_HALF * sy
        x1 = max(0, min(w - 1, int(x - hw)))
        x2 = max(0, min(w, int(x + hw)))
        y1 = max(0, min(h - 1, int(y - hh)))
        y2 = max(0, min(h, int(y + hh)))
        crop = arr[y1:y2, x1:x2]
        if crop.size == 0:
            return np.zeros(3)
        return crop.reshape(-1, 3).mean(axis=0)

    for idx in range(COLS * ROWS):
        row = idx // COLS
        col = idx % COLS
        x = (GRID_CX + col * GRID_OX + LOCK_DX) * sx
        y = (GRID_CY + row * GRID_OY + off_y + LOCK_DY) * sy
        r, g, _b = mean_color(x, y)
        lock = bool(r > 180.0 and (r - g) > 50.0)
        _r2, g2, b2 = mean_color(x, y + SLOT_SPACING * sy)
        astral = bool(lock and (g2 - b2) > 100.0)
        out.append((lock, astral))
    return out


def worker(job: tuple[int, str, float, float]) -> tuple[int, float, float, dict[str, list[tuple[bool, bool]]]]:
    out_idx, full_path, raw_off_y, fixed_off_y = job
    arr = load_rgb(Path(full_path))
    return (
        out_idx,
        round(raw_off_y, 1),
        round(fixed_off_y, 1),
        {
            "raw": detect_cells(arr, raw_off_y),
            "production": detect_cells(arr, fixed_off_y),
            "zero": detect_cells(arr, 0.0),
        },
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--actual", required=True, type=Path)
    parser.add_argument("--expected", required=True, type=Path)
    parser.add_argument("--images", required=True, type=Path)
    parser.add_argument("--workers", type=int, default=max(1, min(10, (os.cpu_count() or 4) - 1)))
    args = parser.parse_args()

    actual = load_json(args.actual).get("artifacts") or []
    expected = load_json(args.expected).get("artifacts") or []
    index_map = load_json(args.images / "artifacts" / "index_map.json")
    labels = build_expected_labels(actual, expected)

    print(f"labels={len(labels)} index_map={len(index_map)}")
    print(
        "dump_diffs "
        f"lock={sum(bool(a.get('lock', False)) != l for a, (l, _s) in zip(actual, labels))} "
        f"astral={sum(bool(a.get('astralMark', False)) != s for a, (_l, s) in zip(actual, labels))}"
    )

    page_offsets: list[tuple[float, float]] = []
    for page_start in range(0, len(labels), COLS * ROWS):
        folder = index_map[page_start]
        full_path = args.images / "artifacts" / f"{folder:04d}" / "full.png"
        page_offsets.append(calibrate_grid(load_rgb(full_path)))
    print("page_offsets", Counter((round(r, 1), round(f, 1)) for r, f in page_offsets).most_common())

    jobs = []
    for out_idx, folder in enumerate(index_map):
        raw, fixed = page_offsets[out_idx // (COLS * ROWS)]
        full_path = args.images / "artifacts" / f"{folder:04d}" / "full.png"
        jobs.append((out_idx, str(full_path), raw, fixed))

    stats = {
        mode: {"lock": 0, "astral": 0, "observations": 0, "examples": []}
        for mode in ("raw", "production", "zero")
    }
    offset_counts: Counter[tuple[float, float]] = Counter()

    with ProcessPoolExecutor(max_workers=args.workers) as executor:
        for out_idx, raw, fixed, detections in executor.map(worker, jobs, chunksize=8):
            offset_counts[(raw, fixed)] += 1
            page_start = (out_idx // (COLS * ROWS)) * (COLS * ROWS)
            page_items = min(COLS * ROWS, len(labels) - page_start)
            for mode, cells in detections.items():
                st = stats[mode]
                for cell_idx, (lock, astral) in enumerate(cells[:page_items]):
                    exp_lock, exp_astral = labels[page_start + cell_idx]
                    st["observations"] += 1
                    if lock != exp_lock:
                        st["lock"] += 1
                    if astral != exp_astral:
                        st["astral"] += 1
                    if (lock != exp_lock or astral != exp_astral) and len(st["examples"]) < 10:
                        st["examples"].append(
                            {
                                "imageIndex": out_idx,
                                "cell": cell_idx,
                                "artifactIndex": page_start + cell_idx,
                                "expected": [exp_lock, exp_astral],
                                "detected": [lock, astral],
                                "offY": fixed if mode == "production" else raw if mode == "raw" else 0.0,
                            }
                        )

    print("image_offsets", offset_counts.most_common(20))
    for mode, st in stats.items():
        print(f"mode={mode} lock={st['lock']} astral={st['astral']} observations={st['observations']}")
        print("examples", json.dumps(st["examples"], ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
