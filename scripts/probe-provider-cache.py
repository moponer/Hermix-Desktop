#!/usr/bin/env python3
"""
Provider prefix-cache probe — measures what the kilo free gateway does
with the request body we actually send.

WHY THIS EXISTS
---------------
The system-prompt byte-stability work (decision note
docs/notes/proposed/architecture/2026-10-23-prompt-behavior-gap-vs-mature-agent-ecosystems.md)
rests on one question: when something in the request changes, how much
of the provider's prefix cache survives?

That question cannot be answered by reading code — it has to be measured
against the provider.

THE TRAP (read this before drawing conclusions)
----------------------------------------------
The app does NOT send the system prompt as a top-level `system` field.
It puts it into the `messages` array as a `{"role": "system"}` entry
(`provider.rs` serialises every `Message` verbatim). The kilo gateway
**drops a top-level `system` field**: growing the system prompt from 580
to 58,000 characters leaves `prompt_tokens` unchanged. An experiment that
uses the top-level field therefore measures the gateway's own prefix and
every conclusion drawn from it is false. This script uses the app's real
transport.

THE NOISE FLOOR
----------------
The gateway load-balances across backends with independent caches, so a
byte-identical request can come back with `cached=0` in the middle of an
otherwise clean sequence. Single measurements are not conclusive — only
the deltas are. Repeat before believing a number.

USAGE
-----
    python scripts/probe-provider-cache.py [model]

The model defaults to `reflection/beam-free`; `kilo-auto/free` (the app's
default) is an auto-router whose usage numbers are inconsistent between
requests and therefore unusable for measurement. The API key is read
from the hermix config and never printed.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

import yaml

HERMIX_HOME = os.environ.get("HERMIX_HOME") or os.path.join(
    os.path.expanduser("~"), ".hermix"
)
CONFIG = os.path.join(HERMIX_HOME, "config.yaml")
MODEL = sys.argv[1] if len(sys.argv) > 1 else "reflection/beam-free"

SYSTEM = (
    "# System prompt for a cache experiment\n\n"
    + ("This is a stable system prompt used to measure prefix caching. " * 330)
    + "\n\n## Trailing section\nStable content that must stay byte-identical across runs."
)
USER = "Reply with the single word: ok. Do not use any tools."


def load_provider():
    with open(CONFIG, "r", encoding="utf-8") as fh:
        cfg = yaml.safe_load(fh)
    prov = (cfg.get("llm_providers") or {}).get("custom-free-kilo") or {}
    base = (prov.get("base_url") or "").rstrip("/")
    if not base:
        sys.exit("provider config incomplete: base_url missing")
    return base


BASE = load_provider()


def tool(name):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": "A tool that does a thing. " + "x" * 250,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the target file."},
                    "mode": {
                        "type": "string",
                        "enum": ["read", "write", "append"],
                        "description": "Operation mode.",
                    },
                },
                "required": ["path"],
            },
        },
    }


TOOLS = [
    tool(n)
    for n in [
        "read_file", "write_file", "search_files", "terminal",
        "web_search", "web_extract", "todo", "memory",
    ]
]


def send(label, messages, tools, repeat=1):
    body = {
        "model": MODEL,
        "messages": messages,
        "max_tokens": 16,
        "temperature": 0,
        "stream": False,
    }
    if tools is not None:
        body["tools"] = tools
    data = json.dumps(body).encode("utf-8")
    headers = {"Content-Type": "application/json"}
    row = None
    for _ in range(repeat):
        payload = None
        problem = None
        for _attempt in range(3):
            req = urllib.request.Request(
                BASE + "/chat/completions", data=data, headers=headers
            )
            try:
                with urllib.request.urlopen(req, timeout=180) as resp:
                    payload = json.loads(resp.read().decode("utf-8"))
                break
            except urllib.error.HTTPError as exc:
                problem = f"HTTP {exc.code}"
                break
            except Exception as exc:  # noqa: BLE001
                problem = type(exc).__name__
                time.sleep(3)
        if payload is None:
            print(f"{label:<44} FAILED {problem}")
            return None
        usage = payload.get("usage") or {}
        details = usage.get("prompt_tokens_details") or {}
        cached = details.get("cached_tokens")
        if cached is None:
            cached = usage.get("cache_read_input_tokens")
        row = {
            "label": label,
            "prompt": usage.get("prompt_tokens"),
            "cached": cached,
            "creation": usage.get("cache_creation_input_tokens"),
        }
    print(
        f"{row['label']:<44} prompt={row['prompt']!s:<8} cached={row['cached']!s:<8}"
        f" creation={row['creation']}"
    )
    return row


def msgs(system_text):
    return [
        {"role": "system", "content": system_text},
        {"role": "user", "content": USER},
    ]


def main():
    print(f"endpoint: {BASE}/chat/completions")
    print(f"model:    {MODEL}")
    print(f"system chars: {len(SYSTEM)}   tools chars: {len(json.dumps(TOOLS))}")
    print()
    print("--- phase 1: validity (system as a messages entry) ---")
    small = "You are a helpful assistant. " * 20
    big = "You are a helpful assistant. " * 2000
    send("A user only", [{"role": "user", "content": USER}], None)
    send("B user + small system", msgs(small), None)
    send("C user + big system (100x)", msgs(big), None)
    send("D user + big system again", msgs(big), None)
    send("E user + 8 tools", [{"role": "user", "content": USER}], TOOLS)
    send("F user + big system + 8 tools", msgs(big), TOOLS)

    print()
    print("--- phase 2: decisive ---")
    reordered = list(reversed(TOOLS))
    changed = [t for t in TOOLS if t["function"]["name"] != "memory"] + [
        tool("daily_memory")
    ]
    send("R1 baseline", msgs(SYSTEM), TOOLS)
    send("R2 identical (ceiling)", msgs(SYSTEM), TOOLS)
    send("R3 tools reordered", msgs(SYSTEM), reordered)
    send("R4 tool SET changed", msgs(SYSTEM), changed)
    send("R5 system APPENDED", msgs(SYSTEM + " Extra tail."), TOOLS)
    send("R6 tools back to original", msgs(SYSTEM), TOOLS)
    send("R7 identical again", msgs(SYSTEM), TOOLS)
    send("R8 system PREPENDED", msgs("Leading sentence. " + SYSTEM), TOOLS)


if __name__ == "__main__":
    main()
