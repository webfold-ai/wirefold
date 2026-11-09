# Embedding columns: storage, speed and cost

How a dense embedding column should be stored and read, measured with the apeiron primitives (static
partition of the rows over `par::Pool`, associative top-k merge) and priced with a cost model that follows
from them. Reproduce with `bench/src/embed.rs` and `bench/scripts/embed_report.py`.

**Read this first.** Everything below is measured on SYNTHETIC vectors (1024 dimensions, 200,000 rows, one
4-core machine). No trained embedding model, no real corpus. Sizes and throughputs do not depend on the data;
recall does. The two corpora bracket the answer: `mrl-like` has clusters and a decaying variance spectrum (what
prefix truncation relies on), `isotropic` has no structure at all. A real model lands somewhere between; the
recall columns say how each format behaves at the two ends, not what it will do on your data. Prices in the
cost model are inputs, not measurements.

## What sangdb already has (from its source, not from running it)

`sangdb::vector` keeps an f32 master column (`VectorPoint`) and an optional packed column (`QuantIndex`,
TurboQuant: one shared orthogonal rotation, then a fixed Lloyd-Max codebook, 2 or 4 bits per coordinate, a
fixed-width row so a row range is a byte range). `sangdb::mrl` adds a second packed column over a re-normalised
prefix and a two-stage read (coarse scan, f32 re-rank of a shortlist). `lsh` and `graph_vec` are the
approximate paths, `rrf` fuses a dense and a posting list by rank. Persistence goes through the wire codec this
repository came from. webfold itself has no vector index yet: it calls an external embeddings endpoint and
scans the vectors linearly (`docs/ENGINE-RECONCILE.md` in webfold, step 2). Not read: iotadb's reports,
lorelake's manifest (the seeds README says it is not tracked in the checkout) and memoryfold (its README
would not download), so nothing here is about their on-disk formats.

## Why the apeiron basis gives the cost model

- A scan is a fold over rows. Each task owns a contiguous range and its own top-k; the lists merge by
  (score descending, id ascending), which is associative and commutative. The self-check in `embed.rs` asserts
  the parallel answer equals the serial one for every format. So a shard costs what its rows cost, shards add,
  and there is no coordination term: cost is linear in rows.
- Fixed-width codes make a row range a byte range, so the scan is bytes over bandwidth and the per-row time
  is measured once and scaled.
- A data-independent codebook (the rotation) means an insert touches only its own row and two columns built
  under the same seed concatenate. That is what keeps the packed column cheap to maintain; nothing here
  measures insert cost.

The cost model is therefore: monthly cost = RAM for the scanned column + SSD for the f32 master + cores sized
for the query rate, with cores = QPS x core-seconds per query / utilisation, and core-seconds per query =
measured all-core latency x threads x (N / measured N). It models an exact scan only. An ANN index changes the
rows-touched term, and that is not measured here.

## Results, mrl-like corpus


### storage, encode and decode (819 MB of f32 in)

| format | B/vec | vs f32 | encode MB/s (in) | decode MB/s (f32 out) | tANS ratio | zstd-3 ratio | tANS dec MB/s | zstd dec MB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 (byte planes) | 4096 | 1.0x | 6536 | 19277 | 0.925 | 0.859 | 478 | 920 |
| f16 (byte planes) | 2048 | 2.0x | 6416 | 21402 | 0.927 | 0.861 | 595 | 875 |
| int8 | 1028 | 4.0x | 4191 | 24375 | 0.638 | 0.656 | 622 | 549 |
| int4 | 516 | 7.9x | 5572 | 23410 | 0.270 | 0.309 | 735 | 691 |
| 2-bit rot | 256 | 16.0x | 3390 | 26484 | 0.959 | 0.963 | 819 | 760 |
| binary rot | 128 | 32.0x | 4105 | 26629 | 1.003 | 1.000 | 843 | 4629 |
| mrl256 int8 | 260 | 15.8x | 12922 | n/a | 0.762 | 0.767 | 569 | 978 |
| mrl128 int8 | 132 | 31.0x | 21874 | n/a | 0.824 | 0.828 | 848 | 837 |
| mrl64 int8 | 68 | 60.2x | 32938 | n/a | 0.882 | 0.881 | 838 | 703 |

### scan: exact top-10 over 200000 vectors, per query (median of 3 passes of 40 queries; range in brackets)

| format | scan ms (all cores) | scan GB/s | 1-core ns/row | recall@10, scan only | recall@10, scan + f32 re-rank x10 | scan + re-rank ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 | 49.30 [49.23-50.16] | 16.6 | 904 | 1.000 | 1.000 | 48.58 |
| f16 | 106.97 [102.12-118.18] | 3.8 | 1782 | 1.000 | 1.000 | 105.37 |
| int8 | 10.43 [9.02-10.53] | 19.7 | 162 | 0.962 | 1.000 | 10.55 |
| int4 | 7.03 [6.74-7.10] | 14.7 | 115 | 0.405 | 0.910 | 7.56 |
| 2-bit rot | 29.55 [24.90-33.84] | 1.7 | 461 | 0.703 | 1.000 | 30.59 |
| binary rot | 0.54 [0.42-0.79] | 47.4 | 8 | 0.495 | 1.000 | 0.74 |
| mrl256 int8 | 1.66 [1.20-1.86] | 31.4 | 20 | 0.887 | 1.000 | 1.94 |
| mrl128 int8 | 0.79 [0.77-1.01] | 33.5 | 12 | 0.815 | 1.000 | 0.88 |
| mrl64 int8 | 0.72 [0.69-0.98] | 18.8 | 8 | 0.752 | 1.000 | 0.62 |


_3 runs, median per cell; bracketed ranges are min-max over the runs._


### cost model (inputs: RAM $3.0/GB-mo, SSD $0.1/GB-mo, vCPU $25.0/mo at 60% utilisation; scan cost scaled linearly from the measured n=200000)

Rows scan one column in RAM; rows marked `+rerank` also read 100 f32 master vectors per query from SSD (their IO is not priced).

| N | QPS | format | recall@10 | RAM $ | SSD $ | CPU $ | total $/month | cores |
|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 10M | 1 | mrl64 int8 +rerank | 1.00 | 2 | 4 | 5 | 11 | 0.2 |
| 10M | 1 | binary rot +rerank | 1.00 | 4 | 4 | 6 | 14 | 0.2 |
| 10M | 1 | mrl128 int8 +rerank | 1.00 | 4 | 4 | 7 | 15 | 0.3 |
| 10M | 1 | mrl256 int8 +rerank | 1.00 | 8 | 4 | 16 | 28 | 0.6 |
| 10M | 1 | int4 +rerank (recall < 0.95) | 0.91 | 15 | 4 | 63 | 83 | 2.5 |
| 10M | 1 | int8 +rerank | 1.00 | 31 | 4 | 88 | 123 | 3.5 |
| 10M | 1 | 2-bit rot +rerank | 1.00 | 8 | 4 | 255 | 267 | 10.2 |
| 10M | 1 | f32 | 1.00 | 123 | 0 | 411 | 534 | 16.4 |
| 10M | 1 | f16 | 1.00 | 61 | 4 | 891 | 957 | 35.7 |
| 10M | 50 | mrl64 int8 +rerank | 1.00 | 2 | 4 | 260 | 266 | 10.4 |
| 10M | 50 | binary rot +rerank | 1.00 | 4 | 4 | 310 | 318 | 12.4 |
| 10M | 50 | mrl128 int8 +rerank | 1.00 | 4 | 4 | 367 | 375 | 14.7 |
| 10M | 50 | mrl256 int8 +rerank | 1.00 | 8 | 4 | 807 | 819 | 32.3 |
| 10M | 50 | int4 +rerank (recall < 0.95) | 0.91 | 15 | 4 | 3,150 | 3,170 | 126.0 |
| 10M | 50 | int8 +rerank | 1.00 | 31 | 4 | 4,395 | 4,430 | 175.8 |
| 10M | 50 | 2-bit rot +rerank | 1.00 | 8 | 4 | 12,747 | 12,758 | 509.9 |
| 10M | 50 | f32 | 1.00 | 123 | 0 | 20,543 | 20,666 | 821.7 |
| 10M | 50 | f16 | 1.00 | 61 | 4 | 44,569 | 44,635 | 1,782.8 |
| 100M | 1 | mrl64 int8 +rerank | 1.00 | 20 | 41 | 52 | 113 | 2.1 |
| 100M | 1 | binary rot +rerank | 1.00 | 38 | 41 | 62 | 141 | 2.5 |
| 100M | 1 | mrl128 int8 +rerank | 1.00 | 40 | 41 | 73 | 154 | 2.9 |
| 100M | 1 | mrl256 int8 +rerank | 1.00 | 78 | 41 | 161 | 280 | 6.5 |
| 100M | 1 | int4 +rerank (recall < 0.95) | 0.91 | 155 | 41 | 630 | 826 | 25.2 |
| 100M | 1 | int8 +rerank | 1.00 | 308 | 41 | 879 | 1,228 | 35.2 |
| 100M | 1 | 2-bit rot +rerank | 1.00 | 77 | 41 | 2,549 | 2,667 | 102.0 |
| 100M | 1 | f32 | 1.00 | 1,229 | 0 | 4,109 | 5,337 | 164.3 |
| 100M | 1 | f16 | 1.00 | 614 | 41 | 8,914 | 9,569 | 356.6 |
| 100M | 50 | mrl64 int8 +rerank | 1.00 | 20 | 41 | 2,596 | 2,657 | 103.8 |
| 100M | 50 | binary rot +rerank | 1.00 | 38 | 41 | 3,096 | 3,175 | 123.8 |
| 100M | 50 | mrl128 int8 +rerank | 1.00 | 40 | 41 | 3,671 | 3,751 | 146.8 |
| 100M | 50 | mrl256 int8 +rerank | 1.00 | 78 | 41 | 8,067 | 8,186 | 322.7 |
| 100M | 50 | int4 +rerank (recall < 0.95) | 0.91 | 155 | 41 | 31,500 | 31,696 | 1,260.0 |
| 100M | 50 | int8 +rerank | 1.00 | 308 | 41 | 43,946 | 44,295 | 1,757.8 |
| 100M | 50 | 2-bit rot +rerank | 1.00 | 77 | 41 | 127,467 | 127,584 | 5,098.7 |
| 100M | 50 | f32 | 1.00 | 1,229 | 0 | 205,433 | 206,662 | 8,217.3 |
| 100M | 50 | f16 | 1.00 | 614 | 41 | 445,692 | 446,347 | 17,827.7 |

## Results, isotropic corpus (the hard case)


### storage, encode and decode (819 MB of f32 in)

| format | B/vec | vs f32 | encode MB/s (in) | decode MB/s (f32 out) | tANS ratio | zstd-3 ratio | tANS dec MB/s | zstd dec MB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 (byte planes) | 4096 | 1.0x | 6208 | 20630 | 0.923 | 0.857 | 481 | 901 |
| f16 (byte planes) | 2048 | 2.0x | 6069 | 19763 | 0.917 | 0.845 | 594 | 888 |
| int8 | 1028 | 4.0x | 3986 | 22583 | 0.912 | 0.916 | 691 | 723 |
| int4 | 516 | 7.9x | 5584 | 22433 | 0.782 | 0.790 | 794 | 889 |
| 2-bit rot | 256 | 16.0x | 3657 | 27427 | 0.959 | 0.964 | 817 | 728 |
| binary rot | 128 | 32.0x | 4158 | 27290 | 1.005 | 1.000 | 829 | 4556 |
| mrl256 int8 | 260 | 15.8x | 13769 | n/a | 0.935 | 0.936 | 809 | 780 |
| mrl128 int8 | 132 | 31.0x | 23232 | n/a | 0.947 | 0.946 | 855 | 720 |
| mrl64 int8 | 68 | 60.2x | 32288 | n/a | 0.957 | 0.957 | 608 | 690 |

### scan: exact top-10 over 200000 vectors, per query (median of 3 passes of 40 queries; range in brackets)

| format | scan ms (all cores) | scan GB/s | 1-core ns/row | recall@10, scan only | recall@10, scan + f32 re-rank x10 | scan + re-rank ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| f32 | 52.07 [50.45-52.69] | 15.7 | 939 | 1.000 | 1.000 | 51.39 |
| f16 | 114.06 [110.81-119.21] | 3.6 | 1714 | 1.000 | 1.000 | 121.19 |
| int8 | 10.36 [10.01-11.72] | 19.8 | 110 | 0.960 | 1.000 | 10.90 |
| int4 | 8.04 [7.39-8.55] | 12.8 | 122 | 0.745 | 1.000 | 7.95 |
| 2-bit rot | 38.36 [36.46-38.44] | 1.3 | 466 | 0.457 | 0.922 | 32.17 |
| binary rot | 0.68 [0.45-1.39] | 37.7 | 9 | 0.065 | 0.225 | 0.77 |
| mrl256 int8 | 1.10 [1.10-1.52] | 47.1 | 20 | 0.015 | 0.090 | 1.67 |
| mrl128 int8 | 0.81 [0.80-0.95] | 32.6 | 13 | 0.005 | 0.028 | 1.20 |
| mrl64 int8 | 0.70 [0.56-0.82] | 19.5 | 13 | 0.003 | 0.008 | 0.80 |


_3 runs, median per cell; bracketed ranges are min-max over the runs._


### cost model (inputs: RAM $3.0/GB-mo, SSD $0.1/GB-mo, vCPU $25.0/mo at 60% utilisation; scan cost scaled linearly from the measured n=200000)

Rows scan one column in RAM; rows marked `+rerank` also read 100 f32 master vectors per query from SSD (their IO is not priced).

| N | QPS | format | recall@10 | RAM $ | SSD $ | CPU $ | total $/month | cores |
|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 10M | 1 | mrl64 int8 +rerank (recall < 0.95) | 0.01 | 2 | 4 | 7 | 13 | 0.3 |
| 10M | 1 | binary rot +rerank (recall < 0.95) | 0.23 | 4 | 4 | 6 | 14 | 0.3 |
| 10M | 1 | mrl128 int8 +rerank (recall < 0.95) | 0.03 | 4 | 4 | 10 | 18 | 0.4 |
| 10M | 1 | mrl256 int8 +rerank (recall < 0.95) | 0.09 | 8 | 4 | 14 | 26 | 0.6 |
| 10M | 1 | int4 +rerank | 1.00 | 15 | 4 | 66 | 86 | 2.6 |
| 10M | 1 | int8 +rerank | 1.00 | 31 | 4 | 91 | 126 | 3.6 |
| 10M | 1 | 2-bit rot +rerank (recall < 0.95) | 0.92 | 8 | 4 | 268 | 280 | 10.7 |
| 10M | 1 | f32 | 1.00 | 123 | 0 | 434 | 557 | 17.4 |
| 10M | 1 | f16 | 1.00 | 61 | 4 | 950 | 1,016 | 38.0 |
| 10M | 50 | binary rot +rerank (recall < 0.95) | 0.23 | 4 | 4 | 322 | 330 | 12.9 |
| 10M | 50 | mrl64 int8 +rerank (recall < 0.95) | 0.01 | 2 | 4 | 335 | 342 | 13.4 |
| 10M | 50 | mrl128 int8 +rerank (recall < 0.95) | 0.03 | 4 | 4 | 501 | 509 | 20.1 |
| 10M | 50 | mrl256 int8 +rerank (recall < 0.95) | 0.09 | 8 | 4 | 696 | 708 | 27.9 |
| 10M | 50 | int4 +rerank | 1.00 | 15 | 4 | 3,312 | 3,332 | 132.5 |
| 10M | 50 | int8 +rerank | 1.00 | 31 | 4 | 4,542 | 4,577 | 181.7 |
| 10M | 50 | 2-bit rot +rerank (recall < 0.95) | 0.92 | 8 | 4 | 13,402 | 13,414 | 536.1 |
| 10M | 50 | f32 | 1.00 | 123 | 0 | 21,695 | 21,818 | 867.8 |
| 10M | 50 | f16 | 1.00 | 61 | 4 | 47,525 | 47,590 | 1,901.0 |
## What the numbers say

1. **Do not compress the scanned column.** The entropy coders get nothing off rotated codes (2-bit rot 0.96,
   binary 1.00) and decode at 0.5-0.9 GB/s, while the scans run at 15-47 GB/s. Decompress-then-scan would be
   20-60x slower than scanning the codes as stored. Store the hot column uncompressed and fixed-width.
2. **Compressibility is a signal, not an opportunity.** Unrotated int8 and int4 shrink to 0.64 and 0.27 on the
   mrl-like corpus because the decaying spectrum leaves most codes near zero: the bits are being wasted. The
   fix is to equalise the coordinates (rotate) or truncate, not to add a coder. On the isotropic corpus the same
   formats barely compress (0.91, 0.78).
3. **The f32 master is the only column worth compressing, and it is cold.** Byte-plane tANS takes 7.5% off, zstd
   -3 takes 14%, at about 0.5 and 0.9 GB/s decode. That is fine for a cold tier and irrelevant to a scan. The
   uncompressed f32 column decodes at memcpy speed, or at no cost if the stored bytes are borrowed.
4. **Prefix truncation is only as good as the model.** On the mrl-like corpus a 64-dimension int8 prefix scans
   in 0.7 ms (about 70x faster than f32 at 49 ms) and, with a 10x f32 re-rank, finds the exact top 10 on every
   query. On the isotropic corpus the same pipeline finds 0.8% of them. That is the Matryoshka claim, and the
   negative control `sangdb::mrl` already carries: the prefix is a usable embedding only if the model was
   trained for it. Check recall on your own model before choosing a prefix; this benchmark cannot.
5. **Quantise the full vector when the model is not Matryoshka.** On the isotropic corpus int8 and int4 with a
   re-rank both reach recall 1.00 at 4x and 8x less memory than f32; binary and the 2-bit column do not (0.23
   and 0.92).
6. **Cost is dominated by cores, so bytes scanned per query is the number to cut.** At 10M vectors and 1 QPS
   the cheapest configuration meeting recall (mrl-like corpus, 64-dim prefix plus re-rank) is about $11 a
   month against $534 for an f32 scan; at 50 QPS it is $266 against $20,666. At 100M and 50 QPS an exact scan
   needs 100+ cores even with the smallest column, which is where an index stops being optional.

## Limits and known weaknesses of this measurement

- **My kernels are not tuned.** The f16 row is slow because the half-to-float conversion is scalar (a machine
  with F16C would not be); the 2-bit row is slow because it is a lookup loop (sangdb says the same about its own
  kernel); the int4 here uses a plain per-vector max scale without rotation, which is why its scan-only recall is
  poor on the mrl-like corpus. Those rows measure the kernels, not the formats. int8, f32 and binary are the
  rows to trust for speed.
- The re-rank reads 100 f32 rows per query from RAM here; from SSD that is 100 random reads per query, which
  the cost model does not price.
- Recall is measured against exact f32 top-10 on 40 queries drawn from the generator. Queries come from the
  same distribution, with no near-duplicate in the corpus, which is easier than many real workloads.
- One machine, four threads, 819 MB of vectors: the f32 scan runs near memory bandwidth, so absolute GB/s will
  differ elsewhere. The ratios between formats are what transfer.
- The prices ($3/GB-month RAM, $0.10/GB-month SSD, $25/vCPU-month at 60% utilisation) are round numbers, not
  quotes. Rerun the script with `--ram`, `--ssd`, `--core`, `--util`.
- Insert and update cost, the ANN paths (`lsh`, `graph_vec`), filtered search and the persistence format are
  not measured.
