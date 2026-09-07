#!/usr/bin/env python3
"""Extract the reference line from a codex rollout log: round trips, bytes
returned to the model, tool-exec time vs model think time.
Usage: codex_rollout_stats.py ~/.codex/**/rollout-*.jsonl
Find candidates: grep -rl '"browser_use' ~/.codex/sessions ~/.codex/archived_sessions
"""
import json, sys, statistics as st
from datetime import datetime

def ts(s): return datetime.fromisoformat(s.replace('Z', '+00:00')).timestamp()

for path in sys.argv[1:]:
    ev, results = [], []
    for line in open(path):
        try: d = json.loads(line)
        except Exception: continue
        p = d.get('payload') or {}
        t = p.get('type')
        if t in ('function_call', 'function_call_output',
                 'custom_tool_call', 'custom_tool_call_output'):
            ev.append((ts(d['timestamp']), t))
        it = p.get('item') or {}
        if it.get('type') == 'McpToolCall' and it.get('server') == 'cua_repl':
            results.append(''.join(c.get('text', '')
                                   for c in (it.get('result', {}).get('content') or [])))
    tool, think = [], []
    i = 0
    while i < len(ev):
        if ev[i][1] in ('function_call', 'custom_tool_call'):
            j = i + 1
            while j < len(ev) and not ev[j][1].endswith('_output'): j += 1
            if j < len(ev):
                tool.append(ev[j][0] - ev[i][0])
                k = j + 1
                while k < len(ev) and ev[k][1].endswith('_output'): k += 1
                if k < len(ev): think.append(ev[k][0] - ev[j][0])
                i = j
        i += 1
    clip = lambda a: [x for x in a if 0 <= x < 600]
    tool, think = clip(tool), clip(think)
    by = [len(r) for r in results]
    print(f"file: {path}")
    print(f"  cua_repl round trips = {len(results)}")
    if by:
        print(f"  bytes returned: median={st.median(by):.0f} total={sum(by)}")
    if tool:
        print(f"  tool exec:   median={st.median(tool):.2f}s total={sum(tool):.0f}s")
    if think:
        print(f"  model think: median={st.median(think):.2f}s total={sum(think):.0f}s")
