//! wirefold against serde_json, bincode, postcard, rmp-serde and rkyv on the same records.
//!
//! Run: `cargo run --release -- [records] [runs]` (default 20000 records, 3 runs). Prints markdown tables.
//! Every encoder is round-trip checked before it is timed. Each row and operation runs in its own
//! process, `runs` times, and is reported as mean (minimum to maximum); a run is the median of 9
//! repetitions (fewer for zstd, one for the context-mixing coder, which takes seconds).

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
    if std::env::var("WARMCURVE").is_ok() {
        let v: Vec<f64> = (0..runs).map(|_| { let s = Instant::now(); black_box(f()); s.elapsed().as_secs_f64() * 1e3 }).collect();
        eprintln!("per-run ms: {}", v.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(" "));
    }
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

/// One serialization row, measured in this process: prints `| name | size | ms |`.
///
/// `row` picks the format and `op` is `encode`, `decode` or (for the `col-*` rows) `read`. The input
/// is built, encoded and round-trip checked before the timer starts.
fn serialize_row(dataset: &str, n: usize, row: &str, op: &str) {
    use wirefold::codec::web::CollapseElem as _;
    let data: &'static Vec<Event> = Box::leak(Box::new(dataset_by_name(dataset, n)));
    let packed = Event::collapse_pack(data);
    let wf: Vec<u8> = packed.as_bytes().to_vec();
    let want: f64 = data.iter().map(|e| e.value).sum();
    let runs = 9;
    // (display name, encoded size, milliseconds)
    let (name, size, t): (&str, usize, f64) = match (row, op) {
        ("collapse", _) => {
            assert!(EventView::collapse_open(&wf).unwrap().collapse_to_vec() == *data);
            let t = if op == "encode" {
                ms(runs, || Event::collapse_pack(data))
            } else {
                ms(runs, || {
                    EventView::collapse_open(&wf).unwrap().collapse_to_vec()
                })
            };
            ("wirefold Collapse", wf.len(), t)
        }
        ("collapse-cm", _) => {
            let z = wirefold::codec::compress(&wf);
            assert!(wirefold::codec::decompress(&z) == wf);
            let t = if op == "encode" {
                ms(1, || wirefold::codec::compress(&wf))
            } else {
                ms(1, || wirefold::codec::decompress(&z))
            };
            ("wirefold Collapse + cm coder", z.len(), t)
        }
        ("collapse-zstd", _) => {
            let z = zstd::encode_all(&wf[..], 19).unwrap();
            assert!(zstd::decode_all(&z[..]).unwrap() == wf);
            let t = if op == "encode" {
                ms(3, || zstd::encode_all(&wf[..], 19).unwrap())
            } else {
                ms(runs, || zstd::decode_all(&z[..]).unwrap())
            };
            ("wirefold Collapse + zstd -19", z.len(), t)
        }
        ("bincode", _) => {
            let b = bincode::serialize(data).unwrap();
            assert!(bincode::deserialize::<Vec<Event>>(&b).unwrap() == *data);
            let t = if op == "encode" {
                ms(runs, || bincode::serialize(data).unwrap())
            } else {
                ms(runs, || bincode::deserialize::<Vec<Event>>(&b).unwrap())
            };
            ("bincode 1.3", b.len(), t)
        }
        ("postcard", _) => {
            let b = postcard::to_stdvec(data).unwrap();
            assert!(postcard::from_bytes::<Vec<Event>>(&b).unwrap() == *data);
            let t = if op == "encode" {
                ms(runs, || postcard::to_stdvec(data).unwrap())
            } else {
                ms(runs, || postcard::from_bytes::<Vec<Event>>(&b).unwrap())
            };
            ("postcard", b.len(), t)
        }
        ("postcard-zstd", _) => {
            let b = postcard::to_stdvec(data).unwrap();
            let z = zstd::encode_all(&b[..], 19).unwrap();
            assert!(zstd::decode_all(&z[..]).unwrap() == b);
            let t = if op == "encode" {
                ms(3, || zstd::encode_all(&b[..], 19).unwrap())
            } else {
                ms(runs, || zstd::decode_all(&z[..]).unwrap())
            };
            ("postcard + zstd -19", z.len(), t)
        }
        ("rmp", _) => {
            let b = rmp_serde::to_vec(data).unwrap();
            assert!(rmp_serde::from_slice::<Vec<Event>>(&b).unwrap() == *data);
            let t = if op == "encode" {
                ms(runs, || rmp_serde::to_vec(data).unwrap())
            } else {
                ms(runs, || rmp_serde::from_slice::<Vec<Event>>(&b).unwrap())
            };
            ("rmp-serde (MessagePack)", b.len(), t)
        }
        ("rkyv", _) => {
            let b = rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap();
            assert!(rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&b).unwrap() == *data);
            let t = if op == "encode" {
                ms(runs, || {
                    rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap()
                })
            } else {
                ms(runs, || {
                    rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&b).unwrap()
                })
            };
            ("rkyv 0.8 (validated)", b.len(), t)
        }
        ("json", _) => {
            let b = serde_json::to_vec(data).unwrap();
            assert!(serde_json::from_slice::<Vec<Event>>(&b).unwrap() == *data);
            let t = if op == "encode" {
                ms(runs, || serde_json::to_vec(data).unwrap())
            } else {
                ms(runs, || serde_json::from_slice::<Vec<Event>>(&b).unwrap())
            };
            ("serde_json", b.len(), t)
        }
        // Reading one f64 column without decoding the rest.
        ("col-collapse", _) => {
            let sum = |v: &EventView| (0..v.collapse_len()).map(|i| v.value(i)).sum::<f64>();
            assert_eq!(sum(&EventView::collapse_open(&wf).unwrap()), want);
            let t = ms(runs, || sum(&EventView::collapse_open(&wf).unwrap()));
            ("wirefold view (open + column read)", wf.len(), t)
        }
        ("col-rkyv", _) => {
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
        ("col-bincode", _) => {
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
        ("col-postcard", _) => {
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
        other => panic!("unknown serialization row {other:?}"),
    };
    println!("| {name} | {size} | {t:.3} |");
}

/// Mean, minimum and maximum of a set of timings.
fn spread(v: &[f64]) -> (f64, f64, f64) {
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let min = v.iter().copied().fold(f64::INFINITY, f64::min);
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (mean, min, max)
}

/// Run one serialization row `runs` times, each in its own process; returns name, size and timings.
fn collect_row(
    dataset: &str,
    n: usize,
    row: &str,
    op: &str,
    runs: usize,
) -> (String, usize, Vec<f64>) {
    let exe = std::env::current_exe().expect("own path");
    let (mut name, mut size, mut times) = (String::new(), 0usize, Vec::new());
    for _ in 0..runs {
        let out = std::process::Command::new(&exe)
            .args(["serialize", dataset, &n.to_string(), row, op])
            .output()
            .expect("child run");
        assert!(out.status.success(), "{row} {op} child failed");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let line = text
            .lines()
            .find(|l| l.starts_with("| "))
            .expect("a result row");
        let cols: Vec<&str> = line.split('|').map(str::trim).collect();
        name = cols[1].to_string();
        size = cols[2].parse().unwrap();
        times.push(cols[3].parse().unwrap());
    }
    (name, size, times)
}

/// The serialization comparison: every format on the same records, each row and operation in its own
/// process, repeated `runs` times, reported as mean (minimum to maximum).
fn serialize_tables(n: usize, runs: usize) {
    println!(
        "wirefold {} against serde_json 1, bincode 1.3, postcard 1, rmp-serde 1, rkyv 0.8 and zstd; {n} records, {runs} runs per cell",
        env!("CARGO_PKG_VERSION")
    );
    let fmt = |v: &[f64]| {
        let (m, lo, hi) = spread(v);
        format!("{m:.2} ({lo:.2} to {hi:.2})")
    };
    for (title, dataset) in [
        ("Telemetry-shaped records", "ordered"),
        ("Random records (worst case for a columnar model)", "random"),
    ] {
        println!("\n### {title} ({n} records)\n");
        println!("| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |");
        println!("|---|---:|---:|---:|---:|");
        for row in [
            "collapse",
            "collapse-cm",
            "collapse-zstd",
            "bincode",
            "postcard",
            "postcard-zstd",
            "rmp",
            "rkyv",
            "json",
        ] {
            // the context-mixing coder takes seconds per run: one run per cell
            let r = if row == "collapse-cm" { 1 } else { runs };
            let (name, size, enc) = collect_row(dataset, n, row, "encode", r);
            let (_, _, dec) = collect_row(dataset, n, row, "decode", r);
            println!(
                "| {name} | {size} | {:.1} | {} | {} |",
                size as f64 / n as f64,
                fmt(&enc),
                fmt(&dec)
            );
        }
        println!("\nSum one `f64` column of {n} records (open + read, no full decode):\n");
        println!("| format | time (ms) |");
        println!("|---|---:|");
        for row in ["col-collapse", "col-rkyv", "col-bincode", "col-postcard"] {
            let (name, _, t) = collect_row(dataset, n, row, "read", runs);
            println!("| {name} | {} |", fmt(&t));
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
    // `serialize <dataset> <n> <row> <op>` is one measured row (run by `serialize_tables` as a child).
    if args.get(1).map(String::as_str) == Some("serialize") && args.len() == 6 {
        serialize_row(&args[2], args[3].parse().unwrap(), &args[4], &args[5]);
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
