#!/usr/bin/env python3
"""Reject new literal text in common GTK user-visible setters.

This is intentionally a small source check rather than a Rust parser.  It only
guards the high-signal call sites; diagnostics, commands, tests and dynamic
labels still need normal code review.
"""

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "crates" / "cloudstack-gtk" / "src"
ALLOWED_LITERALS = {"", "…", "Aa", "+", "-"}

PATTERNS = (
    re.compile(
        r'\.(?:title|subtitle|label|placeholder_text|tooltip_text)\s*\(\s*"'
        r'(?P<value>(?:\\.|[^"\\])*)"'
    ),
    re.compile(
        r'\bset_(?:label|title|subtitle|tooltip_text|placeholder_text)\s*\(\s*"'
        r'(?P<value>(?:\\.|[^"\\])*)"'
    ),
    re.compile(
        r'\b(?:toast|show_error|show_warning)\s*\([^\n,]+,\s*&?\s*"'
        r'(?P<value>(?:\\.|[^"\\])*)"'
    ),
)


def main() -> int:
    violations: list[tuple[Path, int, str, str]] = []
    for path in sorted(SOURCE_ROOT.rglob("*.rs")):
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if "i18n-allow:" in line:
                continue
            for pattern in PATTERNS:
                match = pattern.search(line)
                if not match:
                    continue
                value = match.group("value")
                if value not in ALLOWED_LITERALS:
                    violations.append((path, line_number, value, line.strip()))
                break

    if not violations:
        return 0

    for path, line_number, value, source in violations:
        relative = path.relative_to(ROOT)
        print(f"{relative}:{line_number}")
        print(f'  hard-coded user-visible text: "{value}"')
        print(f"  {source}")
        print("  use UiMessage/Fluent or add an i18n-allow reason")
    return 1


if __name__ == "__main__":
    sys.exit(main())
