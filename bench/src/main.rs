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
                ok: r.next() % 10 != 0,
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
/// to rule out heap-layout effects between surfaces (`cargo run --release -- parse json`, and `parse serde` for the serde_json reference alone; a third argument sets the record count).
fn parse_surfaces(n: usize, only: Option<&str>) {
    let data = ordered(n);
    let json = String::from_utf8(serde_json::to_vec(&data).unwrap()).unwrap();
    let serde_only = only.is_some_and(|o| o.eq_ignore_ascii_case("serde"));
    let term = if serde_only {
        wirefold::codec::syntax::SyntaxTerm::Nb
    } else {
        parse_surface(Surface::Json, &json).unwrap()
    };
    println!("\n### Parsing {n} records into a generic tree\n");
    println!("| surface | size (bytes) | parse (ms) | MB/s |");
    println!("|---|---:|---:|---:|");
    for (name, sf) in [
        ("JSON", Surface::Json),
        ("YAML", Surface::Yaml),
        ("TOON", Surface::Toon),
        ("POML", Surface::Poml),
        ("CSV", Surface::Csv),
        ("BAML", Surface::Baml),
    ] {
        if only.is_some_and(|o| !name.eq_ignore_ascii_case(o)) || serde_only {
            continue;
        }
        let text = wirefold::codec::syntax::render_surface(&term, sf);
        assert!(
            parse_surface(sf, &text).is_ok(),
            "{name} parses its own render"
        );
        let t = ms(15, || parse_surface(sf, &text).unwrap());
        println!(
            "| {name} | {} | {:.1} | {:.0} |",
            text.len(),
            t,
            text.len() as f64 / 1e6 / (t / 1e3)
        );
    }
    if only.is_none_or(|o| o.eq_ignore_ascii_case("json") || o.eq_ignore_ascii_case("serde")) {
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

fn main() {
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
