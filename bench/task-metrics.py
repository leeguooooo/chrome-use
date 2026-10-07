#!/usr/bin/env python3
"""Summarize one explicitly bounded replay run; never infer model turns."""
import argparse
import csv
import json
import math
import shlex
from pathlib import Path


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)] if values else None


def summarize(path, trace_path=None):
    meta, rows, assertions = {}, [], []
    with open(path, encoding="utf-8", newline="") as handle:
        for raw in handle:
            if raw.startswith("# "):
                key, _, value = raw[2:].rstrip("\n").partition("\t")
                if key == "assert":
                    assertions.append(value.split("\t", 1)[0])
                else:
                    if key in meta:
                        raise ValueError(f"duplicate metadata {key}: multiple runs need separate files")
                    meta[key] = value
            elif raw.strip():
                rows.append(next(csv.reader([raw], delimiter="\t")))
    if not rows or rows[0][:5] != ["n", "ms", "bytes", "rc", "cmd"]:
        raise ValueError("expected replay TSV header n/ms/bytes/rc/cmd")
    header = rows.pop(0)
    calls = [dict(zip(header, row)) for row in rows]
    for index, call in enumerate(calls, 1):
        if len(rows[index - 1]) != len(header) or int(call["n"]) != index:
            raise ValueError("malformed or non-sequential replay row")
        if not math.isfinite(float(call["ms"])) or float(call["ms"]) < 0 or int(call["bytes"]) < 0:
            raise ValueError("nonfinite/negative elapsed time or negative bytes")
        if "unknown" in call and call["unknown"] not in ("true", "false", "unclassified"):
            raise ValueError("invalid unknown classification")
        if "timed_out" in call and call["timed_out"] not in ("true", "false"):
            raise ValueError("invalid timed_out classification")
    if "task_wall_ms" in meta and (not math.isfinite(float(meta["task_wall_ms"])) or float(meta["task_wall_ms"]) < 0):
        raise ValueError("task_wall_ms must be finite and nonnegative")
    if any(character in value for key, value in meta.items() for character in ("\t", "\r", "\0")):
        raise ValueError("invalid metadata control character")
    if meta.get("binary_source_verified", "false") != "false":
        raise ValueError("this collector has no build-receipt verification support")
    latencies = [float(call["ms"]) for call in calls]
    features = {name: 0 for name in ("batch", "script", "observe")}
    for call in calls:
        args = shlex.split(call["cmd"])
        for name in features:
            # This counts invocations containing a feature, not nested actions.
            features[name] += int((bool(args) and args[0] == name) or (name == "observe" and "--observe" in args))
    failures = sum(int(call["rc"]) != 0 for call in calls)
    verdict = meta.get("verdict", "none")
    checked = bool(assertions)
    passed = checked and all(item == "pass" for item in assertions) and verdict == "pass"
    result = {
        "schema": 1, "run_id": meta.get("run_id"), "task": meta.get("task"),
        "boundary": "explicit_run" if meta.get("run_id") else "single_replay_file",
        "binary": meta.get("binary"), "version": meta.get("version"),
        "binary_source_verified": False,
        "binary_sha256": meta.get("binary_sha256"), "source_sha256": meta.get("source_sha256"),
        "temperature": meta.get("temperature", "legacy_warmup_requested" if meta.get("warm") == "true" else "unknown"),
        "cli_calls": len(calls), "assertion_cli_calls": len(assertions),
        "warmup_cli_calls": int(meta["warmup_cli_calls"]) if "warmup_cli_calls" in meta else None,
        "model_round_trips": None, "trace_tool_calls": None,
        "call_time_sum_ms": sum(latencies), "task_wall_ms": float(meta["task_wall_ms"]) if "task_wall_ms" in meta else None,
        "call_p50_ms": percentile(latencies, .5), "call_p95_ms": percentile(latencies, .95),
        "response_utf8_bytes": sum(int(call["bytes"]) for call in calls) if meta.get("bytes_encoding") == "utf-8" else None,
        "legacy_response_units": sum(int(call["bytes"]) for call in calls) if meta.get("bytes_encoding") != "utf-8" else None,
        "failed_cli_calls": failures,
        "timed_out_cli_calls": sum(call["timed_out"] == "true" for call in calls) if "timed_out" in header else None,
        "stopped_after_timeout": meta.get("stopped_after_timeout") == "true",
        "unknown_calls": sum(call["unknown"] == "true" for call in calls) if "unknown" in header else None,
        "unclassified_outcomes": sum(call["unknown"] == "unclassified" for call in calls) if "unknown" in header else len(calls),
        "feature_calls": features,
        "feature_rates": {name: count / len(calls) if calls else None for name, count in features.items()},
        "postcondition": "pass" if passed else ("fail" if checked else "unverified"),
        "all_cli_calls_succeeded": failures == 0,
        "successful_task": passed and bool(calls),
        "warnings": [],
    }
    if passed and failures:
        result["warnings"].append("Postcondition passed after CLI errors; keep recovery cost and failure count in comparisons.")
    if not result["successful_task"]:
        result["warnings"].append("Run is not a verified completed task; fewer calls or lower latency do not prove an improvement.")
    if trace_path:
        events = [json.loads(line) for line in Path(trace_path).read_text(encoding="utf-8").splitlines() if line.strip()]
        run_id = meta.get("run_id")
        if not run_id or any(event.get("run_id") != run_id for event in events):
            raise ValueError("trace must contain only the TSV's explicit run_id")
        if len(events) < 2 or events[0].get("event") != "run_start" or events[-1].get("event") != "run_end":
            raise ValueError("trace requires run_start first and run_end last")
        if any(event.get("event") not in ("model_turn", "tool_call") for event in events[1:-1]):
            raise ValueError("unsupported trace event or nested run boundary")
        for kind, field in (("model_turn", "model_round_trips"), ("tool_call", "trace_tool_calls")):
            selected = [event for event in events if event["event"] == kind]
            ids = [event.get("id") for event in selected]
            if any(not isinstance(value, str) or not value for value in ids) or len(set(ids)) != len(ids):
                raise ValueError(f"{kind} requires unique nonempty string ids")
            result[field] = len(ids)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tsv", type=Path)
    parser.add_argument("--trace", type=Path, help="explicit caller model/tool events for this run only")
    args = parser.parse_args()
    try:
        print(json.dumps(summarize(args.tsv, args.trace), ensure_ascii=False, indent=2))
    except (ValueError, KeyError, OSError, TypeError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
