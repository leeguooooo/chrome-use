#!/usr/bin/env python3
"""
Verify that every SKILL.md in the repository has valid YAML frontmatter.
Parses frontmatter with yaml.safe_load and checks required fields.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent

try:
    import yaml
except ImportError:
    sys.stderr.write("PyYAML is required to check skill frontmatter. Run: pip install pyyaml\n")
    sys.exit(1)

FRONTMATTER_REGEX = re.compile(r"^---\r?\n(.*?)\r?\n---\r?\n", re.DOTALL)


def check_skill_file(skill_path: Path) -> list[str]:
    text = skill_path.read_text(encoding="utf-8", errors="replace")
    match = FRONTMATTER_REGEX.match(text)
    if not match:
        return ["Frontmatter block delimited by '---' not found at start of file"]

    frontmatter_text = match.group(1)
    try:
        data = yaml.safe_load(frontmatter_text)
    except Exception as exc:
        return [f"Invalid YAML: {exc}"]

    if not isinstance(data, dict):
        return ["Frontmatter is not a YAML mapping"]

    errors = []
    for key in ("name", "description"):
        if not data.get(key):
            errors.append(f"Missing required key '{key}'")

    return errors


def main() -> int:
    skill_files = sorted(
        p for p in root.glob("**/SKILL.md")
        if ".git" not in p.parts and "node_modules" not in p.parts and "target" not in p.parts
    )

    if not skill_files:
        sys.stderr.write("No SKILL.md files found.\n")
        return 1

    failures: list[tuple[str, list[str]]] = []
    for skill_path in skill_files:
        rel_path = skill_path.relative_to(root).as_posix()
        errors = check_skill_file(skill_path)
        if errors:
            failures.append((rel_path, errors))
            print(f"FAIL {rel_path}")
            for err in errors:
                print(f"  {err}")
        else:
            print(f"ok   {rel_path}")

    if failures:
        print(f"\n{len(failures)} SKILL.md file(s) failed validation.")
        return 1

    print(f"\nAll {len(skill_files)} SKILL.md files have valid YAML frontmatter.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
