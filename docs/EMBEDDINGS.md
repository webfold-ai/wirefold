# Embedding columns: storage, speed and cost

How a dense embedding column should be stored and read, measured on the apeiron primitives (a static partition of
the rows over `par::Pool`, each task owning a contiguous range and its own top-k, merged associatively) and priced
with `tcofold` (Ptyktos/ai-gateway), the existing rent-vs-own model, rather than a model of my own. Reproduce with
`bench/src/embed.rs`, `bench/scripts/embed_report.py` and `tcofold/examples/vector_demo.rs`.

**Everything below is measured on SYNTHETIC vectors** (1024 dimensions, 200,000 rows, one 4-core VM). No trained
embedding model, no real corpus. Sizes and throughputs do not depend on the data; recall does. `mrl-like` is
clustered with a decaying variance spectrum (what prefix truncation relies on); `isotropic` has no structure at all.
A real model lands somewhere between, and this benchmark cannot say where. The rates in the cost section are
SYNTHETIC placeholders from tcofold, and nothing is calibrated.

## Corrections to the first version of this report

The first version of this report was wrong in ways that mattered. What was wrong, found by checking it:

1. **The f32 baseline was handicapped.** The f32 scan was one serial floating-point add chain (`.sum::<f32>()`), which
   the compiler may not reorder, so it ran at the latency of a float add: one core scanned 4.6 GB/s where a plain
   read of the same memory runs at 12.9 GB/s. Sixteen independent accumulators in a fixed order fixed it (49 ms to 37 ms per query resident).
   Every "N times faster than f32" in version one was inflated by that.
2. **The packed kernels were not vectorised and the build targeted baseline x86-64.** f16 went through a scalar
   conversion, int4 and 2-bit through interleaved loops the compiler would not vectorise. They are now explicit AVX2
   kernels with runtime detection and a scalar fallback; every vector kernel is checked row by row against the scalar
   one (and the check was mutation-tested: breaking the int4 correction term or the 2-bit shift makes it fail).
   Resident scan per query: f16 107 to 16 ms, 2-bit 28 to 1.7 ms.
3. **Small columns were measured from cache.** The machine has a 260 MiB L3; at 200,000 rows every column but f32
   and f16 fit in it, so their "scan GB/s" (40 to 75) was cache speed, and the cost model scaled that to 10M and
   100M rows. All speed claims now come from columns of at least 2 GiB. In DRAM every format runs at 16 to 23 GB/s
   and time is proportional to bytes.
4. **Statistics were a median of three runs with min-max.** They now follow spbench's criterion port: twelve samples per
   measurement, percentile-bootstrap 95% intervals (10,000 resamples, fixed seed), Tukey fences, and for the
   L1-resident kernels the criterion linear-sampling slope (least squares through the origin over 20 growing
   iteration counts), which removes the fixed per-sample overhead. Each measurement is then run three times.
5. **The cost model was an ad-hoc Python script** in float dollars with invented prices. It is deleted. Costing is
   `tcofold::vector` in the ai-gateway repository ([Ptyktos/ai-gateway#19](https://github.com/Ptyktos/ai-gateway/pull/19)),
   built on `rent::ServiceFootprint` and tcofold's energy model, with the measured per-row core times and their
   intervals as inputs. Writing it found a bug of mine (own-node cost clamped at one node, so every 50-QPS row showed
   the same cost); my tests had missed it, so there is now a test for it and a test for the power term.
6. **The first int4 was a plain per-vector max scale on unrotated coordinates**, which is why its scan-only recall was
   0.41. The rotated variants (`int8 rot`, `int4 rot`) are added.
7. **"The f32 master is the only column worth compressing" was wrong at the granule a re-rank needs** (point 3 below).

## Method

| Part | What |
|---|---|
| Corpus | clustered (512 centroids, spectrum `(1+j)^-0.5`) and isotropic, 1024 dims, unit vectors, queries drawn from the same generator |
| Scan | exact top-10, static partition over `par::Pool`, per-task top-k, merge by (score descending, id ascending); the parallel answer equals the serial one (self-check, every format) |
| Re-rank | scan a coarse column, take 10x the shortlist, re-score at f32 |
| Kernels | AVX2 (runtime-detected) with scalar reference; f32 and f16 use 16 and 32 independent lanes summed in a fixed order |
| Timing | 12 samples, bootstrap 95% CI of the median, Tukey outliers; OLS-through-origin slope for L1 kernels; 3 runs |
| spbench | `embed one <format> <corpus>` prints `BENCH_QUALITY` (recall@10) and `BENCH_TOK_S` (rows scored per second), the marker contract in spbench's harness. spbench itself was not run here |
| Costing | `tcofold::vector` over `tcofold/data/embed-scan.csv` (`embed_report.py csv`) |

## The roofline

Raw sequential read of a 2 GiB buffer on this machine (median of 3 runs):

| read of 2 GiB | GB/s |
| --- | ---: |
| all 4 cores | 43.9 |
| one core | 12.9 |

Each kernel on a row that stays in L1 (compute-bound floor, slope estimator with 95% interval; the bracket spans the
run medians, so a wide one is a noisy run):

| format | B/vec | L1-resident ns/row [95% CI] | r^2 | GB/s of stored bytes (one core) |
| --- | ---: | ---: | ---: | ---: |
| f32 | 4096 | 193.2 [178.4-196.1] | 0.9783 | 21.2 |
| f16 | 2048 | 56.9 [54.4-59.4] | 0.9837 | 36.0 |
| int8 | 1028 | 28.2 [27.8-29.2] | 0.9907 | 36.5 |
| int8 rot | 1028 | 28.7 [28.0-48.5] | 0.9000 | 35.8 |
| int4 rot | 516 | 23.1 [22.9-39.1] | 0.9841 | 22.4 |
| 2-bit rot | 256 | 20.9 [20.1-35.6] | 0.9865 | 12.3 |
| binary rot | 128 | 9.4 [8.4-17.9] | 0.9837 | 13.6 |
| mrl256 int8 | 260 | 10.8 [9.7-20.7] | 0.9886 | 24.1 |
| mrl64 int8 | 68 | 5.8 [5.8-11.6] | 0.9913 | 11.7 |

Scan of a 2 GiB column of each format (one query at a time, all 4 cores; the interval is the bootstrap 95% CI of the
median of the best-agreeing run), against the 200,000-row column that sits in L3:

| format | B/vec | resident: 200k rows ms/query | resident GB/s | DRAM: 2 GiB column ms/query [95% CI] | DRAM GB/s | DRAM 1-core ns/row | rows/s/core (DRAM) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 | 4096 | 39.53 | 20.7 | 118.3 [118.2-119.0] | 20.8 | 715 | 1.4M |
| f16 | 2048 | 17.14 | 23.9 | 103.4 [100.0-104.0] | 23.8 | 282 | 3.5M |
| int8 | 1028 | 8.07 | 25.5 | 99.1 [98.0-102.2] | 22.8 | 145 | 6.9M |
| int8 rot | 1028 | 8.91 | 23.1 | 98.8 [92.9-103.2] | 22.9 | 141 | 7.1M |
| int4 rot | 516 | 2.20 | 46.9 | 104.6 [102.0-107.8] | 20.7 | 83 | 12.1M |
| 2-bit rot | 256 | 1.43 | 35.9 | 128.5 [126.2-130.2] | 16.7 | 52 | 19.4M |
| binary rot | 128 | 0.81 | 31.5 | 118.3 [116.0-124.1] | 18.2 | 24 | 41.7M |
| mrl256 int8 | 260 | 1.26 | 41.4 | 106.1 [100.7-108.7] | 20.6 | 42 | 24.0M |
| mrl64 int8 | 68 | 0.65 | 20.9 | 131.0 [128.6-132.8] | 16.4 | 14 | 72.3M |

**Reading it.** Every format lands at 16 to 23 GB/s in DRAM, against 43.9 GB/s for the raw read: the scan reaches
roughly 40 to 50% of the read bandwidth whatever the format, and one core manages about 7 GB/s of the 12.9 it can
read. I did not find out why (prefetch, TLB reach on 4 KiB pages and the top-k insertion are the candidates; none was
tested). Because the time is set by bytes, rows per second per core go as 1/bytes: 1.4M for f32 down to 72M for the 68-byte prefix.

## Storage, encode and decode

Clustered corpus (the isotropic one differs only in the compressibility columns, shown after it). Encode and decode
are all-core MB/s of f32; the two ratios are compressed size over stored size, a static tANS coder and zstd -3, on the
stored column (f32 and f16 byte-plane shuffled first).

| format | B/vec | vs f32 | encode MB/s (in) | decode MB/s (f32 out) | tANS ratio | zstd-3 ratio | tANS dec MB/s | zstd dec MB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 (byte planes) | 4096 | 1.0x | 6431 | 19768 | 0.925 | 0.859 | 475 | 970 |
| f16 (byte planes) | 2048 | 2.0x | 6526 | 21084 | 0.927 | 0.861 | 585 | 871 |
| int8 | 1028 | 4.0x | 4318 | 24359 | 0.638 | 0.656 | 804 | 576 |
| int8 rot | 1028 | 4.0x | 2867 | 23460 | 0.913 | 0.918 | 752 | 756 |
| int4 | 516 | 7.9x | 5680 | 22457 | 0.270 | 0.309 | 826 | 718 |
| int4 rot | 516 | 7.9x | 2720 | 22872 | 0.785 | 0.792 | 822 | 884 |
| 2-bit rot | 256 | 16.0x | 3946 | 26779 | 0.953 | 0.958 | 873 | 781 |
| binary rot | 128 | 32.0x | 4085 | 26954 | 1.003 | 1.000 | 841 | 5246 |
| mrl256 int8 | 260 | 15.8x | 13139 | n/a | 0.762 | 0.767 | 847 | 1040 |
| mrl128 int8 | 132 | 31.0x | 22491 | n/a | 0.824 | 0.828 | 875 | 875 |
| mrl64 int8 | 68 | 60.2x | 32448 | n/a | 0.882 | 0.881 | 869 | 758 |

Isotropic corpus:

| format | B/vec | vs f32 | encode MB/s (in) | decode MB/s (f32 out) | tANS ratio | zstd-3 ratio | tANS dec MB/s | zstd dec MB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 (byte planes) | 4096 | 1.0x | 6421 | 20631 | 0.923 | 0.857 | 486 | 973 |
| f16 (byte planes) | 2048 | 2.0x | 6517 | 21566 | 0.917 | 0.845 | 586 | 876 |
| int8 | 1028 | 4.0x | 4165 | 23961 | 0.912 | 0.916 | 746 | 732 |
| int8 rot | 1028 | 4.0x | 2842 | 24221 | 0.911 | 0.916 | 753 | 725 |
| int4 | 516 | 7.9x | 5554 | 23130 | 0.782 | 0.790 | 811 | 918 |
| int4 rot | 516 | 7.9x | 2682 | 23070 | 0.782 | 0.789 | 756 | 817 |
| 2-bit rot | 256 | 16.0x | 3820 | 27213 | 0.954 | 0.958 | 866 | 786 |
| binary rot | 128 | 32.0x | 4036 | 26247 | 1.005 | 1.000 | 849 | 5206 |
| mrl256 int8 | 260 | 15.8x | 13223 | n/a | 0.935 | 0.936 | 770 | 762 |
| mrl128 int8 | 132 | 31.0x | 22201 | n/a | 0.947 | 0.946 | 594 | 764 |
| mrl64 int8 | 68 | 60.2x | 33349 | n/a | 0.957 | 0.957 | 865 | 779 |

## Scan, recall and re-rank (200,000 rows: cache-resident, so use the DRAM table above for speed)

Clustered corpus:

| format | scan ms (all cores) | scan GB/s | 1-core ns/row | recall@10, scan only | recall@10, scan + f32 re-rank x10 | scan + re-rank ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 | 37.46 [37.45-38.78] | 21.9 | 689 | 1.000 | 1.000 | 37.35 |
| f16 | 16.30 [15.02-17.31] | 25.1 | 279 | 1.000 | 1.000 | 16.31 |
| int8 | 7.92 [7.47-9.02] | 26.0 | 44 | 0.962 | 1.000 | 8.13 |
| int8 rot | 7.68 [6.73-7.79] | 26.8 | 45 | 0.992 | 1.000 | 8.43 |
| int4 | 1.60 [1.52-1.77] | 64.4 | 27 | 0.405 | 0.910 | 2.17 |
| int4 rot | 1.77 [1.47-2.23] | 58.4 | 28 | 0.870 | 1.000 | 2.26 |
| 2-bit rot | 1.72 [1.29-1.76] | 29.7 | 22 | 0.702 | 1.000 | 1.82 |
| binary rot | 0.46 [0.45-0.77] | 55.3 | 8 | 0.495 | 1.000 | 0.64 |
| mrl256 int8 | 0.77 [0.70-0.80] | 67.4 | 13 | 0.887 | 1.000 | 1.05 |
| mrl128 int8 | 0.46 [0.46-0.56] | 57.2 | 8 | 0.815 | 1.000 | 0.77 |
| mrl64 int8 | 0.42 [0.36-0.68] | 32.1 | 7 | 0.752 | 1.000 | 0.57 |

Isotropic corpus:

| format | scan ms (all cores) | scan GB/s | 1-core ns/row | recall@10, scan only | recall@10, scan + f32 re-rank x10 | scan + re-rank ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 | 38.78 [37.05-38.79] | 21.1 | 676 | 1.000 | 1.000 | 37.84 |
| f16 | 16.19 [15.11-16.36] | 25.3 | 273 | 1.000 | 1.000 | 16.02 |
| int8 | 6.36 [5.21-8.68] | 32.3 | 45 | 0.960 | 1.000 | 7.65 |
| int8 rot | 7.36 [6.66-8.09] | 27.9 | 47 | 0.967 | 1.000 | 7.79 |
| int4 | 1.79 [1.50-1.98] | 57.5 | 27 | 0.745 | 1.000 | 2.29 |
| int4 rot | 2.31 [1.49-2.69] | 44.6 | 34 | 0.767 | 1.000 | 1.95 |
| 2-bit rot | 1.57 [1.19-1.66] | 32.6 | 23 | 0.455 | 0.930 | 1.69 |
| binary rot | 0.64 [0.51-0.68] | 40.2 | 9 | 0.065 | 0.225 | 0.80 |
| mrl256 int8 | 1.11 [0.72-1.13] | 46.7 | 13 | 0.015 | 0.090 | 1.13 |
| mrl128 int8 | 0.53 [0.52-0.93] | 50.2 | 9 | 0.005 | 0.028 | 0.76 |
| mrl64 int8 | 0.39 [0.34-0.71] | 34.6 | 6 | 0.003 | 0.008 | 0.57 |

## The six points, pushed further

**1. Do not compress the scanned column.** A DRAM scan reads stored bytes at 16 to 23 GB/s. The coders decode at
0.5 to 0.9 GB/s (tANS) and 0.55 to 1.0 GB/s (zstd), so decompress-then-scan is about 20 to 50 times slower than
scanning the codes as stored, and the 2-bit and binary columns do not compress at all (0.95 to 1.00). Store the hot
column uncompressed and fixed-width, so a row range is a byte range.

**2. Compressibility is a signal that bits are being wasted.** Unrotated int8 and int4 shrink to 0.64 and 0.27 on the
clustered corpus because a decaying spectrum leaves most codes near zero. After the rotation the same formats compress
to the same ratio on both corpora (int8 rot 0.91, int4 rot 0.78): the rotation makes the coordinates statistically
alike whatever the data, which is what a data-independent codebook needs. int4 rot still compresses 22%, because a
uniform 4-bit quantiser on a Gaussian does not use all of its entropy; a better codebook would reclaim that, an
entropy coder at 0.8 GB/s would not pay for itself.

**3. The f32 master, at the granule a re-rank reads.** A re-rank touches about 100 individual rows. Compressing a
row on its own does not work, and larger granules make re-rank decode slow:

| master | granule | tANS ratio | tANS decode us/granule | zstd-3 ratio | zstd decode us/granule | 100-row re-rank decode ms (tANS) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| f32 | 1 row | 1.058 | 9.5 | 0.940 | 21.4 | 0.95 |
| f32 | 16 rows | 0.933 | 85.8 | 0.923 | 96.7 | 8.58 |
| f32 | 256 rows | 0.926 | 1225.8 | 0.857 | 532.6 | 122.58 |
| f16 | 1 row | 1.185 | 4.3 | 0.960 | 18.8 | 0.43 |
| f16 | 16 rows | 0.943 | 47.3 | 0.930 | 57.5 | 4.73 |
| f16 | 256 rows | 0.928 | 608.7 | 0.861 | 291.4 | 60.87 |

At one row, tANS expands the data (1.06 for f32, 1.19 for f16: the per-granule table costs more than it saves) and
zstd saves 6%. At 256 rows both save 7 to 14%, and a 100-row re-rank then decodes 100 granules: 123 ms with tANS,
against a few milliseconds for the whole scan. **Keep the master uncompressed.** What does help is f16 for the master:
recall after re-rank is 1.00 on both corpora with an f16 scan alone, it halves the bytes with no decode at all, and
the half-to-float conversion is hardware (57 ns per row in L1). It is lossy (about 5e-4 relative), which this
benchmark shows does not change the ranking here and a real corpus may not agree.

**4. Prefix truncation: structure matters more than the spectrum.** int8 prefix of d dimensions, scan alone and then
with the 10x f32 re-rank, as the amplitude exponent of the spectrum (the share of energy in the prefix) varies, on
the clustered generator (100,000 rows):

| amplitude exponent | energy in first 64 / 256 dims | prefix 16: scan / +rerank x10 | prefix 32: scan / +rerank x10 | prefix 64: scan / +rerank x10 | prefix 128: scan / +rerank x10 | prefix 256: scan / +rerank x10 | prefix 512: scan / +rerank x10 |
|---|---|---:|---:|---:|---:|---:|---:|
| 0 | 6% / 25% | 0.05 / 0.31 | 0.09 / 0.62 | 0.11 / 0.75 | 0.17 / 0.79 | 0.23 / 0.87 | 0.37 / 0.98 |
| 0.25 | 23% / 49% | 0.17 / 0.64 | 0.35 / 0.93 | 0.42 / 0.98 | 0.50 / 1.00 | 0.60 / 1.00 | 0.71 / 1.00 |
| 0.5 | 63% / 82% | 0.35 / 0.83 | 0.65 / 0.99 | 0.79 / 1.00 | 0.85 / 1.00 | 0.91 / 1.00 | 0.95 / 1.00 |
| 0.75 | 93% / 98% | 0.44 / 0.86 | 0.69 / 0.96 | 0.87 / 1.00 | 0.94 / 1.00 | 0.96 / 1.00 | 0.98 / 1.00 |
| 1 | 99% / 100% | 0.57 / 0.98 | 0.78 / 1.00 | 0.89 / 1.00 | 0.93 / 1.00 | 0.94 / 1.00 | 0.95 / 1.00 |
| 1.5 | 100% / 100% | 0.78 / 1.00 | 0.83 / 1.00 | 0.84 / 1.00 | 0.84 / 1.00 | 0.84 / 1.00 | 0.84 / 1.00 |

With the re-rank, a 64-dimension prefix recovers 0.98 of the exact top 10 once about a quarter of the energy is in it
(exponent 0.25) and 1.00 from exponent 0.5. At a flat spectrum (exponent 0) it is still 0.75, and on the isotropic
corpus, where the prefix carries no cluster structure at all, it is 0.01. So what the prefix needs is cluster
structure visible in its first coordinates, not just a steep spectrum. A Matryoshka-trained model is built to put it
there; whether yours does is a measurement on your data, and this one cannot make it. The scan-only recall is the
harsher figure: 0.11 to 0.89 at 64 dimensions.

**5. Quantise the whole vector when the model is not Matryoshka.** On the isotropic corpus int8 rot and int4 rot with
the re-rank both reach recall 1.00; binary (0.23) and 2-bit (0.93) do not. int4 rot reads half the bytes of int8:
12.1M against 6.9M rows per second per core in DRAM.

**6. Cost is cores, and batching is the biggest lever on them.** Queries per pass over the DRAM column, ns per row
per query (all 4 cores, 95% CIs in the per-run files):

| format | 1 q/pass: ns/row/query | 4 q/pass: ns/row/query | 16 q/pass: ns/row/query | 64 q/pass: ns/row/query |
| --- | ---: | ---: | ---: | ---: |
| f32 | 190.29 | 90.78 | 59.69 | 51.61 |
| f16 | 84.08 | 33.69 | 23.46 | 18.85 |
| int8 | 45.82 | 18.51 | 12.14 | 10.15 |
| int8 rot | 44.67 | 21.31 | 11.96 | 10.59 |
| int4 rot | 25.51 | 14.28 | 9.08 | 7.83 |
| 2-bit rot | 14.61 | 8.50 | 7.18 | 6.35 |
| binary rot | 7.60 | 4.20 | 3.05 | 2.76 |
| mrl256 int8 | 11.62 | 5.70 | 4.33 | 3.75 |
| mrl64 int8 | 4.02 | 2.41 | 2.03 | 1.84 |

Scoring 16 queries against each block while it is in L2 cuts the per-query cost 3.2 times for f32 and 3.8 times for
int8, because the column crosses the memory bus once for all of them. Priced by `tcofold::vector` (rent is the
cheapest of BinaryLane, DigitalOcean, AWS, Google, Azure under tcofold's synthetic rate cards; own is a share of a
16-vCore, 256 GB node; the band is the 95% interval of the measured core time; `*` marks recall under 0.95 on a
corpus, so that price buys a result your model may not give you):

```
═══ embedding scan: rent-vs-own per storage format (A$/month, SYNTHETIC rates, uncalibrated) ═══
measured table: /tmp/tcotest/tcofold/data/embed-scan.csv (9 formats)

── 10M rows, 1 QPS, one query per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8         0.3       0.7  1.00/0.01*         17 [   17-    17] Bin          2 [    2-     2]        own
  binary rot         0.5       1.3  1.00/0.23*         19 [   19-    19] Bin          4 [    4-     4]        own
  mrl256 int8        0.9       2.6  1.00/0.09*         23 [   23-    23] Bin          7 [    7-     7]        own
  2-bit rot          1.0       2.6  1.00/0.93*         24 [   24-    24] Bin          9 [    9-     9]        own
  int4 rot           1.7       5.2  1.00/1.00          31 [   31-    31] Bin         14 [   14-    14]        own
  int8               3.0      10.3  1.00/1.00          46 [   46-    47] Bin         26 [   25-    27]        own
  int8 rot           3.0      10.3  1.00/1.00          46 [   45-    47] Bin         26 [   24-    26]        own
  f16                5.7      20.5  1.00/1.00          71 [   69-    72] Bin         50 [   48-    51]        own
  f32               13.1      41.0  1.00/1.00         145 [  143-   147] Bin        113 [  111-   116]        own

── 10M rows, 1 QPS, 16 queries per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8         0.1       0.7  1.00/0.01*         16 [   16-    16] Bin          1 [    1-     1]        own
  binary rot         0.2       1.3  1.00/0.23*         17 [   17-    17] Bin          2 [    2-     2]        own
  mrl256 int8        0.3       2.6  1.00/0.09*         19 [   19-    19] Bin          2 [    2-     3]        own
  2-bit rot          0.5       2.6  1.00/0.93*         20 [   20-    21] Bin          4 [    4-     4]        own
  int4 rot           0.6       5.2  1.00/1.00          24 [   23-    24] Bin          5 [    4-     6]        own
  int8 rot           0.8      10.3  1.00/1.00          31 [   31-    32] Bin          7 [    6-     8]        own
  int8               0.8      10.3  1.00/1.00          31 [   31-    31] Bin          7 [    7-     7]        own
  f16                1.6      20.5  1.00/1.00          41 [   41-    43] Bin         14 [   12-    15]        own
  f32                4.0      41.0  1.00/1.00          81 [   80-    83] Bin         34 [   34-    37]        own

── 10M rows, 50 QPS, one query per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8        13.3       0.7  1.00/0.01*        108 [  108-   108] Bin        115 [  115-   115]       rent
  binary rot        23.3       1.3  1.00/0.23*        179 [  179-   179] Bin        202 [  202-   202]       rent
  mrl256 int8       43.3       2.6  1.00/0.09*        320 [  297-   320] Bin        374 [  345-   374]       rent
  2-bit rot         50.0       2.6  1.00/0.93*        367 [  367-   390] Bin        432 [  432-   461]       rent
  int4 rot          83.3       5.2  1.00/1.00         603 [  580-   603] Bin        720 [  691-   720]       rent
  int8             150.0      10.3  1.00/1.00        1075 [ 1029-  1122] Bin       1295 [ 1238-  1353]       rent
  int8 rot         150.0      10.3  1.00/1.00        1075 [ 1005-  1099] Bin       1295 [ 1209-  1324]       rent
  f16              286.7      20.5  1.00/1.00        2037 [ 1967-  2084] Bin       2476 [ 2389-  2533]       rent
  f32              656.7      41.0  1.00/1.00        4650 [ 4556-  4743] Bin       5671 [ 5556-  5786]       rent

── 10M rows, 50 QPS, 16 queries per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8         6.8       0.7  1.00/0.01*         62 [   59-    66] Bin         58 [   54-    63]        own
  binary rot        10.2       1.3  1.00/0.23*         87 [   83-    90] Bin         88 [   84-    92]       rent
  mrl256 int8       14.4       2.6  1.00/0.09*        118 [  112-   130] Bin        125 [  118-   140]       rent
  2-bit rot         23.9       2.6  1.00/0.93*        185 [  179-   198] Bin        207 [  200-   223]       rent
  int4 rot          30.3       5.2  1.00/1.00         232 [  198-   254] Bin        261 [  220-   289]       rent
  int8 rot          39.9      10.3  1.00/1.00         304 [  284-   336] Bin        344 [  319-   384]       rent
  int8              40.5      10.3  1.00/1.00         309 [  294-   322] Bin        349 [  332-   365]       rent
  f16               78.2      20.5  1.00/1.00         578 [  536-   640] Bin        675 [  624-   752]       rent
  f32              199.0      41.0  1.00/1.00        1446 [ 1418-  1535] Bin       1718 [ 1684-  1828]       rent

── 100M rows, 1 QPS, one query per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8         2.7       6.8  1.00/0.01*         93 [   93-    93] Dig         23 [   23-    23]        own
  binary rot         4.7      12.8  1.00/0.23*        116 [  116-   116] Bin         40 [   40-    40]        own
  mrl256 int8        8.7      26.0  1.00/0.09*        159 [  154-   159] Bin         75 [   69-    75]        own
  2-bit rot         10.0      25.6  1.00/0.93*        168 [  168-   172] Bin         86 [   86-    92]        own
  int4 rot          16.7      51.6  1.00/1.00         243 [  238-   243] Bin        144 [  138-   144]        own
  int8              30.0     102.8  1.00/1.00         393 [  383-   402] Bin        259 [  248-   271]        own
  int8 rot          30.0     102.8  1.00/1.00         393 [  379-   397] Bin        259 [  242-   265]        own
  f16               57.3     204.8  1.00/1.00         635 [  621-   644] Bin        495 [  478-   507]        own
  f32              131.3     409.6  1.00/1.00        1378 [ 1359-  1397] Bin       1134 [ 1111-  1157]        own

── 100M rows, 1 QPS, 16 queries per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8         1.4       6.8  1.00/0.01*         81 [   80-    82] Dig         12 [   11-    13]        own
  binary rot         2.0      12.8  1.00/0.23*         95 [   95-    96] Dig         18 [   17-    18]        own
  mrl256 int8        2.9      26.0  1.00/0.09*        118 [  117-   121] Bin         25 [   24-    28]        own
  2-bit rot          4.8      25.6  1.00/0.93*        131 [  130-   134] Bin         41 [   40-    45]        own
  int4 rot           6.1      51.6  1.00/1.00         169 [  162-   173] Bin         52 [   44-    58]        own
  int8 rot           8.0     102.8  1.00/1.00         238 [  234-   245] Bin         69 [   64-    77]        own
  int8               8.1     102.8  1.00/1.00         239 [  236-   242] Bin         70 [   66-    73]        own
  f16               15.6     204.8  1.00/1.00         343 [  334-   355] Bin        135 [  125-   150]        own
  f32               39.8     409.6  1.00/1.00         737 [  732-   755] Bin        344 [  337-   366]        own

── 100M rows, 50 QPS, one query per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8       133.3       6.8  1.00/0.01*       1010 [ 1010-  1010] Bin       1151 [ 1151-  1151]       rent
  binary rot       233.3      12.8  1.00/0.23*       1717 [ 1717-  1717] Bin       2015 [ 2015-  2015]       rent
  mrl256 int8      433.3      26.0  1.00/0.09*       3131 [ 2898-  3131] Bin       3742 [ 3454-  3742]       rent
  2-bit rot        500.0      25.6  1.00/0.93*       3598 [ 3598-  3831] Bin       4318 [ 4318-  4606]       rent
  int4 rot         833.3      51.6  1.00/1.00        5960 [ 5726-  5960] Bin       7197 [ 6909-  7197]       rent
  int8            1500.0     102.8  1.00/1.00       10683 [10216- 11149] Bin      12954 [12378- 13530]       rent
  int8 rot        1500.0     102.8  1.00/1.00       10683 [ 9983- 10916] Bin      12954 [12090- 13242]       rent
  f16             2866.7     204.8  1.00/1.00       20300 [19600- 20767] Bin      24756 [23893- 25332]       rent
  f32             6566.7     409.6  1.00/1.00       46425 [45492- 47359] Bin      56709 [55557- 57860]       rent

── 100M rows, 50 QPS, 16 queries per pass ──
  format          vCores    RAM GB  recall c/i   rent (best provider) band         own-node share band    cheaper
  mrl64 int8        67.6       6.8  1.00/0.01*        550 [  517-   589] Bin        584 [  543-   631]       rent
  binary rot       101.6      12.8  1.00/0.23*        795 [  762-   831] Bin        878 [  837-   923]       rent
  mrl256 int8      144.3      26.0  1.00/0.09*       1108 [ 1053-  1229] Bin       1247 [ 1178-  1395]       rent
  2-bit rot        239.5      25.6  1.00/0.93*       1774 [ 1717-  1907] Bin       2068 [ 1998-  2232]       rent
  int4 rot         302.7      51.6  1.00/1.00        2245 [ 1911-  2466] Bin       2614 [ 2201-  2886]       rent
  int8 rot         398.5     102.8  1.00/1.00        2972 [ 2768-  3291] Bin       3442 [ 3190-  3835]       rent
  int8             404.6     102.8  1.00/1.00        3015 [ 2871-  3143] Bin       3494 [ 3316-  3653]       rent
  f16              782.0     204.8  1.00/1.00        5707 [ 5291-  6330] Bin       6753 [ 6239-  7521]       rent
  f32             1989.8     409.6  1.00/1.00       14387 [14111- 15277] Bin      17183 [16844- 18281]       rent

recall c/i = recall@10 after the f32 re-rank on the clustered / isotropic synthetic corpus; * = under 0.95 on one of them,
so its price is for a result a real model may not give you. Formats never measured on both corpora are left out.
```

What that shows: at 1 QPS owning is cheaper at every size; at 50 QPS the cheaper side flips to renting, because the
synthetic node costs about A$6.5 per vCore-month against BinaryLane's A$7 plus its RAM charge. At 10M rows and 1
QPS the cheapest format that holds recall on both corpora is int4 rot (A$7 per month on an owned node batched, A$24
rented) against A$34 / A$81 for f32. At 100M rows and 50 QPS an exact scan needs hundreds to thousands of vCores
for any format that holds recall, which is where an index stops being optional; this report does not measure one.

## How bad our implementation still is

Named, since I was asked to:

- **I never ran sangdb's own code.** I read `sangdb::vector`, `mrl` and `quant` (the TurboQuant column, the two-stage
  read, the dispatch) and benchmarked my own kernels on the same ideas. Whether sangdb's kernels are faster or
  slower than these is unknown.
- **The scan reaches about half the raw read bandwidth and I do not know why.**
- **int8 is not using VNNI.** The machine has AVX-512 VNNI; the int8 kernel is AVX2 `maddubs`. There is headroom I have not taken.
- **The 2-bit and binary columns do not buy speed per byte.** In L1 the 2-bit kernel is 21 ns per row against 28 for
  int8, for a quarter of the bytes. In DRAM the byte saving is real, but their recall is the problem on the isotropic corpus.
- **Everything is synthetic.** No real model, no real corpus, one machine, four threads. The core time per row is
  assumed to scale linearly to many machines; that follows from the static partition but was not measured past 4 cores.
- **No index.** `lsh` and `graph_vec` exist in sangdb and are not measured; the cost model is an exact scan only.
- **Re-rank IO is not priced** (100 random 4 KiB reads per query from SSD).
- **Nothing is calibrated.** tcofold's rule is no refit before 8 paid or metered outcomes (`calibration::MIN_SAMPLE`); there are none.
- **Insert, update, delete and the persistence format are not measured.** Neither are filtered queries.
- **No embedding column type exists in wirefold's library,** and none of this lives in sangdb: it is a benchmark and a costing module.
- **spbench was not run.** The `BENCH_QUALITY` and `BENCH_TOK_S` markers follow its harness document; I did not build spbench to check they parse.
- **The tcofold tests ran on a copy with its own workspace,** not through ai-gateway's workspace, which needs private git dependencies.

## Reproduce

```bash
cd bench && cargo build --release --bin embed
./target/release/embed bw        # raw read bandwidth
./target/release/embed kernel    # L1-resident kernels
./target/release/embed big       # DRAM-resident scan, 2 GiB columns
./target/release/embed batch     # queries per pass
./target/release/embed sweep     # spectrum sweep
./target/release/embed cold      # the master at row, 16-row and 256-row granules
./target/release/embed 200000 mrl 40   # storage, encode/decode, recall (also: iso)
./target/release/embed one int8 mrl    # spbench markers
python3 scripts/embed_report.py tables <prefix>   # merge runs <prefix>.N.txt
python3 scripts/embed_report.py csv --dir <dir>   # the table tcofold reads
```
