#!/usr/bin/env python3
"""Validate and count the curated archive survey; no network or media required.

    python3 tools/archive_survey.py
    python3 tools/archive_survey.py --json
    python3 tools/archive_survey.py --reference-html /tmp/psx-archive-survey.html

Counts are distinct games, not evidence rows, files, releases, or parsers.
"""

import argparse
import csv
import hashlib
import json
from collections import defaultdict
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
STRUCTURES = {
    "named_directory": "Named directories",
    "offset_size": "Offset and size tables",
    "offset_only": "Offset tables",
    "chunks": "Sequential chunks / sections",
    "folders": "Dedicated folder structures",
    "size_table": "Size tables",
    "paged_or_custom": "Paged / custom streaming containers",
    "hidden_sector_index": "Indexes outside ISO9660 files",
}


class Anchors(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()

    def handle_starttag(self, tag, attrs):
        self.ids.update(value for key, value in attrs if key == "id")


def summarize(csv_path, reference_html=None):
    with csv_path.open(newline="") as stream:
        rows = list(csv.DictReader(stream))
    if not rows:
        raise ValueError("Empty survey")
    expected = {"game", "family", "structure", "release_evidence", "local_disc", "source", "evidence"}
    if set(rows[0]) != expected:
        raise ValueError("Unexpected CSV columns")
    source_ids = None
    if reference_html:
        parser = Anchors()
        parser.feed(reference_html.read_text())
        source_ids = parser.ids
    families = defaultdict(set)
    structures = defaultdict(set)
    local = set()
    scopes = defaultdict(set)
    seen = set()
    for row in rows:
        if not all(row.values()):
            raise ValueError(f"Missing field: {row}")
        identity = row["game"], row["family"], row["source"]
        if identity in seen:
            raise ValueError(f"Duplicate evidence: {identity}")
        seen.add(identity)
        if row["structure"] not in STRUCTURES:
            raise ValueError(f"Unknown structure: {row['structure']}")
        if row["release_evidence"] not in {"demo", "retail", "mixed", "unspecified"}:
            raise ValueError(f"Unknown release evidence: {row}")
        if row["local_disc"] not in {"yes", "no"}:
            raise ValueError(f"Unknown local overlap: {row}")
        source = urlsplit(row["source"])
        if source.hostname == "psx-spx.consoledev.net":
            if not source.fragment:
                raise ValueError(f"Missing source section: {row}")
            if source_ids is not None and source.fragment not in source_ids:
                raise ValueError(f"Missing reference anchor: {source.fragment}")
        elif source.scheme != "https" and row["source"] != "../external_agent_tests/kings_field/PSX_FEEDBACK.md":
            raise ValueError(f"Unexpected source: {row['source']}")
        families[row["family"]].add(row["game"])
        structures[row["structure"]].add(row["game"])
        scopes[row["release_evidence"]].add(row["game"])
        if row["local_disc"] == "yes":
            local.add(row["game"])
    games = {row["game"] for row in rows}

    def counts(groups):
        return [dict(id=key, count=len(members), percent=round(100 * len(members) / len(games), 1),
                     games=sorted(members))
                for key, members in sorted(groups.items(), key=lambda item: (-len(item[1]), item[0]))]

    return {
        "survey_date": "2026-10-15",
        "unit": "distinct named games in the curated documented sample",
        "game_count": len(games),
        "evidence_rows": len(rows),
        "game_family_pairs": len({(r['game'], r['family']) for r in rows}),
        "csv_sha256": hashlib.sha256(csv_path.read_bytes()).hexdigest(),
        "structures": counts(structures),
        "repeated_families": [item for item in counts(families) if item["count"] > 1],
        "main_checkout_overlap": sorted(local),
        "release_evidence": counts(scopes),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, default=ROOT / "research/archive-survey.csv")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--reference-html", type=Path)
    args = parser.parse_args()
    data = summarize(args.csv, args.reference_html)
    if args.json:
        print(json.dumps(data, indent=2))
        return
    print(f"{data['game_count']} games; {data['evidence_rows']} evidence rows; "
          f"{data['game_family_pairs']} distinct game/family pairs")
    print("\nStructure | Games | Sample share")
    for item in data["structures"]:
        print(f"{STRUCTURES[item['id']]} | {item['count']} | {item['percent']}%")
    print("\nRepeated family | Games")
    for item in data["repeated_families"]:
        print(f"{item['id']} | {item['count']}")
    print("\nMain checkout overlap: " + ", ".join(data["main_checkout_overlap"]))


if __name__ == "__main__":
    main()
