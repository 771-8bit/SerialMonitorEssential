"""Pairwise (t=2) covering array - round 2: full factor model (docs/24 §5.2).

Deterministic greedy generator (same algorithm as pairwise_gen.py) extended to
multi-level factors. Constraints (C-1/C-2: separator/lineWrap/timestamp are
inert in Hex mode etc.) are handled at EXECUTION time as don't-care skips; see
docs/24 §5.6 for the coverage caveat this implies for inert-context pairs.

Outputs the rows both as a markdown table and as a PowerShell hashtable body
ready to paste into pairwise_run2.ps1.
"""

from itertools import combinations, product

FACTORS: dict[str, list[str]] = {
    "connected": ["yes", "no"],
    "stream": ["on", "off"],
    "viewMode": ["ascii", "hex"],
    "lineWrap": ["on", "off"],
    "timestamp": ["on", "off"],
    "separator": ["Space", "Comma", "Tab"],
    "autoScroll": ["on", "off"],
    "plotterOpen": ["yes", "no"],
    "aggMode": ["LTTB", "Average"],
    "viewState": ["live", "inspect", "paused"],
    "windowSec": ["1", "10", "300"],
    "hiddenCh": ["none", "sin"],
}

names = list(FACTORS)


def all_pairs():
    for (i, a), (j, b) in combinations(enumerate(names), 2):
        for va, vb in product(FACTORS[a], FACTORS[b]):
            yield (i, va, j, vb)


uncovered = set(all_pairs())
rows: list[list[str]] = []

while uncovered:
    seed = next(iter(sorted(uncovered)))  # sorted -> deterministic
    row: list[str | None] = [None] * len(names)
    row[seed[0]], row[seed[2]] = seed[1], seed[3]
    for idx in range(len(names)):
        if row[idx] is not None:
            continue
        gains = []
        for v in FACTORS[names[idx]]:
            g = 0
            for jdx in range(len(names)):
                if jdx == idx or row[jdx] is None:
                    continue
                i, j = sorted((idx, jdx))
                vi = v if i == idx else row[i]
                vj = v if j == idx else row[j]
                if (i, vi, j, vj) in uncovered:
                    g += 1
            gains.append((g, v))
        row[idx] = max(gains)[1]
    rows.append(row)  # type: ignore[arg-type]
    for i, j in combinations(range(len(names)), 2):
        uncovered.discard((i, row[i], j, row[j]))

total_pairs = sum(1 for _ in all_pairs())
print(f"# Round-2 pairwise covering array: {len(rows)} rows, {total_pairs} pairs")
print("| # | " + " | ".join(names) + " |")
print("|---|" + "|".join(["---"] * len(names)) + "|")
for k, row in enumerate(rows, 1):
    print(f"| {k} | " + " | ".join(row) + " |")

# Verify coverage
uncovered = set(all_pairs())
for row in rows:
    for i, j in combinations(range(len(names)), 2):
        uncovered.discard((i, row[i], j, row[j]))
assert not uncovered, f"UNCOVERED: {len(uncovered)}"
print(f"# Coverage verified: all {total_pairs} pairs covered")

# Effective-coverage caveat report: pairs whose row context makes them inert
def inert(row: dict[str, str], f: str) -> bool:
    if f in ("lineWrap", "timestamp") and row["viewMode"] == "hex":
        return True
    if f == "separator" and (row["viewMode"] == "hex" or row["timestamp"] == "off"):
        return True
    if f in ("aggMode", "viewState", "windowSec", "hiddenCh") and row["plotterOpen"] == "no":
        return True
    if f == "stream" and row["connected"] == "no":
        return True
    return False


ineffective = 0
for i, j in combinations(range(len(names)), 2):
    for va, vb in product(FACTORS[names[i]], FACTORS[names[j]]):
        covered_effectively = False
        for row in rows:
            if row[i] == va and row[j] == vb:
                rd = dict(zip(names, row))
                if not inert(rd, names[i]) and not inert(rd, names[j]):
                    covered_effectively = True
                    break
        if not covered_effectively:
            ineffective += 1
print(f"# Pairs covered only in inert contexts: {ineffective}/{total_pairs}")
print("#   (these need the constrained-generation follow-up; see docs/24 §5.6)")

print("\n# --- PowerShell rows for pairwise_run2.ps1 ---")
for row in rows:
    kv = "; ".join(f'{n}="{v}"' for n, v in zip(names, row))
    print("  @{" + kv + "},")
