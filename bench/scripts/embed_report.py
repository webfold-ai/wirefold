#!/usr/bin/env python3
"""Merge runs of the `embed` bench and price the result.

    embed_report.py tables <prefix>            merge <prefix>.N.txt (median per cell, min-max where the run printed one)
    embed_report.py cost --dir D [prices]      cost model from the DRAM (`big`), batched (`batch`) and resident
                                               (`mrl`, `iso`) runs in D

Prices are inputs, not measurements: --ram $/GB-month, --ssd $/GB-month, --core $/vCPU-month, --util, --iops
$/IOPS-month above --free-iops.
"""
import re, sys, glob, statistics as st, argparse

num = re.compile(r"^-?\d+(\.\d+)?$")

def merge_cell(cells):
    heads = [c.split(" [")[0].strip() for c in cells]
    heads = [h[:-1] if h.endswith("M") and num.match(h[:-1]) else h for h in heads]
    if all(num.match(h) for h in heads):
        vals = [float(h) for h in heads]
        dec = len(heads[0].split(".")[1]) if "." in heads[0] else 0
        s = f"{st.median(vals):.{dec}f}"
        if "[" in cells[0]:
            s += f" [{min(vals):.{dec}f}-{max(vals):.{dec}f}]"
        return s + ("M" if cells[0].split(" [")[0].endswith("M") else "")
    return cells[0]

def tables(prefix):
    files = sorted(glob.glob(prefix + ".*.txt"))
    runs = [open(f).read().splitlines() for f in files]
    out = []
    for i, line in enumerate(runs[0]):
        if line.startswith("|") and all(len(r) == len(runs[0]) and r[i].startswith("|") for r in runs):
            cols = [r[i].strip("|").split("|") for r in runs]
            out.append("|" + "|".join(" " + merge_cell([c[j].strip() for c in cols]) + " " for j in range(len(cols[0]))) + "|")
        elif line.startswith(("ROW", "machine-readable", "<!--")):
            continue
        else:
            out.append(line)
    print("\n".join(out))
    print(f"\n_{len(runs)} runs, median per cell; bracketed ranges are min-max over the runs._")

def rows(d, name, tag):
    """tag -> {format: [fields...]} medians over the runs of file group <name>"""
    acc = {}
    for f in sorted(glob.glob(f"{d}/{name}.*.txt")):
        for line in open(f):
            if line.startswith(tag + "\t"):
                p = line.rstrip("\n").split("\t")
                acc.setdefault(p[1], []).append([float(x) for x in p[2:]])
    return {k: [st.median(col) for col in zip(*v)] for k, v in acc.items()}

def cost(a):
    d = a.dir
    threads = 4
    for line in open(sorted(glob.glob(f"{d}/big.*.txt"))[0]):
        m = re.search(r"threads=(\d+)", line)
        if m: threads = int(m.group(1))
    big = rows(d, "big", "ROW")          # name -> [stride, sec per row per query (all cores), 1-core ns/row]
    bat = rows(d, "batch", "ROWB")       # name -> [stride, ns/row/query at Q=1,4,16,64]
    rec = {c: rows(d, c, "ROW") for c in ("mrl", "iso")}   # name -> [stride, scan_s, ns, recall_rerank, rerank_s]
    GB = 1e9
    configs = [("f32", False), ("f16", False), ("int8 rot", True), ("int4 rot", True), ("2-bit rot", True), ("binary rot", True),
               ("mrl256 int8", True), ("mrl64 int8", True)]
    print(f"### cost model (RAM ${a.ram}/GB-mo, SSD ${a.ssd}/GB-mo, vCPU ${a.core}/mo at {a.util:.0%} utilisation, "
          f"{a.free_iops} free IOPS then ${a.iops}/IOPS-mo; {threads} cores measured)\n")
    print("Core-seconds per query come from the DRAM-resident scan (`big`) and, batched, from the 16-queries-per-pass run (`batch`), scaled linearly in rows. "
          "Re-rank reads 100 f32 rows per query from SSD. Recall is the scan plus x10 re-rank on the mrl-like / isotropic corpus; a row under 0.95 on either is marked.\n")
    for N in (10_000_000, 100_000_000):
        for qps in (1, 50):
            print(f"\n**N = {N//1_000_000}M, {qps} QPS**\n")
            print("| format | recall mrl / iso | RAM $ | SSD $ | IOPS $ | CPU $ unbatched | total unbatched | cores | CPU $ batched x16 | total batched | cores |")
            print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
            table = []
            for name, rerank in configs:
                if name not in big: continue
                stride = big[name][0]
                ram = N * stride / GB * a.ram
                ssd = N * 4096 / GB * a.ssd if rerank else 0.0
                if not rerank: ram = N * stride / GB * a.ram    # the scanned column is the master
                iops = max(0.0, qps * 100 - a.free_iops) * a.iops if rerank else 0.0
                cpu_s = big[name][1] * threads * N
                cores = qps * cpu_s / a.util
                cpu = cores * a.core
                bcell = ("-", "-", "-")
                tot_b = None
                if name in bat:
                    cs_b = bat[name][1 + 2] * 1e-9 * threads * N      # Q=16 column
                    cores_b = qps * cs_b / a.util
                    tot_b = ram + ssd + iops + cores_b * a.core
                    bcell = (f"{cores_b * a.core:,.0f}", f"{tot_b:,.0f}", f"{cores_b:,.1f}")
                r_m = rec["mrl"].get(name, [0, 0, 0, 1.0])[3] if rerank else 1.0
                r_i = rec["iso"].get(name, [0, 0, 0, 1.0])[3] if rerank else 1.0
                flag = " (recall < 0.95)" if min(r_m, r_i) < 0.95 else ""
                tot = ram + ssd + iops + cpu
                table.append((tot, f"{name}{' +rerank' if rerank else ''}{flag}", f"{r_m:.2f} / {r_i:.2f}", ram, ssd, iops, cpu, cores, bcell))
            for tot, nm, rc, ram, ssd, iops, cpu, cores, b in sorted(table):
                print(f"| {nm} | {rc} | {ram:,.0f} | {ssd:,.0f} | {iops:,.0f} | {cpu:,.0f} | {tot:,.0f} | {cores:,.1f} | {b[0]} | {b[1]} | {b[2]} |")

if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tables"); t.add_argument("prefix")
    c = sub.add_parser("cost"); c.add_argument("--dir", required=True)
    c.add_argument("--ram", type=float, default=3.0); c.add_argument("--ssd", type=float, default=0.10)
    c.add_argument("--core", type=float, default=25.0); c.add_argument("--util", type=float, default=0.6)
    c.add_argument("--iops", type=float, default=0.005); c.add_argument("--free-iops", type=int, default=3000, dest="free_iops")
    a = ap.parse_args()
    tables(a.prefix) if a.cmd == "tables" else cost(a)
