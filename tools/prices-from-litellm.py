#!/usr/bin/env python3
"""Compares swarmy's built-in prices with LiteLLM's price list (the list ccusage uses too).

Run by a developer before a release; swarmy itself never goes online. Prints every built-in
`claude-*` / `gemini-*` / `gpt-*` row whose price in force today differs from LiteLLM's, and the
Gemini and OpenAI GPT-5/6 models LiteLLM knows that swarmy has no row for.

    tools/prices-from-litellm.py            # fetch the list from GitHub
    tools/prices-from-litellm.py list.json  # or read a downloaded copy
"""

import datetime
import json
import re
import sys
import urllib.request
from pathlib import Path

URL = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
PRICING = Path(__file__).resolve().parent.parent / "src" / "core" / "pricing" / "builtin.rs"
ROW = re.compile(
    r'\(\s*"([a-z0-9.*-]+)",\s*date\((\d+),\s*(\d+),\s*(\d+)\),\s*([\d.]+),\s*([\d.]+),\s*([\d.]+),'
)


def load_litellm(argv):
    if len(argv) > 1:
        return json.loads(Path(argv[1]).read_text())
    with urllib.request.urlopen(URL, timeout=60) as r:
        return json.load(r)


def per_mtok(entry, key):
    v = entry.get(key)
    return None if v is None else round(v * 1e6, 6)


def litellm_rates(data, model):
    """Input, output, cache read per MTok for `model` (Gemini API entry first; OpenAI's entries
    have no prefix)."""
    for key in (f"gemini/{model}", model, f"anthropic/{model}", f"openai/{model}"):
        e = data.get(key)
        if e and e.get("input_cost_per_token") is not None:
            return key, (
                per_mtok(e, "input_cost_per_token"),
                per_mtok(e, "output_cost_per_token"),
                per_mtok(e, "cache_read_input_token_cost"),
            )
    return None, None


def main(argv):
    data = load_litellm(argv)
    today = datetime.date.today().timetuple()[:3]
    newest = {}
    for m in ROW.finditer(PRICING.read_text()):
        model, y, mo, d, i, o, c = m.groups()
        day = (int(y), int(mo), int(d))
        # The row in force today: LiteLLM lists today's price, not an announced change.
        if day <= today and (model not in newest or day > newest[model][0]):
            newest[model] = (day, (float(i), float(o), float(c)))
    differ = 0
    for model, (day, ours) in sorted(newest.items()):
        if model.endswith("*"):
            continue
        key, theirs = litellm_rates(data, model)
        if theirs is None:
            print(f"?  {model:28} not in LiteLLM")
            continue
        if any(t is not None and abs(t - o) > 1e-9 for t, o in zip(theirs, ours)):
            differ += 1
            print(f"≠  {model:28} ours {ours} (from {day})  LiteLLM {key} {theirs}")
    known = {m.rstrip("*") for m in newest}
    dated = re.compile(r"-\d{4}-\d{2}-\d{2}$")
    for key in sorted(data):
        name = key.split("/", 1)[1] if key.startswith("gemini/") else key
        gemini = key.startswith("gemini/gemini-")
        # OpenAI's undated GPT-5 / GPT-6 chat and responses models (Codex runs them).
        openai = (
            re.match(r"gpt-[56]", key) is not None
            and data[key].get("litellm_provider") == "openai"
            and not dated.search(key)
        )
        # A Gemini row covers the previews of its model; an OpenAI row only its own id.
        covered = any(
            name.startswith(k) if gemini else name == k or (k + "*" in newest and name.startswith(k))
            for k in known
        )
        if (gemini or openai) and data[key].get("mode") in (None, "chat", "responses") and not covered:
            print(f"+  {name:28} in LiteLLM only {litellm_rates(data, name)[1]}")
    print(f"{differ} built-in rows differ")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
