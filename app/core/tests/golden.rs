//! Parity with the Swift app: fixtures produced by scripts/golden.sh from the original sources.
//! `records.private.jsonl` (real utterances, gitignored) is used when present.
use plaid_core::{actions, diff, hotwords::Snapshot, phonetic, records, volc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn pairs(v: Vec<(String, String)>) -> Value {
    json!(v.into_iter().map(|(a, b)| [a, b]).collect::<Vec<_>>())
}

fn eval(f: &str, i: &Value) -> Value {
    match f {
        "phonetic" => json!(phonetic::apply(
            s(&i["text"]),
            &Snapshot::new(&strs(&i["lines"]), 0.85).entries,
            0.85
        )),
        "snapshot.apply" => json!(
            Snapshot::new(&strs(&i["lines"]), i["threshold"].as_f64().unwrap())
                .apply(s(&i["text"]))
        ),
        "snapshot.terms" => json!(Snapshot::new(&strs(&i["lines"]), 0.85).terms),
        "snapshot.context" => {
            json!(Snapshot::new(&strs(&i["lines"]), 0.85).context(&strs(&i["dialog"])))
        }
        "tidy" => json!(volc::tidy(s(i))),
        "chars" => json!(records::chars(s(i))),
        "replacements" => pairs(diff::replacements(s(&i[0]), s(&i[1]))),
        "overlap" => json!(diff::overlap(s(&i[0]), s(&i[1]))),
        "insertedSpan" => diff::inserted_span(s(&i[0]), s(&i[1]))
            .map_or(Value::Null, |(a, b, t)| json!([a, b, t])),
        "editedSpan" => json!(diff::edited_span(
            s(&i[0]),
            i[1].as_u64().unwrap() as usize,
            i[2].as_u64().unwrap() as usize,
            s(&i[3])
        )),
        "rules" => pairs(records::rules(
            s(&i["before"]),
            s(&i["after"]),
            &strs(&i["terms"]),
        )),
        "actions.parse" => {
            let list: Vec<actions::Action> = serde_json::from_value(i["actions"].clone()).unwrap();
            match actions::parse(s(&i["text"]), &list, s(&i["wake"])) {
                actions::Parsed::Dictation => json!(["dictation"]),
                actions::Parsed::Unknown(r) => json!(["unknown", r]),
                actions::Parsed::Run(a, args) => json!(["run", a.name, args]),
            }
        }
        "volc.frame" => json!(hex(&volc::frame(
            i["type"].as_u64().unwrap() as u8,
            i["last"].as_bool().unwrap(),
            i["json"].as_bool().unwrap(),
            &unhex(s(&i["payload"]))
        ))),
        "volc.parse" => volc::parse(&unhex(s(i))).map_or(
            Value::Null,
            |r| json!({"text": r.text, "last": r.last, "error": r.error}),
        ),
        _ => panic!("unknown fn {f}"),
    }
}

/// Swift and Rust format doubles differently; compare numbers by value.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-9
        }
        _ => a == b,
    }
}

/// fn → (total, failures). Prints up to `show` failures per fn.
fn run(path: &str, show: usize) -> BTreeMap<String, (usize, usize)> {
    let body = std::fs::read_to_string(path).unwrap();
    let mut stats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for line in body.lines().filter(|l| !l.is_empty()) {
        let v: Value = serde_json::from_str(line).unwrap();
        let f = s(&v["fn"]).to_owned();
        let got = eval(&f, &v["in"]);
        let e = stats.entry(f.clone()).or_default();
        e.0 += 1;
        if !same(&got, &v["out"]) {
            e.1 += 1;
            if e.1 <= show {
                eprintln!(
                    "MISMATCH {f}\n  in:   {}\n  want: {}\n  got:  {got}",
                    v["in"], v["out"]
                );
            }
        }
    }
    for (f, (n, bad)) in &stats {
        eprintln!("{path}: {f}: {}/{n} ok", n - bad);
    }
    stats
}

/// Functions whose Swift version used NLTokenizer; ICU's dictionary segmenter splits some words differently.
const TOKENIZER: [&str; 1] = ["replacements"];

fn check(path: &str, tolerance: f64) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");
    let path = format!("{dir}{path}");
    if !std::path::Path::new(&path).exists() {
        eprintln!("skip {path}");
        return;
    }
    let stats = run(&path, 5);
    for (f, (n, bad)) in stats {
        let allowed = if TOKENIZER.contains(&f.as_str()) {
            (n as f64 * tolerance).floor() as usize
        } else {
            0
        };
        assert!(
            bad <= allowed,
            "{f}: {bad}/{n} mismatches (allowed {allowed})"
        );
    }
}

#[test]
fn synthetic() {
    check("synthetic.jsonl", 0.0);
}

#[test]
fn private_records() {
    check("records.private.jsonl", 0.05);
}
