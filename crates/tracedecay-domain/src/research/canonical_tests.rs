use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::canonical_sink::{
    BufferedSink, CanonicalSink, SINK_BUFFER_CAPACITY, write_json_number, write_json_string,
};
use super::super::canonical_value::{keys_are_canonically_ordered, write_canonical};
use super::{canonical_json_bytes, canonical_json_value, canonical_sha256};

use serde_json::json;

#[test]
fn canonical_outputs_match_for_nested_ordering_and_scalars() {
    let value = json!({
        "z": null,
        "array": [true, false, -12.5, 0, {"z": 2, "a": 1}],
        "a": {"z": "last", "a": "first"},
    });
    let expected = concat!(
        r#"{"a":{"a":"first","z":"last"},"#,
        r#""array":[true,false,-12.5,0,{"a":1,"z":2}],"#,
        r#""z":null}"#,
    );

    let text = canonical_json_value(&value).unwrap();
    let bytes = canonical_json_bytes(&value).unwrap();

    assert_eq!(text, expected);
    assert_eq!(bytes, expected.as_bytes());
}

#[test]
fn canonical_outputs_preserve_json_escapes_and_unicode() {
    let value = json!({
        "unicode": "雪😀é",
        "escaped": "quote: \" slash: \\ newline:\n tab:\t control:\u{0001}",
    });
    let expected = "{\"escaped\":\"quote: \\\" slash: \\\\ newline:\\n tab:\\t control:\\u0001\",\"unicode\":\"雪😀é\"}";

    assert_eq!(canonical_json_value(&value).unwrap(), expected);
    assert_eq!(canonical_json_bytes(&value).unwrap(), expected.as_bytes());
}

#[test]
fn streaming_digest_matches_digest_of_canonical_bytes() {
    let value = json!({
        "unicode": ["雪", "😀", "é"],
        "nested": {"z": null, "a": [true, "line\nfeed", 42]},
    });
    let bytes = canonical_json_bytes(&value).unwrap();
    let digest = Sha256::digest(&bytes);
    let mut expected = String::from("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut expected, "{byte:02x}").unwrap();
    }

    assert_eq!(canonical_sha256(&value).unwrap().as_str(), expected);
}

/// The streamed string writer must stay byte-identical to the allocating
/// `serde_json::to_string` rendering it replaced, for every escape class.
#[test]
fn streamed_string_escapes_match_serde_json_for_every_scalar_byte() {
    let mut samples: Vec<String> = Vec::new();
    for code in 0u32..=0x2ff {
        if let Some(character) = char::from_u32(code) {
            samples.push(character.to_string());
            samples.push(format!("prefix{character}suffix"));
        }
    }
    samples.extend(
        [
            "",
            "plain",
            "\"",
            "\\",
            "\"\\\"",
            "back\\slash/solidus",
            "雪😀é",
            "mixed \u{0}\u{1}\u{7}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{1f} tail",
            "trailing\\",
            "\u{7f}delete",
        ]
        .into_iter()
        .map(str::to_owned),
    );

    for sample in samples {
        let expected = serde_json::to_string(&sample).unwrap();
        let mut streamed = String::new();
        write_json_string(&sample, &mut streamed);
        assert_eq!(streamed, expected, "escape mismatch for {sample:?}");

        let key_object = Value::Object(
            [(sample.clone(), Value::Null)]
                .into_iter()
                .collect::<serde_json::Map<String, Value>>(),
        );
        assert_eq!(
            canonical_json_value(&key_object).unwrap(),
            format!("{{{expected}:null}}"),
        );
    }
}

/// The stack-formatted integer writer must stay byte-identical to
/// `Number::to_string`, including the `i64::MIN` boundary.
#[test]
fn streamed_numbers_match_owned_number_rendering() {
    let numbers = [
        "0",
        "-0",
        "1",
        "-1",
        "9",
        "10",
        "-10",
        "18446744073709551615",
        "9223372036854775807",
        "-9223372036854775808",
        "0.0",
        "-0.5",
        "1e3",
        "-12.5",
        "1.7976931348623157e308",
    ];

    for text in numbers {
        let value: Value = serde_json::from_str(text).unwrap();
        let Value::Number(number) = &value else {
            panic!("{text} is not a JSON number");
        };
        let mut streamed = String::new();
        write_json_number(number, &mut streamed);
        assert_eq!(streamed, number.to_string(), "number mismatch for {text}");
    }
}

/// The exact pipeline the streaming serializer replaced: materialize a
/// `Value` with `serde_json::to_value`, then canonicalize that tree.
fn legacy_canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("legacy to_value");
    let mut output = Vec::new();
    write_canonical(&value, &mut output);
    output
}

fn legacy_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::from("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("hex encoding");
    }
    encoded
}

/// Assert the streaming serializer is byte-identical to the legacy
/// `to_value` + `write_canonical` pipeline, and that both digest the same.
#[track_caller]
fn assert_canonical_identity<T: Serialize + ?Sized>(value: &T, label: &str) {
    let legacy = legacy_canonical_bytes(value);
    let streamed = canonical_json_bytes(&value).expect("streamed canonical bytes");
    assert_eq!(
        String::from_utf8_lossy(&streamed),
        String::from_utf8_lossy(&legacy),
        "canonical byte mismatch for {label}",
    );
    assert_eq!(
        canonical_sha256(&value).expect("streamed digest").as_str(),
        legacy_digest(&legacy),
        "canonical digest mismatch for {label}",
    );
}

/// A map whose keys arrive in arbitrary order, possibly repeated: the
/// shape `#[derive(Serialize)]` never produces but `serialize_map` callers
/// can.
struct UnsortedMap(Vec<(&'static str, Value)>);

impl Serialize for UnsortedMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// A value that reaches `serialize_bytes` rather than a sequence.
struct RawBytes(&'static [u8]);

impl Serialize for RawBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

#[derive(Serialize)]
struct UnsortedFields {
    zulu: u8,
    alpha: Option<&'static str>,
    mike: Vec<i64>,
    #[serde(rename = "\"quoted\\key\n")]
    quoted: bool,
}

#[derive(Serialize)]
struct UnitStruct;

#[derive(Serialize)]
struct NewtypeStruct(UnsortedFields);

#[derive(Serialize)]
struct TupleStruct(u64, &'static str, ());

#[derive(Serialize)]
enum Shape {
    Unit,
    Newtype(i128),
    Tuple(u128, f64),
    Struct { zebra: char, ant: Option<u8> },
}

#[derive(Serialize)]
struct Outer {
    zeta: Inner,
    #[serde(flatten)]
    inner: Inner,
    alpha: u8,
}

#[derive(Serialize)]
struct Inner {
    yankee: bool,
    bravo: f32,
}

/// Byte-identity proof: the streaming serializer must reproduce the legacy
/// `to_value` + `write_canonical` bytes (and digest) exactly, across every
/// serde data-model shape the domain can hand it.
#[test]
fn streaming_serializer_matches_to_value_pipeline_byte_for_byte() {
    assert_canonical_identity(&(), "unit");
    assert_canonical_identity(&UnitStruct, "unit struct");
    assert_canonical_identity(&Option::<u8>::None, "none");
    assert_canonical_identity(&Some(Some(7u8)), "nested some");
    assert_canonical_identity(&true, "bool");
    assert_canonical_identity(&'雪', "char");
    assert_canonical_identity(&"quote:\" slash:\\ nl:\n ctl:\u{1} 雪😀é", "escapes");
    assert_canonical_identity(&RawBytes(&[0, 1, 127, 128, 255]), "bytes");
    assert_canonical_identity(&RawBytes(&[]), "empty bytes");
    assert_canonical_identity(&Vec::<u8>::new(), "empty sequence");
    assert_canonical_identity(&(1u8, "two", vec![3i8, -4]), "tuple");
    assert_canonical_identity(&TupleStruct(9, "nine", ()), "tuple struct");

    for value in [
        0i64,
        -1,
        1,
        i64::MIN,
        i64::MAX,
        i64::from(i32::MIN),
        -9_223_372_036_854_775_807,
    ] {
        assert_canonical_identity(&value, "i64 boundary");
    }
    for value in [0u64, 1, u64::MAX, u64::from(u32::MAX)] {
        assert_canonical_identity(&value, "u64 boundary");
    }
    for value in [
        0i128,
        -1,
        i128::from(i64::MIN),
        i128::from(u64::MAX),
        i128::from(i64::MAX),
    ] {
        assert_canonical_identity(&value, "in-range i128");
    }
    for value in [0u128, u128::from(u64::MAX)] {
        assert_canonical_identity(&value, "in-range u128");
    }
    // Out-of-range 128-bit integers must keep failing rather than digest.
    assert!(canonical_json_bytes(&(i128::from(u64::MAX) + 1)).is_err());
    assert!(canonical_json_bytes(&(i128::from(i64::MIN) - 1)).is_err());
    assert!(canonical_json_bytes(&(u128::from(u64::MAX) + 1)).is_err());

    for value in [
        0.0f64,
        -0.0,
        1.0,
        -12.5,
        1e3,
        1e-7,
        f64::MIN,
        f64::MAX,
        1.797_693_134_862_315_7e308,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert_canonical_identity(&value, "f64 boundary");
    }
    for value in [0.0f32, -0.0, 13.37, f32::MIN, f32::MAX, f32::NAN] {
        assert_canonical_identity(&value, "f32 boundary");
    }

    let fields = || UnsortedFields {
        zulu: 255,
        alpha: Some("first"),
        mike: vec![i64::MIN, 0, i64::MAX],
        quoted: false,
    };
    assert_canonical_identity(&fields(), "unsorted derived fields");
    assert_canonical_identity(&NewtypeStruct(fields()), "newtype struct");
    assert_canonical_identity(&vec![fields(), fields()], "sequence of structs");

    assert_canonical_identity(&Shape::Unit, "unit variant");
    assert_canonical_identity(&Shape::Newtype(i128::from(u64::MAX)), "newtype variant");
    assert_canonical_identity(&Shape::Tuple(u128::from(u64::MAX), -0.0), "tuple variant");
    assert_canonical_identity(
        &Shape::Struct {
            zebra: '\u{1}',
            ant: None,
        },
        "struct variant",
    );
    assert_canonical_identity(
        &vec![
            Shape::Unit,
            Shape::Newtype(-1),
            Shape::Struct {
                zebra: '"',
                ant: Some(3),
            },
        ],
        "sequence of variants",
    );

    assert_canonical_identity(
        &Outer {
            zeta: Inner {
                yankee: true,
                bravo: -1.5,
            },
            inner: Inner {
                yankee: false,
                bravo: 0.25,
            },
            alpha: 1,
        },
        "flattened struct",
    );

    assert_canonical_identity(&UnsortedMap(vec![]), "empty map");
    assert_canonical_identity(
        &UnsortedMap(vec![
            ("zulu", json!({"z": 1, "a": [1, 2, {"b": null, "a": true}]})),
            ("alpha", json!("value")),
            ("\u{1}control", json!(-0.0)),
            ("雪", json!({})),
            ("mike", Value::Null),
        ]),
        "unsorted map keys",
    );
    // Repeated keys collapse to the last value, exactly as repeated
    // `serde_json::Map::insert` calls do.
    assert_canonical_identity(
        &UnsortedMap(vec![
            ("dup", json!(1)),
            ("alpha", json!("a")),
            ("dup", json!(2)),
            ("dup", json!(3)),
        ]),
        "duplicate map keys",
    );

    assert_canonical_identity(
        &json!({
            "z": null,
            "array": [true, false, -12.5, 0, {"z": 2, "a": 1}],
            "a": {"z": "last", "a": "first"},
            "deep": [[[{"b": [], "a": {}}]]],
        }),
        "json value tree",
    );
}

/// A raw JSON payload keeps its `to_value` meaning: the private
/// `RawValue` struct token is parsed and re-canonicalized, not streamed
/// as an opaque struct.
#[test]
fn raw_value_payloads_match_to_value_pipeline() {
    let raw = serde_json::value::RawValue::from_string(
        r#"{ "z" : 1 , "a" : [ 2 , { "d" : 4 , "c" : 3 } ] }"#.to_owned(),
    )
    .expect("raw value parses");
    assert_canonical_identity(&raw, "bare raw value");
    assert_canonical_identity(
        &UnsortedMap(vec![("zulu", json!(1)), ("alpha", json!(2))]),
        "map beside raw value",
    );

    #[derive(Serialize)]
    struct WithRaw<'a> {
        zulu: &'a serde_json::value::RawValue,
        alpha: u8,
    }
    assert_canonical_identity(
        &WithRaw {
            zulu: &raw,
            alpha: 1,
        },
        "nested raw value",
    );
}

/// The buffered hashing sink must not change the bytes the hasher sees.
#[test]
fn buffered_sink_preserves_written_bytes() {
    let long = "x".repeat(SINK_BUFFER_CAPACITY * 3 + 7);
    let chunks = ["", "a", "\"", &long, "b", &"y".repeat(SINK_BUFFER_CAPACITY)];
    let mut direct = String::new();
    let mut buffered = BufferedSink::new(String::new());
    for chunk in chunks {
        direct.write(chunk);
        buffered.write(chunk);
    }
    assert_eq!(buffered.finish(), direct);
}

/// Objects whose keys already arrive sorted take the collect-free path; it
/// must agree with the collect-and-sort path on the same input.
#[test]
fn presorted_and_unsorted_objects_canonicalize_identically() {
    let sorted: Value =
        serde_json::from_str(r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#).expect("sorted fixture");
    let unsorted: Value = serde_json::from_str(r#"{"z":[{"a":4}],"b":{"z":3,"a":2},"a":1}"#)
        .expect("unsorted fixture");

    assert!(matches!(&sorted, Value::Object(values) if keys_are_canonically_ordered(values)));
    assert_eq!(
        canonical_json_value(&sorted).unwrap(),
        canonical_json_value(&unsorted).unwrap(),
    );
    assert_eq!(
        canonical_json_value(&sorted).unwrap(),
        r#"{"a":1,"b":{"a":2,"z":3},"z":[{"a":4}]}"#,
    );
}
