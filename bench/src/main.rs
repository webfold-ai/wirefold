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
            ("wirefold view (open + column read)", wf.len(), t)
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
        "col-collapse-cols-zstd3" | "col-collapse-colshuf-zstd3" => {
            let transform = row == "col-collapse-colshuf-zstd3";
            let z = columns_compress(&wf, 3, transform);
            assert_eq!(columns_value_sum(&z, transform), want);
            let t = ms(runs, || columns_value_sum(&z, transform));
            (
                if transform {
                    "columns, shuffle, zstd -3: decompress one column"
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

// ── Columns. The `Event` buffer is a section table of two sections: the scalar fields as one array of
// packed 22-byte records (row-major), then the strings as a ragged column. These helpers split it into
// one plane per field (a true column store), compress each plane with zstd (zstd stays in this crate),
// optionally byte-shuffling the multi-byte planes, and rebuild the exact buffer. Reading one field then
// decompresses only its plane. The field offsets are those of the packed record, checked by the round trip.

/// `(offset, width)` of each scalar field in the packed record: ts, user, kind, value, ok.
const FIELDS: [(usize, usize); 5] = [(0, 8), (8, 4), (12, 1), (13, 8), (21, 1)];
const RECORD: usize = 22;
/// Index of the `f64` field in [`FIELDS`].
const VALUE_PLANE: usize = 3;

fn le_u64(b: &[u8], at: usize) -> usize {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) as usize
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

/// The planes of a buffer, in frame order: the five scalar fields, the string lengths, the string bytes.
/// Multi-byte planes are byte-shuffled when `transform` is set.
fn planes(b: &[u8], transform: bool) -> Vec<Vec<u8>> {
    let (st0, st1) = (le_u64(b, 8), le_u64(b, 16));
    let count = le_u64(b, st0);
    let rec = &b[st0 + 8..st0 + 8 + count * RECORD];
    let mut out: Vec<Vec<u8>> = FIELDS
        .iter()
        .map(|&(off, w)| {
            let plane: Vec<u8> = (0..count)
                .flat_map(|i| rec[i * RECORD + off..i * RECORD + off + w].iter().copied())
                .collect();
            if transform {
                shuffle(&plane, w)
            } else {
                plane
            }
        })
        .collect();
    let n = le_u64(b, st1);
    let offs: Vec<usize> = (0..=n).map(|i| le_u64(b, st1 + 8 + 8 * i)).collect();
    let lens: Vec<u8> = (0..n)
        .flat_map(|i| ((offs[i + 1] - offs[i]) as u64).to_le_bytes())
        .collect();
    out.push(if transform { shuffle(&lens, 8) } else { lens });
    let text = st1 + 8 + 8 * (n + 1);
    out.push(b[text..text + offs[n]].to_vec());
    out
}

/// Compress a buffer plane by plane:
/// `[total len][raw section table][record count][string count][compressed length per plane][frames]`.
fn columns_compress(b: &[u8], level: i32, transform: bool) -> Vec<u8> {
    let (st0, st1) = (le_u64(b, 8), le_u64(b, 16));
    let frames: Vec<Vec<u8>> = planes(b, transform)
        .iter()
        .map(|p| zstd::encode_all(&p[..], level).unwrap())
        .collect();
    let mut out = Vec::new();
    out.extend((b.len() as u32).to_le_bytes());
    out.extend_from_slice(&b[..40]);
    out.extend((le_u64(b, st0) as u32).to_le_bytes());
    out.extend((le_u64(b, st1) as u32).to_le_bytes());
    for f in &frames {
        out.extend((f.len() as u32).to_le_bytes());
    }
    for f in &frames {
        out.extend_from_slice(f);
    }
    out
}

/// The parts of a plane-compressed buffer: total length, section table, counts and each frame.
fn columns_parse(z: &[u8]) -> (usize, &[u8], usize, usize, Vec<&[u8]>) {
    let u32_at = |at: usize| u32::from_le_bytes(z[at..at + 4].try_into().unwrap()) as usize;
    let nplanes = FIELDS.len() + 2;
    let mut at = 4 + 40 + 8 + 4 * nplanes;
    let frames = (0..nplanes)
        .map(|j| {
            let l = u32_at(4 + 40 + 8 + 4 * j);
            let f = &z[at..at + l];
            at += l;
            f
        })
        .collect();
    (u32_at(0), &z[4..44], u32_at(44), u32_at(48), frames)
}

/// Rebuild the exact buffer from its plane frames.
fn columns_decompress(z: &[u8], transform: bool) -> Vec<u8> {
    let (total, header, count, n, frames) = columns_parse(z);
    let plane = |j: usize, w: usize| {
        let d = zstd::decode_all(frames[j]).unwrap();
        if transform {
            unshuffle(&d, w)
        } else {
            d
        }
    };
    let (st0, st1) = (le_u64(header, 8), le_u64(header, 16));
    let mut out = vec![0u8; total];
    out[..40].copy_from_slice(header);
    out[st0..st0 + 8].copy_from_slice(&(count as u64).to_le_bytes());
    for (j, &(off, w)) in FIELDS.iter().enumerate() {
        let p = plane(j, w);
        for i in 0..count {
            let dst = st0 + 8 + i * RECORD + off;
            out[dst..dst + w].copy_from_slice(&p[i * w..(i + 1) * w]);
        }
    }
    let lens = plane(FIELDS.len(), 8);
    let text = zstd::decode_all(frames[FIELDS.len() + 1]).unwrap();
    out[st1..st1 + 8].copy_from_slice(&(n as u64).to_le_bytes());
    let mut off = 0u64;
    out[st1 + 8..st1 + 16].copy_from_slice(&off.to_le_bytes());
    for i in 0..n {
        off += u64::from_le_bytes(lens[8 * i..8 * i + 8].try_into().unwrap());
        out[st1 + 16 + 8 * i..st1 + 24 + 8 * i].copy_from_slice(&off.to_le_bytes());
    }
    let t = st1 + 8 + 8 * (n + 1);
    out[t..t + text.len()].copy_from_slice(&text);
    out
}

/// Sum of the `f64` field from a plane-compressed buffer, decompressing only that field's plane.
fn columns_value_sum(z: &[u8], transform: bool) -> f64 {
    let (_, _, _, _, frames) = columns_parse(z);
    let d = zstd::decode_all(frames[VALUE_PLANE]).unwrap();
    let d = if transform { unshuffle(&d, 8) } else { d };
    d.chunks_exact(8)
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
    /// A `Collapse` buffer compressed column by column with zstd, with or without the columnar transform.
    Columns {
        level: i32,
        transform: bool,
    },
}

impl Codec {
    fn parse(s: &str) -> Codec {
        match s {
            "raw" => Codec::Raw,
            "wirefold" => Codec::Wirefold,
            c if c.starts_with("colshuf") => Codec::Columns {
                level: c["colshuf".len()..].parse().expect("a level"),
                transform: true,
            },
            c if c.starts_with("cols") => Codec::Columns {
                level: c["cols".len()..].parse().expect("a level"),
                transform: false,
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
            Codec::Columns {
                level,
                transform: false,
            } => format!("+ columns, zstd -{level}"),
            Codec::Columns {
                level,
                transform: true,
            } => {
                format!("+ columns, shuffle, zstd -{level}")
            }
        }
    }

    fn compress(self, bytes: Vec<u8>) -> Vec<u8> {
        match self {
            Codec::Raw => bytes,
            Codec::Zstd(l) => zstd::encode_all(&bytes[..], l).unwrap(),
            Codec::Wirefold => wirefold::codec::compress(&bytes),
            Codec::Columns { level, transform } => columns_compress(&bytes, level, transform),
        }
    }

    fn decompress(self, z: &[u8]) -> Cow<'_, [u8]> {
        match self {
            Codec::Raw => Cow::Borrowed(z),
            Codec::Zstd(_) => Cow::Owned(zstd::decode_all(z).unwrap()),
            Codec::Wirefold => Cow::Owned(wirefold::codec::decompress(z)),
            Codec::Columns { transform, .. } => Cow::Owned(columns_decompress(z, transform)),
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
                    "colshuf1",
                    "colshuf3",
                    "colshuf6",
                    "colshuf19",
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
            "col-collapse",
            "col-rkyv",
            "col-bincode",
            "col-postcard",
            "col-collapse-whole-zstd3",
            "col-collapse-cols-zstd3",
            "col-collapse-colshuf-zstd3",
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
    if args.get(1).map(String::as_str) == Some("numbers") {
        number_formatting(args.get(2).map(String::as_str));
        return;
    }
    // default: the serialization comparison. `[records] [runs]`.
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    serialize_tables(n, runs);
}
