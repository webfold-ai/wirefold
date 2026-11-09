//! wirefold against serde_json, bincode, postcard, rmp-serde and rkyv on the same records.
//!
//! Run: `cargo run --release -- [records] [runs]` (default 20000 records, 3 runs). Prints markdown tables.
//! Every pipeline is round-trip checked before it is timed. Each pipeline runs in its own process,
//! `runs` times, and is reported as mean (minimum to maximum). A pipeline is a serializer followed by
//! an optional compressor: encode is serialize then compress, decode is decompress then deserialize,
//! and each stage is timed on its own inside the same run. Allocations are counted by a global
//! allocator wrapper (Rust-side only: zstd's internal C allocations are not seen).

use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting calls and requested bytes.
struct Counting;

// SAFETY: every method forwards to `System` with the arguments it was given; the counters are the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(l.size() as u64, Relaxed);
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(l.size() as u64, Relaxed);
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(new as u64, Relaxed);
        System.realloc(p, l, new)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// (allocation calls, bytes requested) so far.
fn alloc_counters() -> (u64, u64) {
    (ALLOCS.load(Relaxed), ALLOC_BYTES.load(Relaxed))
}

use std::hint::black_box;
use std::time::Instant;
use wirefold::codec::syntax::{parse_surface, Surface};

#[derive(
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
    wirefold::Collapse,
)]
struct Event {
    ts: u64,
    user: u32,
    kind: u8,
    value: f64,
    ok: bool,
    name: String,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Telemetry-shaped: increasing timestamps, small user and kind domains, a few repeated names.
fn ordered(n: usize) -> Vec<Event> {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let names = ["login", "logout", "purchase", "view", "search", "click"];
    let mut ts = 1_700_000_000_000u64;
    (0..n)
        .map(|_| {
            ts += 1 + r.next() % 50;
            Event {
                ts,
                user: (r.next() % 5000) as u32,
                kind: (r.next() % 6) as u8,
                value: (r.next() % 100_000) as f64 / 100.0,
                ok: !r.next().is_multiple_of(10),
                name: names[(r.next() % 6) as usize].to_string(),
            }
        })
        .collect()
}

/// Worst case for a columnar model: full-range random integers and floats, random-looking names.
fn random(n: usize) -> Vec<Event> {
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    (0..n)
        .map(|_| Event {
            ts: r.next(),
            user: r.next() as u32,
            kind: r.next() as u8,
            value: f64::from_bits(r.next() & 0x7FEF_FFFF_FFFF_FFFF),
            ok: r.next() & 1 == 0,
            name: format!("{:x}", r.next()),
        })
        .collect()
}

/// Median wall time of `runs` executions of `f`, in milliseconds.
fn ms<R>(runs: usize, mut f: impl FnMut() -> R) -> f64 {
    let mut t: Vec<f64> = (0..runs)
        .map(|_| {
            let s = Instant::now();
            black_box(f());
            s.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    t[t.len() / 2]
}

fn dataset_by_name(name: &str, n: usize) -> Vec<Event> {
    if name == "random" {
        random(n)
    } else {
        ordered(n)
    }
}

/// One column-read row, measured in this process: prints `| name | size | ms |`. Reading one `f64`
/// column without decoding the rest.
fn column_row(dataset: &str, n: usize, row: &str) {
    use wirefold::codec::web::CollapseElem as _;
    let data: &'static Vec<Event> = Box::leak(Box::new(dataset_by_name(dataset, n)));
    let packed = Event::collapse_pack(data);
    let wf: Vec<u8> = packed.as_bytes().to_vec();
    let want: f64 = data.iter().map(|e| e.value).sum();
    let runs = 9;
    let (name, size, t): (&str, usize, f64) = match row {
        "col-collapse" => {
            let sum = |v: &EventView| (0..v.collapse_len()).map(|i| v.value(i)).sum::<f64>();
            assert_eq!(sum(&EventView::collapse_open(&wf).unwrap()), want);
            let t = ms(runs, || sum(&EventView::collapse_open(&wf).unwrap()));
            ("wirefold view (open + per-element accessor)", wf.len(), t)
        }
        "col-collapse-slice" => {
            let sum = |v: &EventView| v.value_column().iter().sum::<f64>();
            assert_eq!(sum(&EventView::collapse_open(&wf).unwrap()), want);
            let t = ms(runs, || sum(&EventView::collapse_open(&wf).unwrap()));
            ("wirefold view (open + column slice)", wf.len(), t)
        }
        "col-collapse-open" => {
            let t = ms(runs, || {
                EventView::collapse_open(&wf).unwrap().collapse_len()
            });
            ("wirefold open only (validates the table)", wf.len(), t)
        }
        "col-collapse-read-elem" | "col-collapse-read-slice" => {
            let v = EventView::collapse_open(&wf).unwrap();
            let slice = row == "col-collapse-read-slice";
            let sum = || {
                if slice {
                    v.value_column().iter().sum::<f64>()
                } else {
                    (0..v.collapse_len()).map(|i| v.value(i)).sum::<f64>()
                }
            };
            assert_eq!(sum(), want);
            let t = ms(runs.max(25), sum);
            (
                if slice {
                    "wirefold, view already open: column slice"
                } else {
                    "wirefold, view already open: per-element accessor"
                },
                wf.len(),
                t,
            )
        }
        "col-rkyv" => {
            let b = rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap();
            let sum = |a: &rkyv::vec::ArchivedVec<ArchivedEvent>| {
                a.iter().map(|e| f64::from(e.value)).sum::<f64>()
            };
            let access = || {
                rkyv::access::<rkyv::vec::ArchivedVec<ArchivedEvent>, rkyv::rancor::Error>(&b)
                    .unwrap()
            };
            assert_eq!(sum(access()), want);
            let t = ms(runs, || sum(access()));
            ("rkyv 0.8 (validate + read)", b.len(), t)
        }
        "col-bincode" => {
            let b = bincode::serialize(data).unwrap();
            let t = ms(runs, || {
                bincode::deserialize::<Vec<Event>>(&b)
                    .unwrap()
                    .iter()
                    .map(|e| e.value)
                    .sum::<f64>()
            });
            ("bincode (decode all, then sum)", b.len(), t)
        }
        "col-postcard" => {
            let b = postcard::to_stdvec(data).unwrap();
            let t = ms(runs, || {
                postcard::from_bytes::<Vec<Event>>(&b)
                    .unwrap()
                    .iter()
                    .map(|e| e.value)
                    .sum::<f64>()
            });
            ("postcard (decode all, then sum)", b.len(), t)
        }
        "col-collapse-whole-zstd3" => {
            let z = zstd::encode_all(&wf[..], 3).unwrap();
            let read = || {
                let b = zstd::decode_all(&z[..]).unwrap();
                let v = EventView::collapse_open(&b).unwrap();
                (0..v.collapse_len()).map(|i| v.value(i)).sum::<f64>()
            };
            assert_eq!(read(), want);
            let t = ms(runs, read);
            (
                "whole buffer zstd -3: decompress all, then read",
                z.len(),
                t,
            )
        }
        "col-collapse-cols-zstd3" | "col-collapse-colsmart-zstd3" => {
            let mode = if row == "col-collapse-colsmart-zstd3" {
                Mode::Smart
            } else {
                Mode::Plain
            };
            let z = columns_compress(&wf, 3, mode, 1);
            assert_eq!(columns_value_sum(&z), want);
            let t = ms(runs, || columns_value_sum(&z));
            (
                if mode == Mode::Smart {
                    "columns, encoded planes, zstd -3: decompress one column"
                } else {
                    "columns, zstd -3: decompress one column"
                },
                z.len(),
                t,
            )
        }
        other => panic!("unknown column row {other:?}"),
    };
    println!("| {name} | {size} | {t:.3} |");
}

/// A serializer under test: bytes out of the records, and the records back out of bytes.
struct Serializer {
    name: &'static str,
    /// The records to bytes.
    ser: Box<dyn Fn() -> Vec<u8>>,
    /// Bytes to the records, consumed (so the work cannot be elided).
    de: Box<dyn Fn(&[u8])>,
    /// Whether the bytes decode to exactly the original records.
    check: Box<dyn Fn(&[u8]) -> bool>,
}

/// The six serializers. `wirefold` and `rkyv` convert to and from `Vec<u8>` (a copy each way) so that
/// every serializer has the same pipeline shape: rkyv needs aligned input, so its decode copies the
/// bytes into an aligned buffer, which is counted in its deserialize stage.
fn serializer(which: &str, data: &'static Vec<Event>) -> Serializer {
    use wirefold::codec::web::CollapseElem as _;
    match which {
        "wirefold" => Serializer {
            name: "wirefold Collapse",
            ser: Box::new(move || Event::collapse_pack(data).as_bytes().to_vec()),
            de: Box::new(|b| {
                black_box(EventView::collapse_open(b).unwrap().collapse_to_vec());
            }),
            check: Box::new(move |b| {
                EventView::collapse_open(b).unwrap().collapse_to_vec() == *data
            }),
        },
        "bincode" => Serializer {
            name: "bincode 1.3",
            ser: Box::new(move || bincode::serialize(data).unwrap()),
            de: Box::new(|b| {
                black_box(bincode::deserialize::<Vec<Event>>(b).unwrap());
            }),
            check: Box::new(move |b| bincode::deserialize::<Vec<Event>>(b).unwrap() == *data),
        },
        "postcard" => Serializer {
            name: "postcard",
            ser: Box::new(move || postcard::to_stdvec(data).unwrap()),
            de: Box::new(|b| {
                black_box(postcard::from_bytes::<Vec<Event>>(b).unwrap());
            }),
            check: Box::new(move |b| postcard::from_bytes::<Vec<Event>>(b).unwrap() == *data),
        },
        "rmp" => Serializer {
            name: "rmp-serde (MessagePack)",
            ser: Box::new(move || rmp_serde::to_vec(data).unwrap()),
            de: Box::new(|b| {
                black_box(rmp_serde::from_slice::<Vec<Event>>(b).unwrap());
            }),
            check: Box::new(move |b| rmp_serde::from_slice::<Vec<Event>>(b).unwrap() == *data),
        },
        "rkyv" => Serializer {
            name: "rkyv 0.8 (validated)",
            ser: Box::new(move || {
                rkyv::to_bytes::<rkyv::rancor::Error>(data)
                    .unwrap()
                    .to_vec()
            }),
            de: Box::new(|b| {
                let mut aligned = rkyv::util::AlignedVec::<16>::new();
                aligned.extend_from_slice(b);
                black_box(rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&aligned).unwrap());
            }),
            check: Box::new(move |b| {
                let mut aligned = rkyv::util::AlignedVec::<16>::new();
                aligned.extend_from_slice(b);
                rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&aligned).unwrap() == *data
            }),
        },
        "json" => Serializer {
            name: "serde_json",
            ser: Box::new(move || serde_json::to_vec(data).unwrap()),
            de: Box::new(|b| {
                black_box(serde_json::from_slice::<Vec<Event>>(b).unwrap());
            }),
            check: Box::new(move |b| serde_json::from_slice::<Vec<Event>>(b).unwrap() == *data),
        },
        other => panic!("unknown serializer {other:?}"),
    }
}

// ── Columns. A `Collapse` buffer of `Event`s is a section table with one section per scalar field
// (a contiguous column: `[count][values]`) and one for the strings (`[n][offsets][bytes]`). These
// helpers take the columns as planes, optionally transform each plane with an encoding chosen for its
// type, compress each plane as its own zstd frame (zstd stays in this crate), and rebuild the exact
// buffer. Reading one field decompresses only its plane.

/// Element widths of the scalar columns in section order: ts, user, kind, value, ok.
const SCALAR_WIDTHS: [usize; 5] = [8, 4, 1, 8, 1];
/// The planes, in frame order: the five scalar columns, then the string lengths and the string bytes.
const PLANE_NAMES: [&str; 7] = [
    "ts (u64)",
    "user (u32)",
    "kind (u8)",
    "value (f64)",
    "ok (bool)",
    "name lengths",
    "name bytes",
];
/// Index of the `f64` plane.
const VALUE_PLANE: usize = 3;
const NPLANES: usize = 7;

/// How planes are transformed before compression.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// The column bytes as stored.
    Plain,
    /// Byte-shuffle each multi-byte plane (kept for comparison).
    Shuffle,
    /// Encodings chosen per plane: delta, narrowing, decimal scaling, dictionary.
    Smart,
}

fn le_u64(b: &[u8], at: usize) -> usize {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as usize
}

/// `(start, length)` of every section of a `Collapse` buffer, from its section table.
fn sections(b: &[u8]) -> Vec<(usize, usize)> {
    let n = le_u64(b, 0);
    (0..n)
        .map(|j| (le_u64(b, 8 + 8 * j), le_u64(b, 8 + 8 * n + 8 * j)))
        .collect()
}

/// Group a plane's elements by byte position (all first bytes, then all second bytes, ...).
fn shuffle(d: &[u8], w: usize) -> Vec<u8> {
    if w <= 1 {
        return d.to_vec();
    }
    let n = d.len() / w;
    let mut o = vec![0u8; d.len()];
    for i in 0..n {
        for b in 0..w {
            o[b * n + i] = d[i * w + b];
        }
    }
    o
}

fn unshuffle(d: &[u8], w: usize) -> Vec<u8> {
    if w <= 1 {
        return d.to_vec();
    }
    let n = d.len() / w;
    let mut o = vec![0u8; d.len()];
    for i in 0..n {
        for b in 0..w {
            o[i * w + b] = d[b * n + i];
        }
    }
    o
}

/// The seven raw planes of an `Event` buffer: the scalar columns' values (without the count word), the
/// string lengths as `u64`s, and the string bytes.
fn raw_planes(b: &[u8]) -> Vec<Vec<u8>> {
    let secs = sections(b);
    let mut out: Vec<Vec<u8>> = (0..5)
        .map(|j| b[secs[j].0 + 8..secs[j].0 + secs[j].1].to_vec())
        .collect();
    let (st, _) = secs[5];
    let n = le_u64(b, st);
    let offs: Vec<usize> = (0..=n).map(|i| le_u64(b, st + 8 + 8 * i)).collect();
    out.push(
        (0..n)
            .flat_map(|i| ((offs[i + 1] - offs[i]) as u64).to_le_bytes())
            .collect(),
    );
    let text = st + 8 + 8 * (n + 1);
    out.push(b[text..text + offs[n]].to_vec());
    out
}

/// Rebuild the exact buffer from its section table (`header`) and raw planes.
fn from_raw_planes(header: &[u8], planes: &[Vec<u8>], total: usize) -> Vec<u8> {
    let secs = sections(header);
    let mut out = vec![0u8; total];
    out[..header.len()].copy_from_slice(header);
    for j in 0..5 {
        let (st, _) = secs[j];
        out[st..st + 8]
            .copy_from_slice(&((planes[j].len() / SCALAR_WIDTHS[j]) as u64).to_le_bytes());
        out[st + 8..st + 8 + planes[j].len()].copy_from_slice(&planes[j]);
    }
    let (st, _) = secs[5];
    let n = planes[5].len() / 8;
    out[st..st + 8].copy_from_slice(&(n as u64).to_le_bytes());
    let mut off = 0u64;
    out[st + 8..st + 16].copy_from_slice(&off.to_le_bytes());
    for i in 0..n {
        off += u64::from_le_bytes(planes[5][8 * i..8 * i + 8].try_into().unwrap());
        out[st + 16 + 8 * i..st + 24 + 8 * i].copy_from_slice(&off.to_le_bytes());
    }
    let t = st + 8 + 8 * (n + 1);
    out[t..t + planes[6].len()].copy_from_slice(&planes[6]);
    out
}

/// The smallest byte width (1, 2, 4 or 8) that holds `max`.
fn width_for(max: u64) -> usize {
    match max {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFFFF_FFFF => 4,
        _ => 8,
    }
}

fn put_narrow(out: &mut Vec<u8>, v: u64, w: usize) {
    out.extend_from_slice(&v.to_le_bytes()[..w]);
}

fn get_narrow(b: &[u8], i: usize, w: usize) -> u64 {
    let mut x = [0u8; 8];
    x[..w].copy_from_slice(&b[i * w..(i + 1) * w]);
    u64::from_le_bytes(x)
}

/// Plane tags written as the first byte of a transformed plane.
const T_RAW: u8 = 0;
const T_SHUFFLE: u8 = 1;
const T_DELTA: u8 = 2;
const T_INTS: u8 = 3;
const T_DECIMAL: u8 = 4;
const T_DICT: u8 = 5;
/// Integer-stream tags: values minus their minimum at the narrowest byte width, or at the exact bit width.
const I_NARROW: u8 = 0;
const I_BITS: u8 = 1;

/// Pack `v` (each below `2^bits`) into `bits` bits apiece, least significant bit first.
fn pack_bits(v: &[u64], bits: usize) -> Vec<u8> {
    let mut out = vec![0u8; (v.len() * bits).div_ceil(8)];
    for (i, &x) in v.iter().enumerate() {
        let mut at = i * bits;
        let mut left = bits;
        let mut x = x;
        while left > 0 {
            let (byte, off) = (at / 8, at % 8);
            let take = (8 - off).min(left);
            out[byte] |= ((x & ((1 << take) - 1)) as u8) << off;
            x >>= take;
            at += take;
            left -= take;
        }
    }
    out
}

fn unpack_bits(b: &[u8], n: usize, bits: usize) -> Vec<u64> {
    (0..n)
        .map(|i| {
            let (mut at, mut left, mut x, mut got) = (i * bits, bits, 0u64, 0);
            while left > 0 {
                let (byte, off) = (at / 8, at % 8);
                let take = (8 - off).min(left);
                x |= u64::from((b[byte] >> off) & ((1u16 << take) - 1) as u8) << got;
                at += take;
                got += take;
                left -= take;
            }
            x
        })
        .collect()
}

/// An integer sequence as `[kind][width][minimum][data]`: the values less their minimum, at the narrowest
/// byte width (which a byte compressor then shrinks further) or packed to the exact bit width (which it
/// cannot, but which is smaller when the values are spread evenly). The smaller after zstd -3 wins.
fn enc_ints(v: &[i64]) -> Vec<u8> {
    let min = v.iter().copied().min().unwrap_or(0);
    let off: Vec<u64> = v.iter().map(|x| (x - min) as u64).collect();
    let range = off.iter().copied().max().unwrap_or(0);
    let w = width_for(range);
    let mut narrow = vec![I_NARROW, w as u8];
    narrow.extend_from_slice(&min.to_le_bytes());
    for &x in &off {
        put_narrow(&mut narrow, x, w);
    }
    let bits = (64 - range.leading_zeros() as usize).max(1);
    let mut packed = vec![I_BITS, bits as u8];
    packed.extend_from_slice(&min.to_le_bytes());
    packed.extend(pack_bits(&off, bits));
    if zstd::encode_all(&narrow[..], 3).unwrap().len() <= packed.len() {
        narrow
    } else {
        packed
    }
}

fn dec_ints(e: &[u8], n: usize) -> Vec<i64> {
    let w = e[1] as usize;
    let min = i64::from_le_bytes(e[2..10].try_into().unwrap());
    let d = &e[10..];
    let off: Vec<u64> = if e[0] == I_NARROW {
        (0..n).map(|i| get_narrow(d, i, w)).collect()
    } else {
        unpack_bits(d, n, w)
    };
    off.into_iter().map(|x| x as i64 + min).collect()
}

/// Encode scalar plane `j` (raw values) with the encoding that suits its type, or leave it raw.
fn encode_scalar(j: usize, raw: &[u8], mode: Mode) -> Vec<u8> {
    let w = SCALAR_WIDTHS[j];
    let mut out = Vec::new();
    match mode {
        Mode::Plain => {
            out.push(T_RAW);
            out.extend_from_slice(raw);
        }
        Mode::Shuffle => {
            out.push(T_SHUFFLE);
            out.extend(shuffle(raw, w));
        }
        Mode::Smart => match j {
            // an increasing u64: the first value, then the differences as an integer sequence
            0 => {
                let v: Vec<u64> = (0..raw.len() / 8).map(|i| get_narrow(raw, i, 8)).collect();
                let d: Vec<i64> = v
                    .windows(2)
                    .map(|p| p[1].wrapping_sub(p[0]) as i64)
                    .collect();
                out.push(T_DELTA);
                out.extend_from_slice(&v.first().copied().unwrap_or(0).to_le_bytes());
                out.extend(enc_ints(&d));
            }
            // a u32 that rarely needs 4 bytes: an integer sequence
            1 => {
                let v: Vec<i64> = (0..raw.len() / 4)
                    .map(|i| get_narrow(raw, i, 4) as i64)
                    .collect();
                out.push(T_INTS);
                out.extend(enc_ints(&v));
            }
            // an f64 that is a decimal number: scaled to an integer sequence
            3 => {
                let v: Vec<f64> = (0..raw.len() / 8)
                    .map(|i| f64::from_bits(get_narrow(raw, i, 8)))
                    .collect();
                let scaled = [0u32, 1, 2, 3, 4, 6].iter().find_map(|&sc| {
                    let m = 10f64.powi(sc as i32);
                    let c: Vec<i64> = v.iter().map(|x| (x * m).round() as i64).collect();
                    let ok = v.iter().zip(&c).all(|(x, ci)| {
                        x.is_finite()
                            && x.abs() * m < 9.0e15
                            && (*ci as f64 / m).to_bits() == x.to_bits()
                    });
                    ok.then_some((sc, c))
                });
                if let Some((sc, c)) = scaled {
                    out.push(T_DECIMAL);
                    out.push(sc as u8);
                    out.extend(enc_ints(&c));
                } else {
                    out.push(T_RAW);
                    out.extend_from_slice(raw);
                }
            }
            _ => {
                out.push(T_RAW);
                out.extend_from_slice(raw);
            }
        },
    }
    out
}

fn decode_scalar(j: usize, enc: &[u8], n: usize) -> Vec<u8> {
    let w = SCALAR_WIDTHS[j];
    match enc[0] {
        T_RAW => enc[1..].to_vec(),
        T_SHUFFLE => unshuffle(&enc[1..], w),
        T_DELTA => {
            let mut v = u64::from_le_bytes(enc[1..9].try_into().unwrap());
            let mut out = v.to_le_bytes().to_vec();
            for d in dec_ints(&enc[9..], n - 1) {
                v = v.wrapping_add(d as u64);
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        }
        T_INTS => dec_ints(&enc[1..], n)
            .into_iter()
            .flat_map(|x| (x as u32).to_le_bytes())
            .collect(),
        T_DECIMAL => {
            let m = 10f64.powi(i32::from(enc[1]));
            dec_ints(&enc[2..], n)
                .into_iter()
                .flat_map(|c| (c as f64 / m).to_bits().to_le_bytes())
                .collect()
        }
        t => panic!("bad plane tag {t}"),
    }
}

/// Encode the two string planes: a dictionary and one-byte indices when there are at most 255 distinct
/// strings, otherwise the lengths and bytes as stored (or shuffled lengths in `Shuffle` mode).
fn encode_strings(lens: &[u8], bytes: &[u8], mode: Mode) -> (Vec<u8>, Vec<u8>) {
    if mode == Mode::Smart {
        let n = lens.len() / 8;
        let mut at = 0usize;
        let mut dict: Vec<&[u8]> = Vec::new();
        let mut idx = Vec::with_capacity(n);
        for i in 0..n {
            let l = get_narrow(lens, i, 8) as usize;
            let s = &bytes[at..at + l];
            at += l;
            let k = dict.iter().position(|d| *d == s).unwrap_or_else(|| {
                dict.push(s);
                dict.len() - 1
            });
            idx.push(k);
            if dict.len() > 255 {
                break;
            }
        }
        if dict.len() <= 255 && idx.len() == n {
            let mut p5 = vec![T_DICT];
            p5.extend(idx.iter().map(|&k| k as u8));
            let mut p6 = vec![T_DICT, dict.len() as u8];
            for d in &dict {
                p6.extend((d.len() as u32).to_le_bytes());
            }
            for d in &dict {
                p6.extend_from_slice(d);
            }
            return (p5, p6);
        }
    }
    let mut p5 = vec![if mode == Mode::Shuffle {
        T_SHUFFLE
    } else {
        T_RAW
    }];
    p5.extend(if mode == Mode::Shuffle {
        shuffle(lens, 8)
    } else {
        lens.to_vec()
    });
    let mut p6 = vec![T_RAW];
    p6.extend_from_slice(bytes);
    (p5, p6)
}

fn decode_strings(p5: &[u8], p6: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if p5[0] == T_DICT {
        let nd = p6[1] as usize;
        let dl: Vec<usize> = (0..nd)
            .map(|i| u32::from_le_bytes(p6[2 + 4 * i..6 + 4 * i].try_into().unwrap()) as usize)
            .collect();
        let mut start = Vec::new();
        let mut at = 2 + 4 * nd;
        for l in &dl {
            start.push(at);
            at += l;
        }
        let (mut lens, mut bytes) = (Vec::new(), Vec::new());
        for &k in &p5[1..] {
            let k = k as usize;
            lens.extend((dl[k] as u64).to_le_bytes());
            bytes.extend_from_slice(&p6[start[k]..start[k] + dl[k]]);
        }
        (lens, bytes)
    } else {
        let lens = if p5[0] == T_SHUFFLE {
            unshuffle(&p5[1..], 8)
        } else {
            p5[1..].to_vec()
        };
        (lens, p6[1..].to_vec())
    }
}

/// The encoded planes of a buffer, in frame order.
fn encoded_planes(b: &[u8], mode: Mode) -> Vec<Vec<u8>> {
    let raw = raw_planes(b);
    let mut out: Vec<Vec<u8>> = (0..5).map(|j| encode_scalar(j, &raw[j], mode)).collect();
    let (p5, p6) = encode_strings(&raw[5], &raw[6], mode);
    out.push(p5);
    out.push(p6);
    out
}

/// Compress a buffer plane by plane, the planes spread over `threads` threads (static: plane `i` goes to
/// thread `i % threads`): `[total len][section table][compressed length per plane][frames]`.
fn columns_compress(b: &[u8], level: i32, mode: Mode, threads: usize) -> Vec<u8> {
    let planes = encoded_planes(b, mode);
    let frames: Vec<Vec<u8>> = if threads <= 1 {
        planes
            .iter()
            .map(|p| zstd::encode_all(&p[..], level).unwrap())
            .collect()
    } else {
        let mut slots: Vec<Option<Vec<u8>>> = vec![None; planes.len()];
        std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let planes = &planes;
                    sc.spawn(move || {
                        (t..planes.len())
                            .step_by(threads)
                            .map(|i| (i, zstd::encode_all(&planes[i][..], level).unwrap()))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for h in hs {
                for (i, f) in h.join().unwrap() {
                    slots[i] = Some(f);
                }
            }
        });
        slots.into_iter().map(Option::unwrap).collect()
    };
    let header_len = 8 * (1 + 2 * le_u64(b, 0));
    let mut out = Vec::new();
    out.extend((b.len() as u32).to_le_bytes());
    out.extend_from_slice(&b[..header_len]);
    out.extend((le_u64(b, sections(b)[0].0) as u32).to_le_bytes());
    for f in &frames {
        out.extend((f.len() as u32).to_le_bytes());
    }
    for f in &frames {
        out.extend_from_slice(f);
    }
    out
}

/// The parts of a plane-compressed buffer: total length, section table, record count and each frame.
fn columns_parse(z: &[u8]) -> (usize, &[u8], usize, Vec<&[u8]>) {
    let u32_at = |at: usize| u32::from_le_bytes(z[at..at + 4].try_into().unwrap()) as usize;
    let header_len = 8 * (1 + 2 * le_u64(z, 4));
    let n = u32_at(4 + header_len);
    let mut at = 8 + header_len + 4 * NPLANES;
    let frames = (0..NPLANES)
        .map(|j| {
            let l = u32_at(8 + header_len + 4 * j);
            let f = &z[at..at + l];
            at += l;
            f
        })
        .collect();
    (u32_at(0), &z[4..4 + header_len], n, frames)
}

/// Rebuild the exact buffer from its plane frames.
fn columns_decompress(z: &[u8]) -> Vec<u8> {
    let (total, header, n, frames) = columns_parse(z);
    let dec: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| zstd::decode_all(*f).unwrap())
        .collect();
    let mut raw: Vec<Vec<u8>> = (0..5).map(|j| decode_scalar(j, &dec[j], n)).collect();
    let (lens, bytes) = decode_strings(&dec[5], &dec[6]);
    raw.push(lens);
    raw.push(bytes);
    from_raw_planes(header, &raw, total)
}

/// Sum of the `f64` field from a plane-compressed buffer, decompressing only that field's plane.
fn columns_value_sum(z: &[u8]) -> f64 {
    let (_, _, n, frames) = columns_parse(z);
    let raw = decode_scalar(
        VALUE_PLANE,
        &zstd::decode_all(frames[VALUE_PLANE]).unwrap(),
        n,
    );
    raw.chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .sum()
}

/// The compression stage after a serializer.
#[derive(Clone, Copy)]
enum Codec {
    /// No compression: the serialized bytes are the output.
    Raw,
    Zstd(i32),
    /// wirefold's own `compress()`.
    Wirefold,
    /// A `Collapse` buffer compressed column by column with zstd, the planes transformed per `mode`.
    Columns {
        level: i32,
        mode: Mode,
    },
}

impl Codec {
    fn parse(s: &str) -> Codec {
        match s {
            "raw" => Codec::Raw,
            "wirefold" => Codec::Wirefold,
            c if c.starts_with("colshuf") => Codec::Columns {
                level: c["colshuf".len()..].parse().expect("a level"),
                mode: Mode::Shuffle,
            },
            c if c.starts_with("colsmart") => Codec::Columns {
                level: c["colsmart".len()..].parse().expect("a level"),
                mode: Mode::Smart,
            },
            c if c.starts_with("cols") => Codec::Columns {
                level: c["cols".len()..].parse().expect("a level"),
                mode: Mode::Plain,
            },
            z => Codec::Zstd(
                z.strip_prefix("zstd")
                    .expect("a codec")
                    .parse()
                    .expect("a level"),
            ),
        }
    }

    fn label(self) -> String {
        match self {
            Codec::Raw => "raw".to_string(),
            Codec::Zstd(l) => format!("+ zstd -{l}"),
            Codec::Wirefold => "+ wirefold compress()".to_string(),
            Codec::Columns { level, mode } => match mode {
                Mode::Plain => format!("+ columns, zstd -{level}"),
                Mode::Shuffle => format!("+ columns, byte shuffle, zstd -{level}"),
                Mode::Smart => format!("+ columns, encoded planes, zstd -{level}"),
            },
        }
    }

    fn compress(self, bytes: Vec<u8>) -> Vec<u8> {
        match self {
            Codec::Raw => bytes,
            Codec::Zstd(l) => zstd::encode_all(&bytes[..], l).unwrap(),
            Codec::Wirefold => wirefold::codec::compress(&bytes),
            Codec::Columns { level, mode } => columns_compress(&bytes, level, mode, 1),
        }
    }

    fn decompress(self, z: &[u8]) -> Cow<'_, [u8]> {
        match self {
            Codec::Raw => Cow::Borrowed(z),
            Codec::Zstd(_) => Cow::Owned(zstd::decode_all(z).unwrap()),
            Codec::Wirefold => Cow::Owned(wirefold::codec::decompress(z)),
            Codec::Columns { .. } => Cow::Owned(columns_decompress(z)),
        }
    }

    /// Repetitions per run: the slow settings take longer than the others.
    fn reps(self) -> usize {
        match self {
            Codec::Zstd(l) | Codec::Columns { level: l, .. } if l >= 10 => 3,
            Codec::Wirefold => 5,
            _ => 9,
        }
    }
}

/// Median of a set of timings.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// One pipeline, measured in this process; prints
/// `| name | size | enc total | serialize | compress | dec total | decompress | deserialize | enc allocs | enc bytes | dec allocs | dec bytes |`
/// (times in milliseconds, medians of the repetitions).
fn pipeline_row(dataset: &str, n: usize, which: &str, codec: &str) {
    let data: &'static Vec<Event> = Box::leak(Box::new(dataset_by_name(dataset, n)));
    let s = serializer(which, data);
    let c = Codec::parse(codec);
    let z = c.compress((s.ser)());
    assert!((s.check)(&c.decompress(&z)), "{which} {codec} round trip");
    let ms_of = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    let reps = c.reps();
    let (mut ser_t, mut comp_t, mut dec_t, mut de_t) = (vec![], vec![], vec![], vec![]);
    for _ in 0..reps {
        let t = Instant::now();
        let bytes = black_box((s.ser)());
        ser_t.push(ms_of(t));
        let t = Instant::now();
        black_box(c.compress(bytes));
        comp_t.push(ms_of(t));
    }
    for _ in 0..reps {
        let t = Instant::now();
        let plain = black_box(c.decompress(&z));
        dec_t.push(ms_of(t));
        let t = Instant::now();
        (s.de)(&plain);
        de_t.push(ms_of(t));
    }
    // allocations of one encode and one decode
    let before = alloc_counters();
    black_box(c.compress((s.ser)()));
    let mid = alloc_counters();
    (s.de)(&c.decompress(&z));
    let after = alloc_counters();
    let (ser, comp, dec, de) = (median(ser_t), median(comp_t), median(dec_t), median(de_t));
    println!(
        "| {} {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} | {} | {} |",
        s.name,
        c.label(),
        z.len(),
        ser + comp,
        ser,
        comp,
        dec + de,
        dec,
        de,
        mid.0 - before.0,
        mid.1 - before.1,
        after.0 - mid.0,
        after.1 - mid.1,
    );
}

/// Mean, minimum and maximum of a set of timings.
fn spread(v: &[f64]) -> (f64, f64, f64) {
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, min, max)
}

/// Run `args` as a child of this executable and return the first `| ` line, split into trimmed columns.
fn child_row(args: &[&str]) -> Vec<String> {
    let exe = std::env::current_exe().expect("own path");
    let out = std::process::Command::new(exe)
        .args(args)
        .output()
        .expect("child run");
    assert!(out.status.success(), "{args:?} child failed");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.starts_with("| "))
        .expect("a result row");
    line.split('|').map(|c| c.trim().to_string()).collect()
}

/// The comparison: every serializer, raw and followed by each compressor, on the same records. Each
/// pipeline runs in its own process `runs` times; the totals are reported as mean (minimum to
/// maximum), the stage timings as means, allocations from the first run (they do not vary).
fn serialize_tables(n: usize, runs: usize) {
    println!(
        "wirefold {} against serde_json 1, bincode 1.3, postcard 1, rmp-serde 1, rkyv 0.8 and zstd; {n} records, {runs} runs per cell",
        env!("CARGO_PKG_VERSION")
    );
    let fmt = |v: &[f64]| {
        let (m, lo, hi) = spread(v);
        format!("{m:.2} ({lo:.2} to {hi:.2})")
    };
    let mean = |v: &[f64]| spread(v).0;
    for (title, dataset) in [
        ("Telemetry-shaped records", "ordered"),
        ("Random records (no structure to exploit)", "random"),
    ] {
        println!("\n### {title} ({n} records)\n");
        println!("Encode is serialize then compress; decode is decompress then deserialize. Milliseconds. Allocations are Rust-side calls and MiB requested for one encode and one decode.\n");
        println!("| pipeline | size (bytes) | bytes/record | encode (ms) | serialize | compress | decode (ms) | decompress | deserialize | encode allocs (MiB) | decode allocs (MiB) |");
        println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        for which in ["wirefold", "bincode", "postcard", "rmp", "rkyv", "json"] {
            let mut codecs = vec!["raw", "zstd1", "zstd3", "zstd6", "zstd19", "wirefold"];
            if which == "wirefold" {
                codecs.extend([
                    "cols1",
                    "cols3",
                    "cols6",
                    "cols19",
                    "colsmart1",
                    "colsmart3",
                    "colsmart6",
                    "colsmart19",
                    "colshuf3",
                ]);
            }
            for codec in codecs {
                let rows: Vec<Vec<String>> = (0..runs)
                    .map(|_| child_row(&["pipeline", dataset, &n.to_string(), which, codec]))
                    .collect();
                let col =
                    |i: usize| -> Vec<f64> { rows.iter().map(|r| r[i].parse().unwrap()).collect() };
                let size: usize = rows[0][2].parse().unwrap();
                let mib = |b: &str| b.parse::<f64>().unwrap() / (1024.0 * 1024.0);
                println!(
                    "| {} | {size} | {:.1} | {} | {:.2} | {:.2} | {} | {:.2} | {:.2} | {} ({:.1}) | {} ({:.1}) |",
                    rows[0][1],
                    size as f64 / n as f64,
                    fmt(&col(3)),
                    mean(&col(4)),
                    mean(&col(5)),
                    fmt(&col(6)),
                    mean(&col(7)),
                    mean(&col(8)),
                    rows[0][9],
                    mib(&rows[0][10]),
                    rows[0][11],
                    mib(&rows[0][12]),
                );
            }
        }
        println!("\nSum one `f64` column of {n} records (open + read, no full decode):\n");
        println!("| format | time (ms) |");
        println!("|---|---:|");
        for row in [
            "col-collapse-open",
            "col-collapse-read-elem",
            "col-collapse-read-slice",
            "col-collapse",
            "col-collapse-slice",
            "col-rkyv",
            "col-bincode",
            "col-postcard",
            "col-collapse-whole-zstd3",
            "col-collapse-cols-zstd3",
            "col-collapse-colsmart-zstd3",
        ] {
            let times: Vec<f64> = (0..runs)
                .map(|_| {
                    child_row(&["column", dataset, &n.to_string(), row])[3]
                        .parse()
                        .unwrap()
                })
                .collect();
            let name = child_row(&["column", dataset, &n.to_string(), row])[1].clone();
            println!("| {name} | {} |", fmt(&times));
        }
    }
}

/// The `Event` buffer of one dataset, built the way the pipelines build it.
fn event_buffer(dataset: &str, n: usize) -> Vec<u8> {
    use wirefold::codec::web::CollapseElem as _;
    let data = dataset_by_name(dataset, n);
    Event::collapse_pack(&data).as_bytes().to_vec()
}

/// Where the bytes are: every plane of the buffer, raw and under zstd at three levels, as stored and as
/// encoded for its type.
fn plane_report(dataset: &str, n: usize) {
    let b = event_buffer(dataset, n);
    let raw = raw_planes(&b);
    let enc = encoded_planes(&b, Mode::Smart);
    let z = |d: &[u8], l: i32| zstd::encode_all(d, l).unwrap().len();
    println!(
        "\n#### Planes, {dataset} records ({n} records, {} bytes)\n",
        b.len()
    );
    println!("| plane | raw (B) | zstd -1 | zstd -3 | zstd -19 | encoded (B) | encoded, zstd -1 | zstd -3 | zstd -19 | encoding |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---|");
    let tag = |t: u8| match t {
        T_DELTA => "delta, integer",
        T_INTS => "integer",
        T_DECIMAL => "decimal scaled, integer",
        T_DICT => "dictionary",
        _ => "as stored",
    };
    for j in 0..NPLANES {
        // the string planes are encoded together; show the dictionary tag on both
        let (e, t) = (&enc[j], tag(enc[j][0]));
        let row = [
            raw[j].len(),
            z(&raw[j], 1),
            z(&raw[j], 3),
            z(&raw[j], 19),
            e.len(),
            z(e, 1),
            z(e, 3),
            z(e, 19),
        ];
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {t} |",
            PLANE_NAMES[j], row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]
        );
    }
    let last: Vec<usize> = (0..NPLANES).map(|j| z(&enc[j], 19)).collect();
    println!(
        "| **total** | {} | {} | {} | {} | {} | {} | {} | {} | |",
        raw.iter().map(Vec::len).sum::<usize>(),
        raw.iter().map(|p| z(p, 1)).sum::<usize>(),
        raw.iter().map(|p| z(p, 3)).sum::<usize>(),
        raw.iter().map(|p| z(p, 19)).sum::<usize>(),
        enc.iter().map(Vec::len).sum::<usize>(),
        enc.iter().map(|p| z(p, 1)).sum::<usize>(),
        enc.iter().map(|p| z(p, 3)).sum::<usize>(),
        last.iter().sum::<usize>(),
    );
}

/// Size and time at every zstd level that matters, for the whole buffer and for per-plane compression
/// (plain and encoded planes, and the encoded planes spread over 4 threads). Times are the median of 5
/// in-process runs.
fn level_report(dataset: &str, n: usize) {
    let b = event_buffer(dataset, n);
    println!(
        "\n#### zstd levels, {dataset} records ({n} records, {} bytes)\n",
        b.len()
    );
    println!("| level | whole: size | enc ms | dec ms | planes: size | enc ms | dec ms | encoded planes: size | enc ms | dec ms | enc ms, 4 threads |");
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for level in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 15, 19, 22] {
        let reps = if level >= 15 { 3 } else { 5 };
        let w = zstd::encode_all(&b[..], level).unwrap();
        let (we, wd) = (
            ms(reps, || zstd::encode_all(&b[..], level).unwrap()),
            ms(reps, || zstd::decode_all(&w[..]).unwrap()),
        );
        let mut row = vec![
            format!("{level}"),
            format!("{}", w.len()),
            format!("{we:.2}"),
            format!("{wd:.2}"),
        ];
        for mode in [Mode::Plain, Mode::Smart] {
            let c = columns_compress(&b, level, mode, 1);
            assert_eq!(columns_decompress(&c), b);
            row.push(format!("{}", c.len()));
            row.push(format!(
                "{:.2}",
                ms(reps, || columns_compress(&b, level, mode, 1))
            ));
            row.push(format!("{:.2}", ms(reps, || columns_decompress(&c))));
        }
        row.push(format!(
            "{:.2}",
            ms(reps, || columns_compress(&b, level, Mode::Smart, 4))
        ));
        println!("| {} |", row.join(" | "));
    }
}

/// FNV-1a over a block: a fast 64-bit non-cryptographic checksum, used here to time the cost of one and
/// to find duplicate blocks (duplicates are confirmed by comparing bytes).
fn fnv(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A block store over the encoded planes: every plane cut into fixed-size blocks, each compressed on its
/// own with a checksum, duplicates stored once. For each block size: total size (with a 16-byte
/// pointer per block: checksum, logical and stored length), encode and decode time, the time to read
/// 100 rows of the `f64` column by decompressing only the blocks that cover them against decompressing the
/// whole plane, the share of bytes in duplicate blocks, and the checksum cost.
fn block_report(dataset: &str, n: usize) {
    let b = event_buffer(dataset, n);
    let planes = encoded_planes(&b, Mode::Smart);
    println!("\n#### Block store over the encoded planes, {dataset} records ({n} records)\n");
    println!("| block size | blocks | stored (B) | with pointers (B) | encode ms | decode ms | read 100 rows (ms) | whole plane read (ms) | duplicate bytes | checksum ms |");
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    let value = &planes[VALUE_PLANE];
    // where the values start in the plane and how many bits each takes: a decimal-scaled integer plane has
    // a 12-byte header (tag, scale, kind, width, minimum) and then `width` bytes or bits per value;
    // anything else is a 1-byte tag and 8-byte floats
    let (hdr, vbits) = if value[0] == T_DECIMAL {
        let w = usize::from(value[3]);
        (12, if value[2] == I_NARROW { 8 * w } else { w })
    } else {
        (1, 64)
    };
    for bs in [1usize << 12, 1 << 14, 1 << 16, 1 << 18, usize::MAX] {
        let blocks: Vec<&[u8]> = planes
            .iter()
            .flat_map(|p| p.chunks(bs.min(p.len().max(1))))
            .collect();
        let comp: Vec<Vec<u8>> = blocks
            .iter()
            .map(|x| zstd::encode_all(*x, 3).unwrap())
            .collect();
        let stored: usize = comp.iter().map(Vec::len).sum();
        let enc_ms = ms(5, || {
            blocks
                .iter()
                .map(|x| zstd::encode_all(*x, 3).unwrap().len())
                .sum::<usize>()
        });
        let dec_ms = ms(5, || {
            comp.iter()
                .map(|z| zstd::decode_all(&z[..]).unwrap().len())
                .sum::<usize>()
        });
        // rows 10000..10100 of the value plane: its blocks are those of plane VALUE_PLANE
        let vb: Vec<Vec<u8>> = value
            .chunks(bs.min(value.len()))
            .map(|x| zstd::encode_all(x, 3).unwrap())
            .collect();
        let per = bs.min(value.len());
        let (lo, hi) = (hdr + 10_000 * vbits / 8, hdr + (10_100 * vbits).div_ceil(8));
        let (b0, b1) = (lo / per, (hi - 1) / per);
        let read_ms = ms(9, || {
            (b0..=b1)
                .map(|i| zstd::decode_all(&vb[i][..]).unwrap().len())
                .sum::<usize>()
        });
        let whole: Vec<u8> = zstd::encode_all(&value[..], 3).unwrap();
        let whole_ms = ms(9, || zstd::decode_all(&whole[..]).unwrap().len());
        // duplicate blocks: same checksum and same bytes
        let mut seen: std::collections::HashMap<u64, Vec<&[u8]>> = std::collections::HashMap::new();
        let mut dup = 0usize;
        for x in &blocks {
            let e = seen.entry(fnv(x)).or_default();
            if e.iter().any(|y| y == x) {
                dup += x.len();
            } else {
                e.push(x);
            }
        }
        let sum_ms = ms(5, || {
            blocks.iter().map(|x| fnv(x)).fold(0u64, u64::wrapping_add)
        });
        let label = if bs == usize::MAX {
            "whole plane".to_string()
        } else {
            format!("{} KiB", bs / 1024)
        };
        println!(
            "| {label} | {} | {stored} | {} | {enc_ms:.2} | {dec_ms:.2} | {read_ms:.3} | {whole_ms:.3} | {dup} | {sum_ms:.2} |",
            blocks.len(),
            stored + 16 * blocks.len(),
        );
    }
}

/// Model weights, the case `seed-format` stores: bf16 values drawn from a normal distribution (synthetic,
/// standard deviation 0.02, as in an initialised or trained linear layer). Compared: the bytes as stored,
/// the two bytes of each value in separate planes, and the sign-and-exponent byte alone, under zstd.
fn bf16_report(n: usize) {
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    let mut unit = move || (r.next() >> 11) as f64 / (1u64 << 53) as f64;
    let vals: Vec<u16> = (0..n)
        .map(|_| {
            let (u1, u2) = (unit().max(1e-12), unit());
            let g = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos() * 0.02;
            let bits = (g as f32).to_bits();
            // round to nearest even into bf16
            ((bits + 0x7FFF + ((bits >> 16) & 1)) >> 16) as u16
        })
        .collect();
    let raw: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
    let hi: Vec<u8> = vals.iter().map(|v| (v >> 8) as u8).collect();
    let lo: Vec<u8> = vals.iter().map(|v| *v as u8).collect();
    let z = |d: &[u8], l: i32| zstd::encode_all(d, l).unwrap().len();
    println!(
        "\n#### bf16 weights, synthetic normal (sd 0.02), {n} values, {} bytes\n",
        raw.len()
    );
    println!("| representation | zstd -1 | zstd -3 | zstd -19 | share of raw at -3 |");
    println!("|---|---:|---:|---:|---:|");
    let row = |name: &str, parts: &[&[u8]]| {
        let t: Vec<usize> = [1, 3, 19]
            .iter()
            .map(|&l| parts.iter().map(|p| z(p, l)).sum())
            .collect();
        println!(
            "| {name} | {} | {} | {} | {:.1}% |",
            t[0],
            t[1],
            t[2],
            100.0 * t[1] as f64 / raw.len() as f64
        );
    };
    row("as stored (interleaved bytes)", &[&raw]);
    row(
        "two byte planes (high, low), compressed separately",
        &[&hi, &lo],
    );
    row("high byte plane alone (sign and 7 exponent bits)", &[&hi]);
    row(
        "low byte plane alone (1 exponent bit, 7 mantissa bits)",
        &[&lo],
    );
}

/// Number formatting, which every text format pays for: std against `ryu` and `itoa`, and against
/// keeping the number's text as it arrived (what the term does for JSON and YAML).
fn number_formatting(only: Option<&str>) {
    let count = 1_000_000usize;
    let floats: Vec<f64> = (0..count).map(|i| (i as f64 * 7.0 + 3.0) / 100.0).collect();
    let ints: Vec<u64> = (0..count as u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 20)
        .collect();
    let texts: Vec<String> = floats.iter().map(|f| f.to_string()).collect();
    let want = only.unwrap_or("");
    println!("\n### Number formatting and parsing ({count} values)\n");
    println!("| operation | time (ms) | ns/value |");
    println!("|---|---:|---:|");
    let row = |name: &str, key: &str, t: f64| {
        if want.is_empty() || want == key {
            println!("| {name} | {t:.1} | {:.1} |", t * 1e6 / count as f64);
        }
    };
    let mut buf = String::new();
    if want.is_empty() || want == "f64-std" {
        row(
            "f64 to text, std `Display`",
            "f64-std",
            ms(5, || {
                let mut total = 0usize;
                for f in &floats {
                    buf.clear();
                    use std::fmt::Write as _;
                    write!(buf, "{f}").unwrap();
                    total += buf.len();
                }
                total
            }),
        );
    }
    if want.is_empty() || want == "f64-ryu" {
        row(
            "f64 to text, `ryu`",
            "f64-ryu",
            ms(5, || {
                let mut b = ryu::Buffer::new();
                let mut total = 0usize;
                for f in &floats {
                    total += b.format(*f).len();
                }
                total
            }),
        );
    }
    if want.is_empty() || want == "u64-std" {
        row(
            "u64 to text, std `Display`",
            "u64-std",
            ms(5, || {
                let mut total = 0usize;
                for v in &ints {
                    buf.clear();
                    use std::fmt::Write as _;
                    write!(buf, "{v}").unwrap();
                    total += buf.len();
                }
                total
            }),
        );
    }
    if want.is_empty() || want == "u64-itoa" {
        row(
            "u64 to text, `itoa`",
            "u64-itoa",
            ms(5, || {
                let mut b = itoa::Buffer::new();
                let mut total = 0usize;
                for v in &ints {
                    total += b.format(*v).len();
                }
                total
            }),
        );
    }
    if want.is_empty() || want == "f64-parse" {
        row(
            "text to f64, std `parse`",
            "f64-parse",
            ms(5, || {
                let mut total = 0.0f64;
                for t in &texts {
                    total += t.parse::<f64>().unwrap();
                }
                total
            }),
        );
    }
    if want.is_empty() || want == "text-kept" {
        row(
            "text kept as it arrived (the term's `Atom::Num`: one copy)",
            "text-kept",
            ms(5, || {
                let mut total = 0usize;
                for t in &texts {
                    total += t.clone().len();
                }
                total
            }),
        );
    }
}

/// Parse speed of every text surface on the same records, with serde_json as the JSON reference.
/// Each surface is timed from the same rendered document; run one surface per process if you need
/// to rule out heap-layout effects between surfaces (`cargo run --release -- parse json`, and `parse serde` for the serde_json reference alone; a third argument sets the record count). With no surface it does exactly that for you, one child process per surface.
fn parse_surfaces(n: usize, only: Option<&str>) {
    // No surface given: run each one in its own process and gather the rows, because the heap state
    // one parse leaves behind changes the speed of the next by 10 to 20%.
    let Some(only) = only else {
        println!("\n### Parsing {n} records into a generic tree (one process per surface)\n");
        println!("| surface | size (bytes) | parse (ms) | MB/s |");
        println!("|---|---:|---:|---:|");
        let exe = std::env::current_exe().expect("own path");
        for name in [
            "json", "json-ref", "serde", "yaml", "yaml-ref", "toon", "poml", "csv", "baml",
        ] {
            let out = std::process::Command::new(&exe)
                .args(["parse", name, &n.to_string()])
                .output()
                .expect("child run");
            assert!(out.status.success(), "{name} child failed");
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.starts_with("| ") && !line.starts_with("| surface") {
                    println!("{line}");
                }
            }
        }
        return;
    };
    let serde_only = only.eq_ignore_ascii_case("serde");
    let json = {
        let data = ordered(n);
        String::from_utf8(serde_json::to_vec(&data).unwrap()).unwrap()
    };
    let text_for = |sf: Surface| {
        let term = parse_surface(Surface::Json, &json).unwrap();
        wirefold::codec::syntax::render_surface(&term, sf)
    };
    println!("\n### Parsing {n} records into a generic tree\n");
    println!("| surface | size (bytes) | parse (ms) | MB/s |");
    println!("|---|---:|---:|---:|");
    if !serde_only {
        for (name, sf, borrowed) in [
            ("JSON", Surface::Json, false),
            ("JSON (borrowed)", Surface::Json, true),
            ("YAML", Surface::Yaml, false),
            ("YAML (borrowed)", Surface::Yaml, true),
            ("TOON", Surface::Toon, false),
            ("POML", Surface::Poml, false),
            ("CSV", Surface::Csv, false),
            ("BAML", Surface::Baml, false),
        ] {
            // `json-ref` selects the borrowed JSON row, `json` the owned one, and so on.
            let selector = name.to_lowercase().replace(" (borrowed)", "-ref");
            if selector != only.to_lowercase() {
                continue;
            }
            let text = text_for(sf);
            let t = if borrowed {
                assert!(
                    wirefold::codec::syntax::parse_surface_ref(sf, &text).is_ok(),
                    "{name} parses its own render"
                );
                ms(15, || {
                    wirefold::codec::syntax::parse_surface_ref(sf, &text).unwrap()
                })
            } else {
                assert!(
                    parse_surface(sf, &text).is_ok(),
                    "{name} parses its own render"
                );
                ms(15, || parse_surface(sf, &text).unwrap())
            };
            println!(
                "| {name} | {} | {:.1} | {:.0} |",
                text.len(),
                t,
                text.len() as f64 / 1e6 / (t / 1e3)
            );
        }
    }
    if only.eq_ignore_ascii_case("serde") {
        let t = ms(15, || {
            serde_json::from_str::<serde_json::Value>(&json).unwrap()
        });
        println!(
            "| JSON, serde_json `Value` (reference) | {} | {:.1} | {:.0} |",
            json.len(),
            t,
            json.len() as f64 / 1e6 / (t / 1e3)
        );
    }
}

/// Run each of `names` as `<mode> <name> <n>` in its own child process and relay the table rows, so
/// no measurement inherits another's heap state.
fn run_children(mode: &str, names: &[&str], n: usize) {
    let exe = std::env::current_exe().expect("own path");
    for name in names {
        let out = std::process::Command::new(&exe)
            .args([mode, name, &n.to_string()])
            .output()
            .expect("child run");
        assert!(out.status.success(), "{mode} {name} child failed");
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.starts_with("| ")
                && !line.starts_with("| surface")
                && !line.starts_with("| operation")
            {
                println!("{line}");
            }
        }
    }
}

/// Render speed of every text surface from the same term.
fn render_surfaces(n: usize, only: Option<&str>) {
    let Some(only) = only else {
        println!("\n### Rendering {n} records from a generic tree (one process per surface)\n");
        println!("| surface | size (bytes) | render (ms) | MB/s |");
        println!("|---|---:|---:|---:|");
        run_children(
            "render",
            &["json", "yaml", "toon", "poml", "csv", "baml"],
            n,
        );
        return;
    };
    let term = {
        let data = ordered(n);
        let json = String::from_utf8(serde_json::to_vec(&data).unwrap()).unwrap();
        parse_surface(Surface::Json, &json).unwrap()
    };
    for (name, sf) in [
        ("JSON", Surface::Json),
        ("YAML", Surface::Yaml),
        ("TOON", Surface::Toon),
        ("POML", Surface::Poml),
        ("CSV", Surface::Csv),
        ("BAML", Surface::Baml),
    ] {
        if !name.eq_ignore_ascii_case(only) {
            continue;
        }
        let size = wirefold::codec::syntax::render_surface(&term, sf).len();
        let t = ms(15, || wirefold::codec::syntax::render_surface(&term, sf));
        println!(
            "| {name} | {size} | {t:.2} | {:.0} |",
            size as f64 / 1e6 / (t / 1e3)
        );
    }
}

/// MessagePack and protobuf encode and decode of the same records as a generic tree. rmp-serde
/// (through `serde_json::Value`, also a generic tree) is the MessagePack reference.
fn binary_codecs(n: usize, only: Option<&str>) {
    let Some(only) = only else {
        println!("\n### Binary codecs, {n} records as a generic tree (one process per row)\n");
        println!("| operation | size (bytes) | time (ms) | MB/s |");
        println!("|---|---:|---:|---:|");
        run_children(
            "binary",
            &[
                "wf-msgpack-encode",
                "rmp-msgpack-encode",
                "wf-msgpack-decode",
                "rmp-msgpack-decode",
                "wf-proto-encode",
                "wf-proto-decode",
            ],
            n,
        );
        return;
    };
    use wirefold::codec::proto::{ProtoRegistry, ProtoSchema};
    use wirefold::codec::syntax::{Atom, SyntaxTerm};
    // The protobuf schema keeps the integer, bool and string fields (this codec models no floats).
    const SRC: &str = "message Event {\n  optional uint64 ts = 1;\n  optional uint32 user = 2;\n  optional uint32 kind = 3;\n  optional bool ok = 5;\n  optional string name = 6;\n}\nmessage Batch {\n  repeated Event events = 1;\n}\n";
    let (term, value, batch, wf_mp, rmp, pb) = {
        let data = ordered(n);
        let json = serde_json::to_vec(&data).unwrap();
        let term = parse_surface(Surface::Json, std::str::from_utf8(&json).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let reg = ProtoRegistry::parse_messages(SRC, &["Event", "Batch"]);
        let schema = ProtoSchema::parse(SRC, "Batch").unwrap();
        let events: Vec<SyntaxTerm> = data
            .iter()
            .map(|e| {
                SyntaxTerm::Bu(vec![
                    ("ts".into(), SyntaxTerm::B(Atom::Num(e.ts.to_string()))),
                    ("user".into(), SyntaxTerm::B(Atom::Num(e.user.to_string()))),
                    ("kind".into(), SyntaxTerm::B(Atom::Num(e.kind.to_string()))),
                    ("ok".into(), SyntaxTerm::B(Atom::Bool(e.ok))),
                    ("name".into(), SyntaxTerm::B(Atom::Str(e.name.clone()))),
                ])
            })
            .collect();
        let batch = SyntaxTerm::Bu(vec![("events".into(), SyntaxTerm::Fourth(events))]);
        let wf_mp = wirefold::codec::msgpack::encode(&term);
        let rmp = rmp_serde::to_vec(&value).unwrap();
        let pb = wirefold::codec::proto::encode(&batch, &schema, &reg);
        (term, value, batch, wf_mp, rmp, pb)
    };
    let reg = ProtoRegistry::parse_messages(SRC, &["Event", "Batch"]);
    let schema = ProtoSchema::parse(SRC, "Batch").unwrap();
    let (label, size, t) = match only {
        "wf-msgpack-encode" => (
            "wirefold msgpack encode",
            wf_mp.len(),
            ms(15, || wirefold::codec::msgpack::encode(&term)),
        ),
        "rmp-msgpack-encode" => (
            "rmp-serde encode (`Value`)",
            rmp.len(),
            ms(15, || rmp_serde::to_vec(&value).unwrap()),
        ),
        "wf-msgpack-decode" => (
            "wirefold msgpack decode",
            wf_mp.len(),
            ms(15, || wirefold::codec::msgpack::decode(&wf_mp).unwrap()),
        ),
        "rmp-msgpack-decode" => (
            "rmp-serde decode (`Value`)",
            rmp.len(),
            ms(15, || {
                rmp_serde::from_slice::<serde_json::Value>(&rmp).unwrap()
            }),
        ),
        "wf-proto-encode" => (
            "wirefold protobuf encode",
            pb.len(),
            ms(15, || wirefold::codec::proto::encode(&batch, &schema, &reg)),
        ),
        "wf-proto-decode" => (
            "wirefold protobuf decode",
            pb.len(),
            ms(15, || {
                wirefold::codec::proto::decode(&pb, &schema, &reg).unwrap()
            }),
        ),
        other => panic!("unknown binary row {other}"),
    };
    println!(
        "| {label} | {size} | {t:.2} | {:.0} |",
        size as f64 / 1e6 / (t / 1e3)
    );
}

fn main() {
    if let Some(mode) = std::env::args()
        .nth(1)
        .filter(|m| m == "render" || m == "binary")
    {
        let only = std::env::args().nth(2);
        let n = std::env::args()
            .nth(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(20_000);
        if mode == "render" {
            render_surfaces(n, only.as_deref());
        } else {
            binary_codecs(n, only.as_deref());
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("parse") {
        let only = std::env::args().nth(2);
        let n = std::env::args()
            .nth(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(20_000);
        parse_surfaces(n, only.as_deref());
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    // `pipeline <dataset> <n> <serializer> <codec>` and `column <dataset> <n> <row>` are single measured
    // rows, run as children of the comparison.
    if args.get(1).map(String::as_str) == Some("pipeline") && args.len() == 6 {
        pipeline_row(&args[2], args[3].parse().unwrap(), &args[4], &args[5]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("column") && args.len() == 5 {
        column_row(&args[2], args[3].parse().unwrap(), &args[4]);
        return;
    }
    if args.get(1).map(String::as_str) == Some("bf16") {
        bf16_report(
            args.get(2)
                .and_then(|v| v.parse().ok())
                .unwrap_or(4_000_000),
        );
        return;
    }
    for (mode, report) in [
        ("planes", plane_report as fn(&str, usize)),
        ("levels", level_report),
        ("blocks", block_report),
    ] {
        if args.get(1).map(String::as_str) == Some(mode) {
            let dataset = args.get(2).map_or("ordered", String::as_str);
            report(
                dataset,
                args.get(3).and_then(|v| v.parse().ok()).unwrap_or(20_000),
            );
            return;
        }
    }
    if args.get(1).map(String::as_str) == Some("numbers") {
        number_formatting(args.get(2).map(String::as_str));
        return;
    }
    // default: the serialization comparison. `[records] [runs]`.
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    serialize_tables(n, runs);
}
