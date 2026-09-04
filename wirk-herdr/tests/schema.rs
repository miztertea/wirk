//! Fixture-driven conformance test: every `HerdrEvent` variant the enum
//! covers matches the vendored Herdr schema's `EventData` variant of
//! the same `type`, field for field (0017 D51; issue 223; W3 fix).
//!
//! Fixture: `tests/fixtures/herdr-schema-0.8.2-p20.json`, the verbatim
//! output of `herdr api schema --json` against the installed `herdr
//! 0.8.2` binary (R4 — nothing here is hand-written). Its top level
//! carries `"protocol": 20, "schema_version": 1`, asserted below.
//!
//! For every covered variant this test builds a JSON object from the
//! fixture's `required` list with dummy values, asserts it
//! deserializes into `HerdrEvent` (proves the enum is missing no
//! required field); removes each required property in turn and
//! asserts deserialization then fails (proves the enum does not make a
//! required field `Option`); and adds every optional property with a
//! dummy value and asserts success (proves the enum accepts the full
//! shape). It also asserts the covered-name set is a subset of the
//! fixture's variant names, so a renamed upstream variant fails this
//! test. No sleeps, no binary invocation at test time — the fixture is
//! a static file read by `serde_json`.

use serde_json::{Map, Value, json};
use wirk_herdr::HerdrEvent;

/// The `EventData.type` names this build's `HerdrEvent` enum covers —
/// a deliberate subset of the schema's full `oneOf` (herdr.md §1); see
/// `wirk-herdr/src/lib.rs`'s enum doc for the ones left out.
const COVERED: &[&str] = &[
    "workspace_created",
    "workspace_closed",
    "workspace_metadata_updated",
    "workspace_focused",
    "worktree_opened",
    "worktree_removed",
    "tab_created",
    "tab_focused",
    "pane_created",
    "pane_updated",
    "pane_closed",
    "pane_focused",
    "pane_moved",
    "pane_exited",
    "pane_agent_detected",
    "pane_agent_status_changed",
];

fn fixture() -> Value {
    let raw = include_str!("fixtures/herdr-schema-0.8.2-p20.json");
    serde_json::from_str(raw).expect("fixture is valid JSON")
}

fn event_data_variants(root: &Value) -> Vec<Value> {
    root.pointer("/schemas/event/$defs/EventData/oneOf")
        .expect("schema has event.$defs.EventData.oneOf")
        .as_array()
        .expect("oneOf is an array")
        .clone()
}

fn variant_type_name(variant: &Value) -> &str {
    variant
        .pointer("/properties/type/const")
        .and_then(Value::as_str)
        .expect("variant has properties.type.const")
}

/// Resolves a JSON Schema `$ref` of the form
/// `#/schemas/event/$defs/<Name>` against the fixture root.
fn resolve_ref<'a>(root: &'a Value, r: &str) -> &'a Value {
    let pointer = r.strip_prefix('#').expect("$ref is a local pointer");
    root.pointer(pointer)
        .unwrap_or_else(|| panic!("unresolved $ref: {r}"))
}

/// Picks the non-null branch of an `anyOf`/`oneOf` nullable union.
fn non_null_branch(branches: &[Value]) -> &Value {
    branches
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) != Some("null"))
        .expect("anyOf/oneOf has a non-null branch")
}

/// Builds a dummy JSON value for one property schema: string -> "x",
/// boolean -> false, integer/number -> 0, array -> [], object/$ref -> a
/// recursively built object holding that def's required properties
/// (enum -> its first member).
fn dummy_value(root: &Value, schema: &Value) -> Value {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        let def = resolve_ref(root, r);
        return dummy_value(root, def);
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        return dummy_value(root, non_null_branch(any_of));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        return dummy_value(root, non_null_branch(one_of));
    }
    if let Some(en) = schema.get("enum").and_then(Value::as_array) {
        return en.first().cloned().expect("enum has at least one member");
    }
    if schema.get("properties").is_some() {
        return Value::Object(build_required(root, schema));
    }
    let ty = match schema.get("type") {
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("string"),
        _ => "string",
    };
    match ty {
        "string" => json!("x"),
        "boolean" => json!(false),
        "integer" | "number" => json!(0),
        "array" => json!([]),
        "object" => json!({}),
        other => panic!("unhandled schema type: {other}"),
    }
}

/// Builds a JSON object from a def's (or variant's) `required` list,
/// each field filled with a dummy value from its property schema.
fn build_required(root: &Value, def: &Value) -> Map<String, Value> {
    let properties = def
        .get("properties")
        .and_then(Value::as_object)
        .expect("def has properties");
    let required = def
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut out = Map::new();
    for name in required {
        let prop = properties
            .get(name)
            .unwrap_or_else(|| panic!("required property {name} missing from properties"));
        out.insert(name.to_string(), dummy_value(root, prop));
    }
    out
}

/// The optional (non-required, non-`type`) property names of a
/// variant, each paired with a dummy value.
fn optional_fields(root: &Value, variant: &Value) -> Vec<(String, Value)> {
    let properties = variant
        .get("properties")
        .and_then(Value::as_object)
        .expect("variant has properties");
    let required: std::collections::HashSet<&str> = variant
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    properties
        .iter()
        .filter(|(name, _)| name.as_str() != "type" && !required.contains(name.as_str()))
        .map(|(name, schema)| (name.clone(), dummy_value(root, schema)))
        .collect()
}

#[test]
fn fixture_is_protocol_20_schema_version_1() {
    let root = fixture();
    assert_eq!(root.get("protocol").and_then(Value::as_i64), Some(20));
    assert_eq!(root.get("schema_version").and_then(Value::as_i64), Some(1));
}

#[test]
fn covered_variant_names_are_a_subset_of_the_schema() {
    let root = fixture();
    let variants = event_data_variants(&root);
    let schema_names: std::collections::HashSet<&str> =
        variants.iter().map(variant_type_name).collect();
    for name in COVERED {
        assert!(
            schema_names.contains(name),
            "HerdrEvent covers {name:?}, absent from the schema's EventData.oneOf \
             (renamed or removed upstream)"
        );
    }
}

/// For every covered variant: the minimal required-only shape
/// deserializes; dropping any one required property makes it fail;
/// adding every optional property still succeeds.
#[test]
fn covered_variants_conform_field_for_field() {
    let root = fixture();
    let variants = event_data_variants(&root);

    for variant in &variants {
        let type_name = variant_type_name(variant);
        if !COVERED.contains(&type_name) {
            continue;
        }

        let mut required_obj = build_required(&root, variant);
        required_obj.insert("type".to_string(), json!(type_name));
        let required_value = Value::Object(required_obj.clone());

        serde_json::from_value::<HerdrEvent>(required_value.clone()).unwrap_or_else(|e| {
            panic!(
                "{type_name}: required-only shape failed to deserialize into HerdrEvent \
                 (a required schema field is missing from the enum): {e}\nshape: {required_value}"
            )
        });

        for key in required_obj.keys() {
            if key == "type" {
                continue;
            }
            let mut missing = required_obj.clone();
            missing.remove(key);
            let missing_value = Value::Object(missing);
            let result = serde_json::from_value::<HerdrEvent>(missing_value.clone());
            assert!(
                result.is_err(),
                "{type_name}: deserialization succeeded with required field {key:?} \
                 missing (the enum makes a required field Option): {missing_value}"
            );
        }

        let mut full_obj = required_obj.clone();
        for (name, value) in optional_fields(&root, variant) {
            full_obj.insert(name, value);
        }
        let full_value = Value::Object(full_obj);
        serde_json::from_value::<HerdrEvent>(full_value.clone()).unwrap_or_else(|e| {
            panic!(
                "{type_name}: full shape (required + every optional property) failed to \
                 deserialize into HerdrEvent (a field is typed stricter than the schema, or \
                 misnamed): {e}\nshape: {full_value}"
            )
        });
    }
}
