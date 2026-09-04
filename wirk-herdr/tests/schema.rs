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

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value, json};
use wirk_herdr::{
    AgentStatus, CloseWorkspace, CreateWorkspace, EventSubscription, FocusPane, HerdrEvent, Notify,
    OpenWorktree, PromptAgent, ReleaseAgent, RemoveWorktree, ReportAgent, ReportAgentSession,
    ReportMetadata, SendKeys, SplitDirection, SplitPane, StartAgent,
};

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

// ---- Request-param conformance (item 4, W1; client.md §4) -----------------
//
// Extends the machinery above (`dummy_value`/`resolve_ref`, reused
// verbatim, R2) to the other side of the wire: instead of building a
// dummy JSON object from the schema and deserializing it into a Rust
// type, this builds a real JSON object from `wirk_herdr::socket::
// params`'s builder functions (fed a sample request struct) and checks
// its keys against the schema's own `request/$defs/<Method>Params`
// def, by method name — every schema `required` property is present
// (non-null) in the produced object, and every key the object carries
// is a schema property (no name drifted from the fixture). The request
// structs (`CreateWorkspace`, `SplitPane`, ...) are already non-`Option`
// on every schema-required field by construction (landed `lib.rs`, out
// of this item's allow-list) — the "removing a required property makes
// the struct unable to omit it" half of client.md §4 is therefore a
// compile-time property of those structs, not a separate runtime
// check; this test proves the JSON shape produced from them lines up
// field-for-field with the schema.

/// The `method` names this item's `SocketClient` sends, each paired
/// with the request params object `wirk_herdr::socket::params` builds
/// for a representative sample of the struct that method sends.
fn request_params_by_method() -> Vec<(&'static str, Value)> {
    let mut env = BTreeMap::new();
    env.insert("WIRK_ESTATE_ROOT".to_string(), "/estate".to_string());

    vec![
        ("ping", json!({})),
        (
            "workspace.create",
            wirk_herdr::socket::params::workspace_create(&CreateWorkspace {
                cwd: PathBuf::from("/repo"),
                env: env.clone(),
                label: Some("wirk".to_string()),
            }),
        ),
        (
            "pane.split",
            wirk_herdr::socket::params::pane_split(&SplitPane {
                workspace_id: Some("ws1".to_string()),
                target_pane_id: Some("pane1".to_string()),
                direction: SplitDirection::Down,
                cwd: PathBuf::from("/repo"),
                env,
            }),
        ),
        (
            "worktree.open",
            wirk_herdr::socket::params::worktree_open(&OpenWorktree {
                path: PathBuf::from("/repo/worktree"),
                workspace_id: Some("ws1".to_string()),
            }),
        ),
        (
            "worktree.remove",
            wirk_herdr::socket::params::worktree_remove(&RemoveWorktree {
                workspace_id: "ws1".to_string(),
                force: Some(true),
            }),
        ),
        (
            "pane.send_text",
            wirk_herdr::socket::params::pane_send_text("pane1", "hello"),
        ),
        (
            "agent.start",
            wirk_herdr::socket::params::agent_start(&StartAgent {
                pane_id: "pane1".to_string(),
                kind: "claude".to_string(),
                name: "run1".to_string(),
                args: vec!["--model".to_string(), "sonnet".to_string()],
                timeout_ms: Some(5_000),
            }),
        ),
        (
            "agent.prompt",
            wirk_herdr::socket::params::agent_prompt(&PromptAgent {
                target: "pane1".to_string(),
                text: "do the task".to_string(),
            }),
        ),
        (
            "agent.wait",
            wirk_herdr::socket::params::agent_wait("pane1", AgentStatus::Working, 1_000),
        ),
        ("pane.get", wirk_herdr::socket::params::pane_get("pane1")),
        ("agent.get", wirk_herdr::socket::params::agent_get("pane1")),
        ("agent.list", wirk_herdr::socket::params::agent_list()),
        (
            "agent.send_keys",
            wirk_herdr::socket::params::agent_send_keys(&SendKeys {
                target: "pane1".to_string(),
                keys: vec!["Enter".to_string()],
            }),
        ),
        (
            "pane.release_agent",
            wirk_herdr::socket::params::pane_release_agent(&ReleaseAgent {
                pane_id: "pane1".to_string(),
                agent: "claude".to_string(),
                source: Some("hook".to_string()),
            }),
        ),
        (
            "pane.close",
            wirk_herdr::socket::params::pane_close("pane1"),
        ),
        (
            "workspace.close",
            wirk_herdr::socket::params::workspace_close(&CloseWorkspace {
                workspace_id: "ws1".to_string(),
            }),
        ),
        (
            "session.snapshot",
            wirk_herdr::socket::params::session_snapshot(),
        ),
        (
            "pane.report_agent_session",
            wirk_herdr::socket::params::pane_report_agent_session(&ReportAgentSession {
                pane_id: "pane1".to_string(),
                source: "herdr:claude".to_string(),
                agent: "claude".to_string(),
                agent_session_id: Some("sess1".to_string()),
                session_start_source: Some("hook".to_string()),
                seq: Some(1),
            }),
        ),
        (
            "pane.report_agent",
            wirk_herdr::socket::params::pane_report_agent(&ReportAgent {
                pane_id: "pane1".to_string(),
                source: "hook".to_string(),
                agent: "claude".to_string(),
                state: "working".to_string(),
                seq: Some(1),
            }),
        ),
        (
            "pane.report_metadata",
            wirk_herdr::socket::params::pane_report_metadata(
                &ReportMetadata {
                    pane_id: Some("pane1".to_string()),
                    workspace_id: None,
                    source: "hook".to_string(),
                    tokens: None,
                    title: Some("wirk run".to_string()),
                },
                "pane1",
            ),
        ),
        (
            "workspace.report_metadata",
            wirk_herdr::socket::params::workspace_report_metadata(
                &ReportMetadata {
                    pane_id: None,
                    workspace_id: Some("ws1".to_string()),
                    source: "hook".to_string(),
                    tokens: Some(json!({"in": 1})),
                    title: None,
                },
                "ws1",
            ),
        ),
        (
            "notification.show",
            wirk_herdr::socket::params::notification_show(&Notify {
                title: "wirk".to_string(),
                body: "run claimed".to_string(),
            }),
        ),
        (
            "pane.focus",
            wirk_herdr::socket::params::pane_focus(&FocusPane {
                pane_id: "pane1".to_string(),
            }),
        ),
        (
            "events.subscribe",
            wirk_herdr::socket::params::events_subscribe(&[
                EventSubscription::PaneAgentStatusChanged {
                    pane_id: "pane1".to_string(),
                },
            ]),
        ),
    ]
}

fn request_params_def<'a>(root: &'a Value, method: &str) -> &'a Value {
    let entries = root
        .pointer("/schemas/request/oneOf")
        .expect("schema has request.oneOf")
        .as_array()
        .expect("request.oneOf is an array");
    let entry = entries
        .iter()
        .find(|e| {
            e.pointer("/properties/method/const")
                .and_then(Value::as_str)
                == Some(method)
        })
        .unwrap_or_else(|| panic!("no request.oneOf entry for method {method:?}"));
    let params_ref = entry
        .pointer("/properties/params")
        .expect("request entry has a params schema");
    if let Some(r) = params_ref.get("$ref").and_then(Value::as_str) {
        resolve_ref(root, r)
    } else {
        params_ref
    }
}

/// For every method `SocketClient` sends: the object `wirk_herdr::
/// socket::params` builds carries every schema-required property
/// (non-null), and carries no key the schema does not name — proving
/// the hand-written params builders in `src/socket.rs` conform to the
/// vendored fixture, field for field.
#[test]
fn request_params_conform_to_the_schema() {
    let root = fixture();

    for (method, produced) in request_params_by_method() {
        let def = request_params_def(&root, method);
        let properties: std::collections::HashSet<&str> = def
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let required: Vec<&str> = def
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let produced_obj = produced
            .as_object()
            .unwrap_or_else(|| panic!("{method}: params builder did not produce a JSON object"));

        for name in &required {
            let value = produced_obj.get(*name).unwrap_or_else(|| {
                panic!(
                    "{method}: schema requires {name:?}, missing from the built params: {produced}"
                )
            });
            assert!(
                !value.is_null(),
                "{method}: schema requires {name:?}, built params sends null: {produced}"
            );
        }

        for key in produced_obj.keys() {
            assert!(
                properties.contains(key.as_str()),
                "{method}: built params sends {key:?}, absent from the schema's \
                 params properties {properties:?} (renamed or removed upstream, \
                 or a typo in src/socket.rs)"
            );
        }
    }
}

/// The set of methods `request_params_by_method` exercises must equal
/// `wirk_herdr::socket::METHODS` — the const `SocketClient` itself
/// owns, built from every `self.call(...)` site and `events.subscribe`
/// (W1 fix: `events.subscribe` was sending its params inline, uncovered
/// by `request_params_conform_to_the_schema`). A method added to
/// `METHODS` without a matching entry here, or vice versa, fails this
/// test — the two sets can no longer drift apart silently.
#[test]
fn method_set_matches_the_conformance_test() {
    let covered: std::collections::BTreeSet<&str> = request_params_by_method()
        .iter()
        .map(|(method, _)| *method)
        .collect();
    let client: std::collections::BTreeSet<&str> =
        wirk_herdr::socket::METHODS.iter().copied().collect();
    assert_eq!(
        covered, client,
        "request_params_by_method (schema.rs) and wirk_herdr::socket::METHODS \
         (socket.rs) have drifted apart: every method SocketClient can send must \
         appear in both"
    );
}

// ---- Enum-value conformance (fix 2, 0028 tried step 2's finding) ----------
//
// `request_params_conform_to_the_schema` and `covered_variants_conform_
// field_for_field` above prove field *shape* — presence, requiredness,
// nesting — but never a field's *values*, so a Rust enum whose wire
// vocabulary drifted from the schema's own (`SplitDirection`'s
// `"horizontal"`/`"vertical"` vs the schema's `"right"`/`"down"`) passed
// clean while every live `pane.split` call the client sent was
// `invalid_request` (`knowledge/work/p1-herdr-executor/tried/
// RESULT.md`, run 2). This section closes that gap for every enum
// `wirk-herdr` puts on the wire — currently `SplitDirection` (a request
// field) and `AgentStatus` (an info/event field, and embedded in the
// `agent.wait` request's `until` array) — by comparing the *set* of
// values each Rust enum serializes to against the schema's own `enum`
// list, exactly (not merely a subset): a Rust variant absent from the
// schema fails, and a schema value no Rust variant reaches also fails,
// so neither side can drift without the other noticing.

use std::collections::BTreeSet;

/// The schema's `enum` list at `schemas/<namespace>/$defs/<def_name>`
/// (e.g. `("event", "SplitDirection")`), as owned `String`s.
fn schema_enum_values(root: &Value, namespace: &str, def_name: &str) -> BTreeSet<String> {
    root.pointer(&format!("/schemas/{namespace}/$defs/{def_name}"))
        .unwrap_or_else(|| panic!("no schemas.{namespace}.$defs.{def_name} in the fixture"))
        .get("enum")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("schemas.{namespace}.$defs.{def_name} has no \"enum\""))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| {
                    panic!("schemas.{namespace}.$defs.{def_name}: non-string enum member: {v}")
                })
                .to_string()
        })
        .collect()
}

/// Serializes every value in `rust_values` (each already a `Value`,
/// e.g. via `serde_json::to_value`) and asserts the resulting string
/// set equals `schema_values` exactly.
fn assert_wire_values_match_schema_exactly(
    type_name: &str,
    rust_values: &[Value],
    schema_values: &BTreeSet<String>,
) {
    let produced: BTreeSet<String> = rust_values
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("{type_name}: serialized to a non-string value: {v}"))
                .to_string()
        })
        .collect();
    assert_eq!(
        &produced, schema_values,
        "{type_name}: Rust variants serialize to {produced:?}, the schema's own enum is \
         {schema_values:?} — every Rust variant must serialize to a schema member, and \
         every schema member must be reachable from some Rust variant"
    );
}

/// `SplitDirection`'s two Rust variants serialize to exactly the
/// schema's `SplitDirection` enum (`["right","down"]`), and a value
/// outside that list — the old, wrong `"horizontal"` wire value chief
/// among them — is rejected on deserialize, not silently accepted.
#[test]
fn split_direction_wire_values_match_the_schema_exactly() {
    let root = fixture();
    let schema_values = schema_enum_values(&root, "event", "SplitDirection");

    let rust_values: Vec<Value> = [SplitDirection::Right, SplitDirection::Down]
        .into_iter()
        .map(|v| serde_json::to_value(v).expect("SplitDirection serializes"))
        .collect();
    assert_wire_values_match_schema_exactly("SplitDirection", &rust_values, &schema_values);

    for outside in ["horizontal", "vertical", "sideways"] {
        let result: Result<SplitDirection, _> = serde_json::from_value(json!(outside));
        assert!(
            result.is_err(),
            "SplitDirection must reject {outside:?}, absent from the schema's enum \
             {schema_values:?}, but deserialized it as {result:?}"
        );
    }
}

/// The specific case 0028 tried step 2 found live: `params::pane_split`
/// serializes `direction` to a string that is a member of the schema's
/// own `SplitDirection` enum, for every direction the client can send.
#[test]
fn split_pane_serializes_direction_to_a_schema_enum_value() {
    let root = fixture();
    let schema_values = schema_enum_values(&root, "event", "SplitDirection");
    let mut env = BTreeMap::new();
    env.insert("WIRK_ESTATE_ROOT".to_string(), "/estate".to_string());

    for direction in [SplitDirection::Right, SplitDirection::Down] {
        let req = SplitPane {
            workspace_id: Some("ws1".to_string()),
            target_pane_id: Some("pane1".to_string()),
            direction,
            cwd: PathBuf::from("/repo"),
            env: env.clone(),
        };
        let params = wirk_herdr::socket::params::pane_split(&req);
        let wire = params
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("pane.split params: \"direction\" missing or non-string: {params}")
            });
        assert!(
            schema_values.contains(wire),
            "pane.split serialized direction {direction:?} to {wire:?}, absent from the \
             schema's SplitDirection enum {schema_values:?}"
        );
    }
}

/// `AgentStatus`'s five Rust variants serialize to exactly the schema's
/// `AgentStatus` enum, every schema value round-trips back through
/// deserialize, and a value outside that list is rejected — exercised
/// both directions since `AgentStatus` is both a request field
/// (`agent.wait`'s `until`) and an info/event field (`PaneInfo`.
/// `agent_status`, `HerdrEvent::PaneAgentStatusChanged.agent_status`).
#[test]
fn agent_status_wire_values_match_the_schema_exactly() {
    let root = fixture();
    let schema_values = schema_enum_values(&root, "event", "AgentStatus");

    let rust_values: Vec<Value> = [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Unknown,
    ]
    .into_iter()
    .map(|v| serde_json::to_value(v).expect("AgentStatus serializes"))
    .collect();
    assert_wire_values_match_schema_exactly("AgentStatus", &rust_values, &schema_values);

    for value in &schema_values {
        let parsed: AgentStatus = serde_json::from_value(json!(value)).unwrap_or_else(|e| {
            panic!("AgentStatus must accept the schema's own value {value:?}: {e}")
        });
        let round_tripped =
            serde_json::to_value(parsed).expect("AgentStatus serializes after round-trip");
        assert_eq!(
            round_tripped.as_str(),
            Some(value.as_str()),
            "AgentStatus round-trip changed the wire value for {value:?}"
        );
    }

    for outside in ["paused", "running", "error"] {
        let result: Result<AgentStatus, _> = serde_json::from_value(json!(outside));
        assert!(
            result.is_err(),
            "AgentStatus must reject {outside:?}, absent from the schema's enum \
             {schema_values:?}, but deserialized it as {result:?}"
        );
    }
}

/// `agent.wait`'s `until` param embeds `AgentStatus` in a request
/// (`params::agent_wait`, not an info/event struct) — checked
/// separately from the round-trip test above since this is the one
/// place `AgentStatus` is serialized as a *request* field rather than
/// read back from a reply.
#[test]
fn agent_wait_serializes_until_to_a_schema_enum_value() {
    let root = fixture();
    let schema_values = schema_enum_values(&root, "event", "AgentStatus");

    for status in [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Unknown,
    ] {
        let params = wirk_herdr::socket::params::agent_wait("pane1", status, 1_000);
        let until = params
            .get("until")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("agent.wait params: \"until\" missing or non-array: {params}")
            });
        for value in until {
            let wire = value.as_str().unwrap_or_else(|| {
                panic!("agent.wait params: \"until\" member not a string: {value}")
            });
            assert!(
                schema_values.contains(wire),
                "agent.wait serialized until={status:?} to {wire:?}, absent from the \
                 schema's AgentStatus enum {schema_values:?}"
            );
        }
    }
}
