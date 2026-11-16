#!/usr/bin/env python3
"""Validate the documentation the way CI does.

Checks, for every file given:

  * every relative link/image target exists on disk
  * every in-page ``#anchor`` resolves to a heading slug (GitHub's rules)
  * every ```toml block is real TOML, and — when a binary is provided — is
    accepted by the program itself (``stratum dump``), so a config example the
    tool rejects can never ship
  * every ```sh block that starts with a shebang is valid shell syntax

Usage:
  scripts/check_docs.py README.md HANDOFF.md
  scripts/check_docs.py README.md --bin target/release/stratum
  scripts/check_docs.py README.md --extract /tmp/blocks   # dump blocks for poking

Exit codes: 0 all good, 1 problems found, 2 usage error.
"""

import argparse
import pathlib
import re
import subprocess
import sys
import tempfile

try:  # Python 3.11+; the runner has it, the author's Mac may not.
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    tomllib = None


def slug(heading: str) -> str:
    h = heading.strip().lower()
    h = re.sub(r"[^\w\s-]", "", h)
    return re.sub(r"\s+", "-", h)


def check_file(path: pathlib.Path, binary: pathlib.Path | None, extract: pathlib.Path | None) -> list[str]:
    src = path.read_text(encoding="utf-8")
    base = path.parent
    problems: list[str] = []

    headings = {slug(h) for h in re.findall(r"^#{1,6}\s+(.+?)\s*$", src, re.M)}
    anchors = set(re.findall(r"\]\(#([^)\s]+)\)", src))
    anchors |= set(re.findall(r'href="#([^"]+)"', src))
    for a in sorted(anchors - headings):
        problems.append(f"dangling anchor: #{a}")

    targets = re.findall(r"\]\(([^)\s]+)\)", src)
    targets += re.findall(r'(?:src|href)="([^"]+)"', src)
    for t in targets:
        if t.startswith(("http://", "https://", "mailto:", "#")):
            continue
        target = t.split("#", 1)[0]
        if target and not (base / target).exists():
            problems.append(f"missing target: {t}")

    blocks = re.findall(r"```(\w*)\n(.*?)```", src, re.S)
    if extract:
        extract.mkdir(parents=True, exist_ok=True)

    for i, (lang, body) in enumerate(blocks):
        if extract:
            ext = {"toml": "toml", "sh": "sh", "bash": "sh", "json": "json"}.get(lang, "txt")
            (extract / f"{path.stem}_{i:02d}.{ext}").write_text(body, encoding="utf-8")

        if lang == "toml":
            if tomllib is not None:
                try:
                    tomllib.loads(body)
                except Exception as e:  # noqa: BLE001 - report anything the parser says
                    problems.append(f"block {i} (toml) does not parse: {e}")
                    continue
            if binary is not None:
                with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as f:
                    f.write(body)
                    tmp = f.name
                try:
                    r = subprocess.run(
                        [str(binary), "dump", tmp], capture_output=True, text=True
                    )
                    if r.returncode != 0:
                        msg = (r.stderr or r.stdout).strip().splitlines()
                        problems.append(
                            f"block {i} (toml) rejected by `stratum dump`: {msg[-1] if msg else '?'}"
                        )
                finally:
                    pathlib.Path(tmp).unlink(missing_ok=True)

        elif lang in ("sh", "bash") and body.lstrip().startswith("#!"):
            with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False) as f:
                f.write(body)
                tmp = f.name
            try:
                r = subprocess.run(["sh", "-n", tmp], capture_output=True, text=True)
                if r.returncode != 0:
                    problems.append(f"block {i} (shell) fails `sh -n`: {r.stderr.strip()}")
            finally:
                pathlib.Path(tmp).unlink(missing_ok=True)

    print(
        f"{path}: {len(headings)} headings, {len(anchors)} anchors, "
        f"{len(blocks)} code blocks"
    )
    return problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+", type=pathlib.Path)
    ap.add_argument("--bin", type=pathlib.Path, default=None, help="stratum binary to validate config blocks with")
    ap.add_argument("--extract", type=pathlib.Path, default=None, help="write blocks here for inspection")
    args = ap.parse_args()

    if args.bin is not None and not args.bin.is_file():
        print(f"--bin {args.bin}: not a file (build it first)")
        return 2

    failures = 0
    for path in args.files:
        if not path.is_file():
            print(f"no such file: {path}")
            return 2
        problems = check_file(path, args.bin, args.extract)
        for p in problems:
            print("  " + p)
        if not problems:
            print("  links, anchors and code blocks OK")
        failures += len(problems)

    if failures:
        print(f"{failures} problem(s)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
