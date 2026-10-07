#!/usr/bin/env python3
"""Generate equivalent local fixture replays; no browser is opened here."""
import argparse
from pathlib import Path
import shlex


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("url", help="loopback fixture server URL ending in /agent-efficiency.html")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if not args.url.startswith("http://127.0.0.1:") or not args.url.endswith("/agent-efficiency.html"):
        parser.error("use the loopback fixture server, not an authenticated destination")
    script = Path(__file__).resolve().parent / "fixtures/agent-efficiency.js"
    variants = {
        "separate": [command for _ in range(3) for command in ('click "#increment" --json', 'snapshot --json')],
        "observe": ['click "#increment" --observe --json'] * 3,
        "batch": ['batch \'click "#increment"\' \'click "#increment"\' \'click "#increment"\' \'snapshot\' --json'],
        "script": [f"script {shlex.quote(str(script))} --json"],
    }
    args.output.mkdir(parents=True, exist_ok=True)
    for name, commands in variants.items():
        task = args.output / f"{name}.txt"
        task.write_text('#! assert text "#count" equals "3"\n' + f"navigate {shlex.quote(args.url)} --json\n" + '\n'.join(commands) + '\n', encoding="utf-8")
        print(task)


if __name__ == "__main__":
    main()
