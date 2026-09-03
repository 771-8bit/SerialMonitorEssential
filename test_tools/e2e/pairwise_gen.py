"""Greedy pairwise (t=2) covering array generator (IPOG-lite).

Factors for the SerialMonitorEssential UI mode-combination test.
Deterministic (no randomness) so the array is reproducible.
"""

from itertools import combinations, product

FACTORS: dict[str, list[str]] = {
    "connected": ["yes", "no"],
    "viewMode": ["ascii", "hex"],
    "lineWrap": ["on", "off"],
    "timestamp": ["on", "off"],
    "autoScroll": ["on", "off"],
    "plotterOpen": ["yes", "no"],
    "aggMode": ["LTTB", "Average"],
    "plotView": ["live", "paused"],
}

# Constraints: combinations that cannot occur / are meaningless.
# - aggMode/plotView only meaningful when plotterOpen=yes (we still assign
#   values but they are "don't care"; constraint-free generation is fine,
#   execution will skip plotter steps when plotterOpen=no).
# - lineWrap/timestamp only apply in ascii view (same treatment).

names = list(FACTORS)
def all_pairs():
    for (i, a), (j, b) in combinations(enumerate(names), 2):
        for va, vb in product(FACTORS[a], FACTORS[b]):
            yield (i, va, j, vb)

uncovered = set(all_pairs())
rows: list[list[str]] = []

while uncovered:
    best_row, best_gain = None, -1
    # Greedy: try each combination of values for the first uncovered pair,
    # then extend one factor at a time choosing the value covering most pairs.
    seed = next(iter(uncovered))
    row = [None] * len(names)
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
    rows.append(row)
    for i, j in combinations(range(len(names)), 2):
        uncovered.discard((i, row[i], j, row[j]))

print(f"# Pairwise covering array: {len(rows)} rows x {len(names)} factors")
print("| # | " + " | ".join(names) + " |")
print("|---|" + "|".join(["---"] * len(names)) + "|")
for k, row in enumerate(rows, 1):
    print(f"| {k} | " + " | ".join(row) + " |")

# Sanity: verify full pairwise coverage
uncovered = set(all_pairs())
for row in rows:
    for i, j in combinations(range(len(names)), 2):
        uncovered.discard((i, row[i], j, row[j]))
assert not uncovered, f"UNCOVERED: {len(uncovered)}"
print(f"\n# Coverage verified: all {sum(1 for _ in all_pairs())} pairs covered")
