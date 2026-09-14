"""Prepare the MEC roster and student photographs from the supplied exports.

The source archive names photographs after students, but spelling, punctuation,
initial order, and department folder names are not consistent.  This script
normalises those names, applies the small set of known source corrections, and
writes canonical photographs named by register number.  It refuses to publish
an ambiguous match, so a new export cannot silently attach the wrong face.
"""

from __future__ import annotations

import argparse
import csv
import re
import shutil
import tempfile
import zipfile
from difflib import SequenceMatcher
from pathlib import Path


DEPARTMENT_CODES = {
    "B.Tech in Artificial Intelligence & Data Science": "AIDS",
    "B.E in Computer Science & Engineering (Artificial Intelligence & Machine Learning)": "AIML",
    "B.E in Computer Science & Business Systems": "CSBS",
    "B.E in Computer Science & Engineering": "CSE",
    "B.E in Computer Science & Engineering (Cyber Security)": "CYBER",
    "B.Tech in Information Technology": "IT",
}

IMAGE_FOLDERS = {
    "AIDS": "AIDS",
    "AIML": "AIML",
    "CSBS": "CSBS",
    "CSE": "CSE",
    "CS": "CYBER",
    "IT": "IT",
}

# These files belong to neither a student in their folder nor the supplied
# roster.  Keeping the exclusions explicit makes the one-to-one audit stable.
EXCLUDED_IMAGES = {
    "AIDS/Adhish-eastavan-M.jpg",
    "CS/S-Dhaya.jpg",
    "IT/Preethi-S.jpg",
    "CSE/Aravind-R.jpg",
    "CSE/Bhavani-R.jpg",
}

# Source spelling differs enough here that a fuzzy choice would be needlessly
# fragile.  Values are register numbers from the authoritative CSV.
IMAGE_OVERRIDES = {
    "AIML/Mohammed--Irfan-K.jpg": "MEC25AM21",
    "AIML/Nandini.K.jpg": "MEC25AM36",
    "CS/Pathmapriya-V.jpg": "MEC25CY15",
    "CSE/Ashwin-B.E.CSE.jpg": "MEC25CS08",
    "CSE/darwin-amrish-waran-RKS-CSE.jpg": "MEC25CS10",
    "CSE/Venkata-Mani-Kanta-Sai-K-B.E.CSE.jpg": "MEC25CS19",
    "CSE/Manoj-Kumar-M-B.E.CSE.jpg": "MEC25CS23",
    "CSE/Muthu-Jaya-Priya.jpg": "MEC25CS24",
    "CSE/pon-mareeswaran-CSE.jpg": "MEC25CS28",
    "IT/Abishek.jpg": "MEC25IT02",
    "IT/M.Ramju.jpg": "MEC25IT35",
    "IT/Sasirekha-S-Y.jpg": "MEC25IT26",
}


def name_key(value: str) -> str:
    return re.sub(r"[^a-z0-9]", "", value.casefold())


def image_name_key(value: str) -> str:
    stem = Path(value).stem.casefold()
    stem = re.sub(r"\bb\s*e\b", " ", stem)
    stem = re.sub(r"\bcse\b", " ", stem)
    return name_key(stem)


def similarity(image_name: str, student_name: str) -> float:
    left = image_name_key(image_name)
    right = name_key(student_name)
    direct = SequenceMatcher(None, left, right).ratio()
    # Initials often move from the front of a file name to the end of a CSV
    # name.  Sorted characters are a useful secondary signal without making it
    # strong enough to overrule the actual sequence.
    characters = SequenceMatcher(None, "".join(sorted(left)), "".join(sorted(right))).ratio()
    return direct * 0.8 + characters * 0.2


def detected_extension(data: bytes) -> str:
    if data.startswith(b"\xff\xd8\xff"):
        return ".jpg"
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return ".png"
    raise ValueError("unsupported photograph format")


def read_roster(path: Path) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8-sig", newline="") as source:
        rows = list(csv.DictReader(source))
    expected = {"S.No", "Name", "Department", "Register No", "Phone", "email"}
    if not rows or set(rows[0]) != expected:
        raise SystemExit(f"unexpected CSV columns; expected {sorted(expected)}")
    seen_rolls: set[str] = set()
    seen_emails: set[str] = set()
    for line, row in enumerate(rows, start=2):
        if any(not value.strip() for value in row.values()):
            raise SystemExit(f"row {line} has a missing value")
        if row["Department"] not in DEPARTMENT_CODES:
            raise SystemExit(f"row {line} has an unknown department")
        roll = row["Register No"].strip().upper()
        email = row["email"].strip().casefold()
        if roll in seen_rolls or email in seen_emails:
            raise SystemExit(f"row {line} repeats a register number or email")
        if not re.fullmatch(r"MEC25[A-Z]{2}\d{2}", roll):
            raise SystemExit(f"row {line} has an invalid register number: {roll}")
        if not re.fullmatch(r"91-\d{10}", row["Phone"].strip()):
            raise SystemExit(f"row {line} has an invalid phone number")
        seen_rolls.add(roll)
        seen_emails.add(email)
        row["Register No"] = roll
        row["email"] = email
    return rows


def prepare(csv_path: Path, archive_path: Path, output: Path) -> None:
    rows = read_roster(csv_path)
    by_roll = {row["Register No"]: row for row in rows}
    unmatched = set(by_roll)
    matches: dict[str, tuple[str, bytes]] = {}

    with zipfile.ZipFile(archive_path) as archive:
        candidates: list[tuple[str, str, bytes]] = []
        for info in archive.infolist():
            if info.is_dir():
                continue
            parts = Path(info.filename).parts
            if len(parts) < 3 or parts[-2] not in IMAGE_FOLDERS:
                continue
            relative = f"{parts[-2]}/{parts[-1]}"
            if relative in EXCLUDED_IMAGES:
                continue
            candidates.append((relative, IMAGE_FOLDERS[parts[-2]], archive.read(info)))

    # Fixed source corrections are assigned first.
    remaining: list[tuple[str, str, bytes]] = []
    for relative, department, data in candidates:
        roll = IMAGE_OVERRIDES.get(relative)
        if roll is None:
            remaining.append((relative, department, data))
            continue
        if roll not in unmatched:
            raise SystemExit(f"override {relative} points to an unavailable student {roll}")
        expected_department = DEPARTMENT_CODES[by_roll[roll]["Department"]]
        if department != expected_department:
            raise SystemExit(f"override {relative} crosses departments")
        matches[roll] = (relative, data)
        unmatched.remove(roll)

    # Repeatedly take the strongest department-local match.  A 0.72 threshold
    # catches punctuation/initial changes while rejecting unrelated names.
    scored: list[tuple[float, str, str, bytes]] = []
    for relative, department, data in remaining:
        for roll in unmatched:
            row = by_roll[roll]
            if DEPARTMENT_CODES[row["Department"]] == department:
                scored.append((similarity(relative, row["Name"]), relative, roll, data))
    used_images: set[str] = set()
    for score, relative, roll, data in sorted(scored, reverse=True):
        if score < 0.72 or relative in used_images or roll not in unmatched:
            continue
        matches[roll] = (relative, data)
        unmatched.remove(roll)
        used_images.add(relative)

    unmatched_images = sorted(relative for relative, _, _ in remaining if relative not in used_images)
    if unmatched_images:
        raise SystemExit("unmatched photographs:\n  " + "\n  ".join(unmatched_images))

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        staging = Path(temporary) / output.name
        images = staging / "student-images"
        images.mkdir(parents=True)
        with (staging / "students.csv").open("w", encoding="utf-8", newline="") as target:
            writer = csv.DictWriter(target, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
        for roll, (_, data) in matches.items():
            (images / f"{roll}{detected_extension(data)}").write_bytes(data)
        with (staging / "photo-manifest.csv").open("w", encoding="utf-8", newline="") as target:
            writer = csv.writer(target)
            writer.writerow(["Register No", "Source image", "Prepared image"])
            for roll in sorted(matches):
                source_name, data = matches[roll]
                writer.writerow([roll, source_name, f"student-images/{roll}{detected_extension(data)}"])
        if output.exists():
            shutil.rmtree(output)
        shutil.move(staging, output)

    print(f"Prepared {len(rows)} students and {len(matches)} photographs.")
    if unmatched:
        print("Students without photographs: " + ", ".join(sorted(unmatched)))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("csv", type=Path)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--output", type=Path, default=Path(__file__).with_name("source"))
    args = parser.parse_args()
    prepare(args.csv, args.archive, args.output)


if __name__ == "__main__":
    main()
