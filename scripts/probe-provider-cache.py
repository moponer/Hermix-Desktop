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

READ THIS BEFORE COMPARING TWO RUNS
-----------------------------------
The cache is **content-addressed**, so the *second* run of this script on
the same provider hits everything it sent the first time: probes that
were cold (`cached≈128`) in run 1 come back fully cached in run 2, which
looks like a contradiction. Read only the row where a given body was
**first** sent; a warm rerun measures nothing new.

USAGE
-----
    python scripts/probe-provider-cache.py [model] [--provider NAME]

The model defaults to `reflection/beam-free` on the kilo gateway;
`kilo-auto/free` (the app's default) is an auto-router whose usage numbers
are inconsistent between requests and therefore unusable for measurement.

`--provider NAME` switches to any provider configured under
`llm_providers` in the hermix config (e.g. `--provider minimax`): its
`base_url` / `api_key` are read from config.yaml, a known default base
URL is used when `base_url` is empty, and the key is sent as a bearer
token. The key is never printed.

A provider that does not report `prompt_tokens_details.cached_tokens`
prints `cached=None` — that is itself the answer (no cache accounting,
so no cache claim can be made about it).
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

# Base URL per provider name, used when config.yaml leaves `base_url`
# empty. Must match `build_provider` (`agent_builder.rs`) / `runtime_provider.rs`.
DEFAULT_BASE = {
    "minimax": "https://api.minimax.chat/v1",
    "deepseek": "https://api.deepseek.com/v1",
    "kimi": "https://api.moonshot.cn/v1",
    "moonshot": "https://api.moonshot.cn/v1",
    "zhipu": "https://open.bigmodel.cn/api/paas/v4",
    "qwen": "https://dashscope.aliyuncs.com/compatible-mode/v1",
    "dashscope": "https://dashscope.aliyuncs.com/compatible-mode/v1",
}
DEFAULT_MODEL = {
    "custom-free-kilo": "reflection/beam-free",
    # same default the app uses for MiniMax (`auxiliary_builder::default_models`)
    "minimax": "MiniMax-M3",
}
DEFAULT_PROVIDER = "custom-free-kilo"

PROVIDER = DEFAULT_PROVIDER
MODEL = None
argv = sys.argv[1:]
i = 0
while i < len(argv):
    if argv[i] == "--provider" and i + 1 < len(argv):
        PROVIDER = argv[i + 1]
        i += 2
        continue
    if argv[i] == "--model" and i + 1 < len(argv):
        MODEL = argv[i + 1]
        i += 2
        continue
    MODEL = argv[i]  # back-compat: positional model
    i += 1

SYSTEM = (
    "# System prompt for a cache experiment\n\n"
    + ("This is a stable system prompt used to measure prefix caching. " * 330)
    + "\n\n## Trailing section\nStable content that must stay byte-identical across runs."
)
USER = "Reply with the single word: ok. Do not use any tools."


def load_provider():
    """(base_url, api_key, configured model list) — the key is never printed."""
    try:
        with open(CONFIG, "r", encoding="utf-8") as fh:
            cfg = yaml.safe_load(fh) or {}
    except FileNotFoundError:
        cfg = {}
    prov = (cfg.get("llm_providers") or {}).get(PROVIDER) or {}
    base = (prov.get("base_url") or DEFAULT_BASE.get(PROVIDER) or "").rstrip("/")
    if not base:
        sys.exit(f"provider {PROVIDER!r}: base_url missing from config and no known default")
    return base, prov.get("api_key"), prov.get("model")


_USAGE_SHAPE_PRINTED = False

BASE, API_KEY, CONFIGURED_MODELS = load_provider()
if MODEL is None:
    first = (CONFIGURED_MODELS or "").split(",")[0].strip()
    MODEL = first or DEFAULT_MODEL.get(PROVIDER)
    if not MODEL:
        sys.exit(f"provider {PROVIDER!r}: pass a model name explicitly")


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
    if API_KEY:
        headers["Authorization"] = f"Bearer {API_KEY}"
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
        global _USAGE_SHAPE_PRINTED
        if cached is None and not _USAGE_SHAPE_PRINTED:
            _USAGE_SHAPE_PRINTED = True
            print(f"  (no cache fields — usage keys: {sorted(usage)})")
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
    print(f"provider: {PROVIDER}")
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

    print()
    print("--- phase 3: cache granularity (where does a change stop invalidating?) ---")
    # Same system text, but a *new message* appended after it. If the unit is
    # the message block, the system block stays cached; if it is the whole
    # request, this drops to ~0 like R5/R8.
    tail = [{"role": "assistant", "content": "ok"}, {"role": "user", "content": USER}]
    send("G1 baseline (system+user)", msgs(SYSTEM), None)
    send("G2 identical", msgs(SYSTEM), None)
    send("G3 extra trailing message", msgs(SYSTEM) + tail, None)
    # A one-character edit *inside* the system text.
    mutated = SYSTEM.replace("Trailing section", "Trailing  section", 1)
    send("G4 one space edited inside system", msgs(mutated), None)
    # Second system message appended after the first (block boundary moves).
    send(
        "G5 second system message after it",
        [{"role": "system", "content": SYSTEM}, {"role": "system", "content": "Extra rule."}, {"role": "user", "content": USER}],
        None,
    )


if __name__ == "__main__":
    main()
