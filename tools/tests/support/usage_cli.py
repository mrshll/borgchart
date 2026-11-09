"""Fake quota protocol; installed by tests with an absolute interpreter shebang."""

import json
import os
import signal
import sys
import time
from pathlib import Path

assert os.environ.get("USAGE_TEST_GUARD") == "isolated-fake"
assert not any(key.startswith(("HERDR_", "BORGCHART_")) for key in os.environ)
assert "CLAUDECODE" not in os.environ and "CODEX_THREAD_ID" not in os.environ
vendor = Path(sys.argv[0]).name
if vendor == "codex":
    assert sys.argv[1:] == ["app-server", "--stdio"]
else:
    assert "--no-session-persistence" in sys.argv
    assert sys.argv[sys.argv.index("--settings") + 1] == '{"disableAllHooks":true}'
    assert "--strict-mcp-config" in sys.argv
    assert sys.argv[sys.argv.index("--mcp-config") + 1] == '{"mcpServers":{}}'
mode = os.environ.get("USAGE_TEST_MODE", "ok")
if mode == "child":
    child = os.fork()
    if child == 0:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        Path(os.environ["USAGE_TEST_CHILD"]).write_text(str(os.getpid()))
        time.sleep(20)
        os._exit(0)
    while not Path(os.environ["USAGE_TEST_CHILD"]).exists():
        time.sleep(0.001)
for line in sys.stdin:
    request = json.loads(line)
    with open(os.environ["USAGE_TEST_LOG"], "a") as handle:
        handle.write(json.dumps(request) + "\n")
    if mode == "nested":
        print("[" * 2000 + "0" + "]" * 2000, flush=True)
        continue
    if mode == "slow":
        time.sleep(0.2)
    if mode == "timeout":
        time.sleep(20)
    if mode == "eof":
        raise SystemExit(0)
    if mode == "garbage":
        print("PRIVATE malformed response", flush=True)
        continue
    if mode == "flood":
        print("x" * (4 * 1024 * 1024 + 1), flush=True)
        continue
    if mode == "notifications":
        for _ in range(5000):
            print(json.dumps({"method": "notice", "params": "x" * 1024}), flush=True)
        continue
    if request.get("method") == "initialized":
        continue
    initial = (
        request.get("method") == "initialize"
        or request.get("request", {}).get("subtype") == "initialize"
    )
    data = {} if initial else json.loads(os.environ["USAGE_TEST_RESPONSE"])
    if vendor == "codex":
        reply = {"id": request["id"], "result": data}
        if mode == "error" and not initial:
            reply = {
                "id": request["id"],
                "error": {"message": "PRIVATE provider failure"},
            }
    else:
        reply = {
            "type": "control_response",
            "response": {
                "request_id": request["request_id"],
                "subtype": "success",
                "response": data,
            },
        }
        if mode == "error" and not initial:
            reply["response"] = {
                "request_id": request["request_id"],
                "subtype": "error",
                "error": "PRIVATE provider failure",
            }
    print(json.dumps(reply), flush=True)

    if mode == "child" and not initial:
        raise SystemExit(0)
