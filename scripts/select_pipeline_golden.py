#!/usr/bin/env python3
"""Select a deterministic, coverage-oriented pipeline golden corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path


IMPORTANT_SPECIAL_ISSUES = {
    "table_fewer_line",
    "table_horizontal",
    "table_span",
    "table_with_formula",
    "table_full_line",
    "table_with_img",
    "table_omission_line",
    "handwriting",
    "fuzzy_content",
    "fuzzy_scan",
    "text_L-shaped_wrap",
    "text_O-shaped_wrap",
    "geometric_deformation",
}

IMPORTANT_CATEGORIES = {
    "table",
    "figure",
    "equation_isolated",
    "equation_semantic",
    "organic_chemical_formula_mask",
    "code_txt",
    "list_group",
    "reference",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def item_tags(item: dict) -> set[str]:
    page_info = item.get("page_info") or {}
    attributes = page_info.get("page_attribute") or {}
    tags = set()
    for key in ("data_source", "language", "layout", "subset"):
        value = attributes.get(key)
        if value is not None:
            tags.add(f"{key}:{value}")
    for issue in attributes.get("special_issue") or []:
        if issue in IMPORTANT_SPECIAL_ISSUES:
            tags.add(f"special_issue:{issue}")
    categories = {det.get("category_type") for det in item.get("layout_dets") or []}
    for category in categories & IMPORTANT_CATEGORIES:
        tags.add(f"category:{category}")
    if "table" in categories and any(
        category in categories
        for category in ("equation_isolated", "equation_semantic", "organic_chemical_formula_mask")
    ):
        tags.add("interaction:table_and_formula")
    if "table" in categories and "figure" in categories:
        tags.add("interaction:table_and_figure")
    return tags


def stable_id(item: dict) -> tuple[int, str]:
    page_info = item.get("page_info") or {}
    return int(page_info.get("sample_id", 2**31 - 1)), str(page_info.get("image_path", ""))


def select_items(items: list[dict], count: int) -> list[dict]:
    if count <= 0:
        return []
    candidates = [(item, item_tags(item)) for item in items]
    frequencies = Counter(tag for _, tags in candidates for tag in tags)
    uncovered = set(frequencies)
    selected: list[dict] = []
    selected_ids = set()

    while uncovered and len(selected) < count:
        ranked = []
        for item, tags in candidates:
            item_id = stable_id(item)
            if item_id in selected_ids:
                continue
            new_tags = tags & uncovered
            score = sum(1.0 / frequencies[tag] for tag in new_tags)
            ranked.append((-score, -len(new_tags), item_id, item, tags))
        if not ranked:
            break
        ranked.sort(key=lambda entry: (entry[0], entry[1], entry[2]))
        neg_score, _, item_id, item, tags = ranked[0]
        if neg_score == 0:
            break
        selected.append(item)
        selected_ids.add(item_id)
        uncovered -= tags

    remaining = [item for item in sorted(items, key=stable_id) if stable_id(item) not in selected_ids]
    needed = count - len(selected)
    if needed > 0 and remaining:
        stride = max(1, len(remaining) // needed)
        cursor = 0
        while len(selected) < count and cursor < len(remaining):
            item = remaining[cursor]
            selected.append(item)
            selected_ids.add(stable_id(item))
            cursor += stride
        for item in remaining:
            if len(selected) >= count:
                break
            if stable_id(item) not in selected_ids:
                selected.append(item)
                selected_ids.add(stable_id(item))
    return selected


def build_output(source: Path, images_dir: Path, count: int) -> dict:
    data = json.loads(source.read_text(encoding="utf-8"))
    if not isinstance(data, list):
        raise ValueError("OmniDocBench source must contain a JSON array")
    selected = select_items(data, count)
    all_target_tags = sorted({tag for item in data for tag in item_tags(item)})
    selected_tags = sorted({tag for item in selected for tag in item_tags(item)})
    entries = []
    missing_images = []
    for item in selected:
        page_info = item["page_info"]
        image_path = images_dir / page_info["image_path"]
        if not image_path.is_file():
            missing_images.append(str(image_path))
        entries.append(
            {
                "sample_id": page_info.get("sample_id"),
                "page_no": page_info.get("page_no"),
                "image_path": str(image_path.resolve()),
                "tags": sorted(item_tags(item)),
            }
        )
    return {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source": str(source.resolve()),
        "source_sha256": sha256_file(source),
        "requested_count": count,
        "selected_count": len(entries),
        "coverage": {
            "target_tag_count": len(all_target_tags),
            "covered_tag_count": len(selected_tags),
            "missing_tags": sorted(set(all_target_tags) - set(selected_tags)),
            "covered_tags": selected_tags,
        },
        "missing_images": missing_images,
        "samples": entries,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--images-dir", type=Path, required=True)
    parser.add_argument("--count", type=int, default=40)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = build_output(args.source, args.images_dir, args.count)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        f"selected {result['selected_count']} pages; "
        f"covered {result['coverage']['covered_tag_count']}/"
        f"{result['coverage']['target_tag_count']} target tags"
    )
    if result["missing_images"]:
        print(f"missing images: {len(result['missing_images'])}")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
