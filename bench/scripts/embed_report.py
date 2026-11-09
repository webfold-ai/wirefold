#!/usr/bin/env python3
"""Merge N runs of `embed` (median per cell, min-max for the scan latency) and add the cost model.

    embed_report.py <run-prefix> [--ram 3.0] [--ssd 0.10] [--core 25.0] [--util 0.6]

Reads <prefix>.1.txt, <prefix>.2.txt, ... Prices are inputs, not measurements (defaults: $/GB-month RAM,
$/GB-month SSD, $/vCPU-month, and the utilisation a core can be held at).
"""
import re, sys, glob, statistics as st, argparse

ap = argparse.ArgumentParser()
ap.add_argument("prefix")
ap.add_argument("--ram", type=float, default=3.0)
ap.add_argument("--ssd", type=float, default=0.10)
ap.add_argument("--core", type=float, default=25.0)
ap.add_argument("--util", type=float, default=0.6)
a = ap.parse_args()

files = sorted(glob.glob(a.prefix + ".*.txt"))
runs = [open(f).read().splitlines() for f in files]
num = re.compile(r"^-?\d+(\.\d+)?$")

def merge_cell(cells):
    heads = [c.split(" [")[0].strip() for c in cells]
    if all(num.match(h) for h in heads):
        vals = [float(h) for h in heads]
        dec = len(heads[0].split(".")[1]) if "." in heads[0] else 0
        m = st.median(vals)
        s = f"{m:.{dec}f}"
        if "[" in cells[0]:
            s += f" [{min(vals):.{dec}f}-{max(vals):.{dec}f}]"
        return s
    return cells[0]

out = []
for i, line in enumerate(runs[0]):
    if line.startswith("|") and all(len(r) == len(runs[0]) and r[i].startswith("|") for r in runs):
        cols = [r[i].strip("|").split("|") for r in runs]
        out.append("|" + "|".join(" " + merge_cell([c[j].strip() for c in cols]) + " " for j in range(len(cols[0]))) + "|")
    elif line.startswith("ROW") or line.startswith("machine-readable") or line.startswith("<!--"):
        continue
    else:
        out.append(line)
print("\n".join(out))
print(f"\n_{len(runs)} runs, median per cell; bracketed ranges are min-max over the runs._\n")

# ---- cost model ----
rows = {}
n = None
for r in runs:
    for line in r:
        if line.startswith("machine-readable"):
            threads = int(re.search(r"threads=(\d+)", line).group(1)); n = int(re.search(r"n=(\d+)", line).group(1))
        if line.startswith("ROW"):
            _, name, stride, lat, ns, rec, lat2 = line.split("\t")
            rows.setdefault(name, []).append((int(stride), float(lat), float(ns), float(rec), float(lat2)))
med = lambda name, k: st.median(x[k] for x in rows[name])
GB = 1e9
def cost(N, qps, coarse, rerank):
    """Monthly dollars for an exact scan: columns resident, cores sized for the query rate."""
    st_b = rows[coarse][0][0]
    ram = N * st_b / GB * a.ram
    ssd = (N * 4096 / GB * a.ssd) if (coarse != "f32") else 0.0   # the f32 master lives on SSD when a smaller column is scanned
    if coarse == "f32":
        ram = N * 4096 / GB * a.ram
    lat = med(coarse, 1) if not rerank else med(coarse, 4)
    cpu_s = lat * threads * (N / n)            # core-seconds per query, scaled linearly in rows
    cores = qps * cpu_s / a.util
    return ram, ssd, cores * a.core, ram + ssd + cores * a.core, cores
print(f"### cost model (inputs: RAM ${a.ram}/GB-mo, SSD ${a.ssd}/GB-mo, vCPU ${a.core}/mo at {a.util:.0%} utilisation; "
      f"scan cost scaled linearly from the measured n={n})\n")
print("Rows scan one column in RAM; rows marked `+rerank` also read {} f32 master vectors per query from SSD (their IO is not priced).\n".format(10 * 10))
print("| N | QPS | format | recall@10 | RAM $ | SSD $ | CPU $ | total $/month | cores |")
print("|---:|---:|---|---:|---:|---:|---:|---:|---:|")
for N in (10_000_000, 100_000_000):
    for qps in (1, 50):
        best = None
        table = []
        for name, rerank in [("f32", False), ("f16", False), ("int8", True), ("int4", True), ("2-bit rot", True), ("binary rot", True),
                             ("mrl256 int8", True), ("mrl128 int8", True), ("mrl64 int8", True)]:
            if name not in rows: continue
            ram, ssd, cpu, tot, cores = cost(N, qps, name, rerank)
            table.append((tot, name + (" +rerank" if rerank else ""), ram, ssd, cpu, cores, med(name, 3)))
        for tot, name, ram, ssd, cpu, cores, rec in sorted(table):
            print(f"| {N//1_000_000}M | {qps} | {name} | {rec:.2f} | {ram:,.0f} | {ssd:,.0f} | {cpu:,.0f} | {tot:,.0f} | {cores:,.1f} |")
