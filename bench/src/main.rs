//! wirefold against serde_json, bincode, postcard, rmp-serde and rkyv on the same records.
//!
//! Run: `cargo run --release -- [records]` (default 100000). Prints markdown tables.
//! Every encoder is round-trip checked before it is timed. Timings are the median of 9 runs (one
//! run for the context-mixing coder, which takes tens of seconds).

use std::hint::black_box;
use std::time::Instant;
use wirefold::codec::syntax::{parse_surface, Surface};
use wirefold::codec::web::CollapseElem;

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

struct Row {
    name: &'static str,
    bytes: usize,
    enc: f64,
    dec: f64,
}

fn report(title: &str, n: usize, rows: &[Row]) {
    println!("\n### {title} ({n} records)\n");
    println!("| format | size (bytes) | bytes/record | encode (ms) | decode all (ms) |");
    println!("|---|---:|---:|---:|---:|");
    for r in rows {
        println!(
            "| {} | {} | {:.1} | {:.2} | {:.2} |",
            r.name,
            r.bytes,
            r.bytes as f64 / n as f64,
            r.enc,
            r.dec
        );
    }
}

fn dataset(title: &str, data: &Vec<Event>) {
    let n = data.len();
    let mut rows = Vec::new();

    // wirefold: columnar Collapse. Decode all = materialise every record from the view.
    let seed = Event::collapse_pack(data);
    let bytes = seed.as_bytes().to_vec();
    assert!(
        EventView::collapse_open(&bytes).unwrap().collapse_to_vec() == *data,
        "wirefold round trip"
    );
    rows.push(Row {
        name: "wirefold Collapse",
        bytes: bytes.len(),
        enc: ms(9, || Event::collapse_pack(data)),
        dec: ms(9, || {
            EventView::collapse_open(&bytes).unwrap().collapse_to_vec()
        }),
    });

    // wirefold Collapse + the context-mixing coder over the packed bytes.
    let z = wirefold::codec::compress(&bytes);
    assert_eq!(wirefold::codec::decompress(&z), bytes);
    rows.push(Row {
        name: "wirefold Collapse + cm coder",
        bytes: z.len(),
        enc: ms(1, || wirefold::codec::compress(&bytes)),
        dec: ms(1, || wirefold::codec::decompress(&z)),
    });

    // zstd -19 over the same bytes: the usual "serialize, then compress" competitor to the cm coder.
    let zw = zstd::encode_all(&bytes[..], 19).unwrap();
    assert!(
        zstd::decode_all(&zw[..]).unwrap() == bytes,
        "zstd round trip"
    );
    rows.push(Row {
        name: "wirefold Collapse + zstd -19",
        bytes: zw.len(),
        enc: ms(3, || zstd::encode_all(&bytes[..], 19).unwrap()),
        dec: ms(9, || zstd::decode_all(&zw[..]).unwrap()),
    });

    let b = bincode::serialize(data).unwrap();
    assert!(
        bincode::deserialize::<Vec<Event>>(&b).unwrap() == *data,
        "bincode round trip"
    );
    rows.push(Row {
        name: "bincode 1.3",
        bytes: b.len(),
        enc: ms(9, || bincode::serialize(data).unwrap()),
        dec: ms(9, || bincode::deserialize::<Vec<Event>>(&b).unwrap()),
    });

    let p = postcard::to_stdvec(data).unwrap();
    assert!(
        postcard::from_bytes::<Vec<Event>>(&p).unwrap() == *data,
        "postcard round trip"
    );
    rows.push(Row {
        name: "postcard",
        bytes: p.len(),
        enc: ms(9, || postcard::to_stdvec(data).unwrap()),
        dec: ms(9, || postcard::from_bytes::<Vec<Event>>(&p).unwrap()),
    });

    let pz = zstd::encode_all(&p[..], 19).unwrap();
    assert!(
        zstd::decode_all(&pz[..]).unwrap() == p,
        "postcard+zstd round trip"
    );
    rows.push(Row {
        name: "postcard + zstd -19",
        bytes: pz.len(),
        enc: ms(3, || zstd::encode_all(&p[..], 19).unwrap()),
        dec: ms(9, || zstd::decode_all(&pz[..]).unwrap()),
    });

    let m = rmp_serde::to_vec(data).unwrap();
    assert!(
        rmp_serde::from_slice::<Vec<Event>>(&m).unwrap() == *data,
        "rmp round trip"
    );
    rows.push(Row {
        name: "rmp-serde (MessagePack)",
        bytes: m.len(),
        enc: ms(9, || rmp_serde::to_vec(data).unwrap()),
        dec: ms(9, || rmp_serde::from_slice::<Vec<Event>>(&m).unwrap()),
    });

    let r = rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap();
    assert!(
        rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&r).unwrap() == *data,
        "rkyv round trip"
    );
    rows.push(Row {
        name: "rkyv 0.8 (validated)",
        bytes: r.len(),
        enc: ms(9, || rkyv::to_bytes::<rkyv::rancor::Error>(data).unwrap()),
        dec: ms(9, || {
            rkyv::from_bytes::<Vec<Event>, rkyv::rancor::Error>(&r).unwrap()
        }),
    });

    let j = serde_json::to_vec(data).unwrap();
    assert!(
        serde_json::from_slice::<Vec<Event>>(&j).unwrap() == *data,
        "serde_json round trip"
    );
    rows.push(Row {
        name: "serde_json",
        bytes: j.len(),
        enc: ms(9, || serde_json::to_vec(data).unwrap()),
        dec: ms(9, || serde_json::from_slice::<Vec<Event>>(&j).unwrap()),
    });

    report(title, n, &rows);

    // Reading one column without decoding the rest. Wirefold and rkyv read in place; the serde
    // formats have to decode every record first.
    let want: f64 = data.iter().map(|e| e.value).sum();
    let view = EventView::collapse_open(&bytes).unwrap();
    let sum_wf = |v: &EventView| (0..v.collapse_len()).map(|i| v.value(i)).sum::<f64>();
    assert_eq!(sum_wf(&view), want);
    let arch =
        rkyv::access::<rkyv::vec::ArchivedVec<ArchivedEvent>, rkyv::rancor::Error>(&r).unwrap();
    let sum_rk = |a: &rkyv::vec::ArchivedVec<ArchivedEvent>| {
        a.iter().map(|e| f64::from(e.value)).sum::<f64>()
    };
    assert_eq!(sum_rk(arch), want);
    println!("\nSum one `f64` column of {n} records (open + read, no full decode):\n");
    println!("| format | time (ms) |");
    println!("|---|---:|");
    println!(
        "| wirefold view (open + column read) | {:.3} |",
        ms(9, || {
            let v = EventView::collapse_open(&bytes).unwrap();
            sum_wf(&v)
        })
    );
    println!(
        "| rkyv 0.8 (validate + read) | {:.3} |",
        ms(9, || {
            let a = rkyv::access::<rkyv::vec::ArchivedVec<ArchivedEvent>, rkyv::rancor::Error>(&r)
                .unwrap();
            sum_rk(a)
        })
    );
    println!(
        "| bincode (decode all, then sum) | {:.3} |",
        ms(9, || bincode::deserialize::<Vec<Event>>(&b)
            .unwrap()
            .iter()
            .map(|e| e.value)
            .sum::<f64>())
    );
    println!(
        "| postcard (decode all, then sum) | {:.3} |",
        ms(9, || postcard::from_bytes::<Vec<Event>>(&p)
            .unwrap()
            .iter()
            .map(|e| e.value)
            .sum::<f64>())
    );
}

/// JSON text into a generic tree: wirefold's syntax surface against serde_json::Value.
fn json_values(data: &[Event]) {
    let text = String::from_utf8(serde_json::to_vec(data).unwrap()).unwrap();
    assert!(parse_surface(Surface::Json, &text).is_ok());
    let wf = ms(9, || parse_surface(Surface::Json, &text).unwrap());
    let sj = ms(9, || {
        serde_json::from_str::<serde_json::Value>(&text).unwrap()
    });
    println!(
        "\n### JSON text into a generic tree ({} records, {} bytes)\n",
        data.len(),
        text.len()
    );
    println!("| parser | time (ms) | MB/s |");
    println!("|---|---:|---:|");
    let mbs = |t: f64| text.len() as f64 / 1e6 / (t / 1e3);
    println!(
        "| wirefold `parse_surface(Json)` | {:.2} | {:.0} |",
        wf,
        mbs(wf)
    );
    println!("| serde_json `Value` | {:.2} | {:.0} |", sj, mbs(sj));
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
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    println!(
        "wirefold {} vs serde_json 1, bincode 1.3, postcard 1, rmp-serde 1, rkyv 0.8; rustc {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("RUSTC_VERSION").unwrap_or("see `rustc -V`")
    );
    dataset("Telemetry-shaped records", &ordered(n));
    dataset(
        "Random records (worst case for a columnar model)",
        &random(n),
    );
    json_values(&ordered(n));
}
