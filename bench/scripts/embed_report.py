#!/usr/bin/env python3
"""Merge runs of the `embed` bench and price the result.

    embed_report.py tables <prefix>            merge <prefix>.N.txt (median per cell, min-max where the run printed one)
    embed_report.py csv --dir D                the measured table for `tcofold::vector` (examples/vector_demo.rs), from the
                                               DRAM (`big`), batched (`batch`) and resident (`mrl`, `iso`) runs in D

There is no cost model here: pricing is tcofold's (rent-vs-own, synthetic rates flagged), not a second copy.
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

def csv_table(d):
    """The measured table `tcofold::vector::parse_formats` reads: core-seconds per row per query (median and the ends of the
    median's 95% interval, all cores' time times the thread count), unbatched and at 16 queries per pass, and recall."""
    threads = 4
    for line in open(sorted(glob.glob(f"{d}/big.*.txt"))[0]):
        m = re.search(r"threads=(\d+)", line)
        if m: threads = int(m.group(1))
    big = rows(d, "big", "ROW")      # name -> [stride, med, lo, hi, 1-core ns/row]   (seconds per row per query, all cores)
    bat = rows(d, "batch", "ROWB")   # name -> [stride, (med, lo, hi) x {1,4,16,64} queries per pass]  (ns per row per query)
    rec = {c: rows(d, c, "ROW") for c in ("mrl", "iso")}   # name -> [stride, scan_s, ns, recall_rerank, rerank_s]
    print("# measured by wirefold/bench `embed` (big, batch, mrl, iso); core-seconds per row per query = wall seconds x threads")
    print("format,bytes_per_row,c_lo,c_mid,c_hi,cb_lo,cb_mid,cb_hi,batch,recall_clustered,recall_isotropic,rerank")
    for name, v in big.items():
        stride, med, lo, hi = v[0], v[1], v[2], v[3]
        b = bat.get(name)
        bcells = ",,"
        # ROWB cells are (med, lo, hi) per size, so the Q=16 triple starts at index 1 + 3*2
        if b:
            med16, lo16, hi16 = b[1 + 3 * 2], b[1 + 3 * 2 + 1], b[1 + 3 * 2 + 2]
            bcells = ",".join(f"{x * 1e-9 * threads:.4e}" for x in (lo16, med16, hi16))
        rm = rec["mrl"].get(name); ri = rec["iso"].get(name)
        flag = 0 if name in ("f32", "f16") else 1
        r = lambda x: f"{x[3]:.4f}" if x else ""
        print(f"{name},{int(stride)},{lo * threads:.4e},{med * threads:.4e},{hi * threads:.4e},{bcells},{16 if b else 0},{r(rm)},{r(ri)},{flag}")

if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tables"); t.add_argument("prefix")
    c = sub.add_parser("csv"); c.add_argument("--dir", required=True)
    a = ap.parse_args()
    tables(a.prefix) if a.cmd == "tables" else csv_table(a.dir)
