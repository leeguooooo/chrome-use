#!/usr/bin/env python3
"""Run the injection drill once per approved model and write one summary (#852).

This is the CI entry point behind `.github/workflows/injection-drill.yml`. For
every model in the policy's `allowed_model_ids` it runs `runner.py --policy`
and keeps the gate's verdict; the public part of each bundle (receipt, config
and per-trial event files, never `private/`) is copied under
`<out>/public/<model>/`, and `<out>/summary.json` records the overall status:

- `ACCEPTED`: every model's evidence verified and met the policy;
- `REJECTED`: a measured policy violation (an attack landed or a task failed);
- `UNVERIFIED`: a bundle that cannot be verified (an unknown trial, a stream the
  runner could not bind to the fixture server, stale or malformed evidence);
- `NOT_RUN`: a prerequisite was missing (no `ANTHROPIC_API_KEY`, no disposable
  host declaration, an unsuitable Claude CLI) and no model was called.

Exit 0 for ACCEPTED and NOT_RUN, 1 for REJECTED, 2 for UNVERIFIED. NOT_RUN
exits 0 so a release without the CI secret still ships; its summary says that
nothing was measured, and `release.sh` publishes that as such.
"""
import argparse
import io
import json
import os
import shutil
import sys
from contextlib import redirect_stdout
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
import runner  # noqa: E402

PUBLIC = ("receipt.json", "config.json")
STATUS_EXIT = {"ACCEPTED": 0, "NOT_RUN": 0, "REJECTED": 1, "UNVERIFIED": 2}


def one_model(model, args, env):
    out = Path(args.out) / "bundles" / model
    argv = ["--out", str(out), "--model", model, "--source-commit", args.source_commit,
            "--chrome-use", args.chrome_use, "--extension", args.extension, "--chrome", args.chrome,
            "--claude", args.claude, "--budget", str(args.budget), "--policy", str(ROOT / "policy.json")]
    if args.launch:
        argv.append("--launch")
    if args.headless:
        argv.append("--headless")
    buf = io.StringIO()
    with redirect_stdout(buf):
        code = runner.main(argv, env)
    try:
        verdict = json.loads(buf.getvalue())
    except ValueError:
        verdict = {"release_gate": "UNVERIFIED", "error": "the gate printed no JSON"}
    if code == runner.NOT_RUN:
        status = "NOT_RUN"
    elif code == 0 and verdict.get("release_gate") == "ACCEPTED_FOR_DECLARED_SCOPE":
        status = "ACCEPTED"
    elif code == 1:
        status = "REJECTED"
    else:
        status = "UNVERIFIED"
    public = Path(args.out) / "public" / model
    if out.is_dir():
        public.mkdir(parents=True)
        for f in sorted(out.iterdir()):
            if f.is_file() and (f.name in PUBLIC or f.name.startswith(("model-", "canary-"))):
                shutil.copyfile(f, public / f.name)
    return {"status": status, "exit": code, "verdict": verdict}


def summarize(models):
    statuses = {m["status"] for m in models.values()}
    for status in ("UNVERIFIED", "REJECTED"):
        if status in statuses:
            return status
    if statuses == {"NOT_RUN"}:
        return "NOT_RUN"
    if statuses == {"ACCEPTED"}:
        return "ACCEPTED"
    # Some models ran and some did not: nothing is proven for the policy's full model list.
    return "UNVERIFIED"


def main(argv=None, env=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", required=True, help="new directory for bundles/, public/ and summary.json")
    p.add_argument("--tag", required=True, help="the release tag being measured, e.g. v1.5.286")
    p.add_argument("--source-commit", required=True)
    p.add_argument("--chrome-use", required=True)
    p.add_argument("--extension", required=True)
    p.add_argument("--chrome", required=True)
    p.add_argument("--launch", action="store_true")
    p.add_argument("--headless", action="store_true")
    p.add_argument("--claude", default="claude")
    p.add_argument("--budget", type=float, default=0.5)
    p.add_argument("--run-url", default="", help="the CI run that produced this summary")
    args = p.parse_args(argv)
    env = dict(os.environ if env is None else env)
    policy = json.loads((ROOT / "policy.json").read_text())
    Path(args.out).mkdir(parents=True, exist_ok=False)
    models = {model: one_model(model, args, env) for model in policy["allowed_model_ids"]}
    status = summarize(models)
    summary = {"schema_version": 1, "tag": args.tag, "source_commit": args.source_commit,
               "generated_at": datetime.now(timezone.utc).isoformat(), "status": status,
               "scope": policy["scope"], "corpus_sha256": policy["corpus_sha256"],
               "policy": {k: policy[k] for k in ("maximum_attack_successes", "minimum_task_completion", "max_age_hours")},
               "run_url": args.run_url, "models": models,
               "note": ("No model was called: a prerequisite was missing, so nothing was measured for this release."
                        if status == "NOT_RUN" else
                        "Synthetic fixture-only scope (restricted-mcp-fixture-only): three fixture tools, a loopback page, "
                        "canaries that only record. Not evidence about the native panel or arbitrary shell attacks.")}
    (Path(args.out) / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps({"status": status, "models": {m: r["status"] for m, r in models.items()}}))
    return STATUS_EXIT[status]


if __name__ == "__main__":
    raise SystemExit(main())
