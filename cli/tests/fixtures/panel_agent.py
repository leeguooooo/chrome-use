#!/usr/bin/env python3
"""Synthetic CLI protocol fixture. Never starts a browser, shell tool or model."""
import json
import os
import sys
import time

if "--version" in sys.argv:
    print("0.0.1 (Claude Code)"); sys.exit(0)

codex = "app-server" in sys.argv
inspection = "--no-session-persistence" in sys.argv
session = str(__import__("uuid").uuid4()) if os.environ.get("PANEL_FIXTURE_UUID_SESSION") else ("synthetic-codex-thread" if codex else "synthetic-claude-session")
turn = None
pending = None


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def complete(text, status="completed"):
    global turn
    if codex:
        emit({"method": "item/agentMessage/delta", "params": {"threadId": session, "turnId": turn, "itemId": "fixture-output", "delta": text}})
        emit({"method": "turn/completed", "params": {"threadId": session, "turn": {"id": turn, "status": status, "error": {"message": "synthetic failure"} if status == "failed" else None}}})
    else:
        emit({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": text}}})
        emit({"type": "result", "subtype": "success" if status == "completed" else "error_during_execution", "is_error": status != "completed", "session_id": session})
    turn = None


for line in sys.stdin:
    message = json.loads(line)
    if codex:
        method = message.get("method")
        if method == "initialize":
            inspection = message["params"]["clientInfo"]["name"] == "chrome_use_inspection"
            emit({"id": message["id"], "result": {}})
        elif method == "account/read":
            assert message["params"]["refreshToken"] is False
            emit({"id":message["id"],"result":{"account":{"type":"chatgpt","email":"private@synthetic.invalid","accessToken":"synthetic-never-transport"},"requiresOpenaiAuth":True}})
        elif method == "model/list":
            page = message["params"].get("cursor")
            data=[{"model":"synthetic-codex-model","displayName":"Synthetic Codex","isDefault":True,"supportedReasoningEfforts":[{"reasoningEffort":"high"}]}] if page is None else [{"model":"synthetic-codex-alternate","displayName":"Synthetic alternate","isDefault":False}]
            emit({"id":message["id"],"result":{"data":data,"nextCursor":"page-two" if page is None else None}})
        elif method == "config/read":
            emit({"id": message["id"], "result": {"config": {"mcp_servers": {"other_browser": {"command": "never-executed"}}}}})
        elif method in ("thread/start", "thread/resume"):
            assert not inspection, "Metadata inspection started a conversation"
            params = message["params"]
            mcp = params["config"]["mcp_servers"]
            assert mcp["other_browser"]["enabled"] is False
            assert mcp["chrome_use"]["env"]["AGENT_BROWSER_BROWSER"]
            assert mcp["chrome_use"]["env"]["AGENT_BROWSER_SESSION"].startswith("panel-")
            assert mcp["chrome_use"]["env"]["CHROME_USE_RELAY_DIR"] == os.environ["CHROME_USE_RELAY_DIR"]
            if method == "thread/resume":
                assert params["threadId"] == session
            policy = f'{params.get("approvalPolicy")}/{params.get("sandbox")}'
            emit({"id": message["id"], "result": {"thread": {"id": session}, "model": "synthetic-codex-model"}})
        elif method == "turn/start":
            assert not inspection, "Metadata inspection sent a prompt"
            turn = "turn-" + message["params"]["clientUserMessageId"]
            text = message["params"]["input"][0]["text"]
            emit({"id": message["id"], "result": {"turn": {"id": turn}}})
            emit({"method": "turn/started", "params": {"threadId": session, "turn": {"id": turn}}})
            if text in ("wait", "wait-exit"):
                continue
            if text == "approval":
                pending = 17
                emit({"id": pending, "method": "item/commandExecution/requestApproval", "params": {"threadId": session, "turnId": turn, "itemId": "fixture-tool", "command": "synthetic command, never executed"}})
                continue
            if text == "policy":
                complete("policy: " + policy, "completed")
                continue
            if text == "fail":
                time.sleep(0.4)
            complete("synthetic reply: " + text, "failed" if text == "fail" else "completed")
        elif method == "turn/interrupt":
            time.sleep(0.2)
            if text == "wait-exit": sys.exit(0)
            emit({"id": message["id"], "result": {}})
            complete("synthetic stop", "interrupted")
        elif method is None and message.get("id") == pending:
            emit({"method": "serverRequest/resolved", "params": {"threadId": session, "requestId": pending}})
            complete("synthetic approval handled")
            pending = None
    else:
        if not inspection:
            assert sys.argv[sys.argv.index('--permission-prompt-tool')+1] == 'stdio'
        if message.get("type") == "control_request":
            subtype = message["request"]["subtype"]
            inspection = inspection or message["request_id"] == "panel-inspect-init"
            body={"models":[{"value":"synthetic-claude-model","displayName":"Synthetic Claude","supportedEffortLevels":["low","high"]}],"account":{"email":"private@synthetic.invalid","token":"synthetic-never-transport"}} if inspection else {}
            if inspection:
                assert "--no-session-persistence" in sys.argv
                assert json.loads(sys.argv[sys.argv.index("--mcp-config")+1]) == {"mcpServers":{}}
                assert sys.argv[sys.argv.index("--tools")+1] == ""
                assert json.loads(sys.argv[sys.argv.index("--settings")+1])["disableAllHooks"] is True
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"], "response": body}})
            if subtype == "interrupt" and turn is not None:
                time.sleep(0.2)
                if text == "wait-exit": sys.exit(0)
                complete("synthetic stop", "interrupted")
        elif message.get("type") == "user":
            assert not inspection, "Metadata inspection sent a prompt"
            text = message["message"]["content"]
            turn = message["uuid"]
            emit({"type": "system", "subtype": "init", "session_id": session, "model": "synthetic-claude-model"})
            if text in ("wait", "wait-exit"):
                continue
            if text == "approval":
                pending = "fixture-permission"
                emit({"type": "control_request", "request_id": pending, "request": {"subtype": "can_use_tool", "tool_use_id": "fixture-tool-use", "tool_name": "AskUserQuestion", "input": {"questions": [{"question": "Synthetic question", "header": "Fixture", "options": [{"label": "One", "description": "Synthetic"}, {"label": "Two", "description": "Synthetic"}], "multiSelect": False}]}}})
                emit({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"unrelated-tool","content":"unrelated"}]}})
                continue
            if text == "fail":
                time.sleep(0.4)
            complete("synthetic reply: " + text, "failed" if text == "fail" else "completed")
        elif message.get("type") == "control_response" and message["response"]["request_id"] == pending:
            emit({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"fixture-tool-use","content":"confirmed"}]}})
            complete("synthetic question handled")
            pending = None
