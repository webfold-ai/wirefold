#!/usr/bin/env python3
"""Merge several runs of `wirefold-bench report` into one: every cell that is the same in all runs is kept (sizes
and ratios do not vary), and every cell that differs (a timing, or a rate derived from one) becomes the median of
the runs. Usage: merge_reports.py run1.md run2.md run3.md > merged.md"""
import statistics
import sys


def cells(line):
    return [c.strip() for c in line.strip().strip("|").split("|")]


def number(text):
    t = text.strip().rstrip("%")
    try:
        return float(t)
    except ValueError:
        return None


def merge_cell(values):
    if len(set(values)) == 1:
        return values[0]
    nums = [number(v) for v in values]
    if any(n is None for n in nums):
        raise SystemExit(f"cells differ and are not numbers: {values}")
    med = statistics.median(nums)
    # keep the precision the cells were printed with
    decimals = max((len(v.split(".")[1]) if "." in v else 0) for v in values)
    suffix = "%" if values[0].endswith("%") else ""
    return f"{med:.{decimals}f}{suffix}"


def main(paths):
    runs = [open(p).read().split("\n") for p in paths]
    if len({len(r) for r in runs}) != 1:
        raise SystemExit("the runs have different numbers of lines")
    out = []
    for lines in zip(*runs):
        if all(l.startswith("|") for l in lines) and len({len(cells(l)) for l in lines}) == 1:
            cols = list(zip(*[cells(l) for l in lines]))
            out.append("| " + " | ".join(merge_cell(list(c)) for c in cols) + " |")
        elif len(set(lines)) == 1:
            out.append(lines[0])
        else:
            raise SystemExit(f"non-table lines differ: {lines}")
    print("\n".join(out))


if __name__ == "__main__":
    main(sys.argv[1:])
