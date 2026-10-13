#!/usr/bin/env python3
"""Smoke-test a built turbovault: `--version`, then a stdio MCP session.

Usage: smoke_test.py [--expect-version VERSION] COMMAND [ARG...]

COMMAND is the binary, or anything that runs it with stdio attached, such as
`docker run -i --rm turbovault:ci`.

The session sends `initialize`, the `initialized` notification and
`tools/list`, then closes stdin, which ends the server. It checks the server
names itself, reports the expected version, and lists tools. Stdlib only, so
it runs on every release runner, Windows included.
"""

import json
import os
import subprocess
import sys
import tempfile

TIMEOUT = 60


def fail(message):
    sys.exit(f"smoke test FAILED: {message}")


def main():
    args = sys.argv[1:]
    expected = None
    if args[:1] == ["--expect-version"]:
        if len(args) < 2:
            sys.exit(__doc__)
        expected, args = args[1], args[2:]
    if not args:
        sys.exit(__doc__)
    command = list(args)
    if os.path.exists(command[0]):
        command[0] = os.path.abspath(command[0])

    version = subprocess.run(command + ["--version"], capture_output=True, text=True,
                             timeout=TIMEOUT)
    if version.returncode != 0:
        fail(f"--version exited {version.returncode}: {version.stderr.strip()}")
    print(version.stdout.strip())
    if expected and expected not in version.stdout:
        fail(f"--version printed {version.stdout.strip()!r}, expected {expected}")

    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "turbovault-smoke-test", "version": "0"},
        }},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    stdin = "".join(json.dumps(r) + "\n" for r in requests)

    # An empty working directory, so the server's per-project state starts clean.
    with tempfile.TemporaryDirectory() as cwd:
        session = subprocess.run(command, input=stdin, capture_output=True, text=True,
                                 timeout=TIMEOUT, cwd=cwd)

    responses = {}
    for line in session.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            fail(f"stdout carried a line that is not JSON-RPC: {line[:200]!r}")
        if "id" in message:
            responses[message["id"]] = message

    init = responses.get(1)
    if not init or "result" not in init:
        fail(f"no initialize result (exit {session.returncode}); stderr: {session.stderr[-2000:]}")
    info = init["result"].get("serverInfo", {})
    print(f"initialize: {info.get('name')} {info.get('version')}")
    if expected and info.get("version") != expected:
        fail(f"serverInfo.version is {info.get('version')!r}, expected {expected}")

    listed = responses.get(2)
    if not listed or "result" not in listed:
        fail(f"no tools/list result; stderr: {session.stderr[-2000:]}")
    tools = listed["result"].get("tools", [])
    print(f"tools/list: {len(tools)} tools")
    if not tools:
        fail("tools/list returned no tools")

    print("smoke test passed")


if __name__ == "__main__":
    main()
