use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use histae_api_rust::contract::{
    ContractCorpus, DEFAULT_MAX_RESPONSE_BYTES, RunConfig, Target, run_corpus,
};
use uuid::Uuid;

#[tokio::test]
async fn compares_the_same_contract_against_two_isolated_targets() {
    let reference = serve(2, |_, path| response_for(path, 200));
    let candidate = serve(2, |_, path| response_for(path, 200));
    let corpus = smoke_corpus();

    let report = run_corpus(
        &corpus,
        &config(reference, "nest-state", candidate, "rust-state", false),
    )
    .await
    .unwrap_or_else(|error| panic!("contract run failed: {error}"));

    assert!(report.passed);
    assert_eq!(report.scenario_count, 2);
    assert!(report.scenarios.iter().all(|scenario| scenario.passed));
}

#[tokio::test]
async fn reports_status_body_and_header_differences_without_exposing_bodies() {
    let reference = serve(2, |_, path| response_for(path, 200));
    let candidate = serve(2, |_, path| {
        if path == "/health/live" {
            http_response(
                201,
                "application/json",
                "not-a-uuid",
                r#"{"status":"different"}"#,
            )
        } else {
            response_for(path, 200)
        }
    });

    let report = run_corpus(
        &smoke_corpus(),
        &config(reference, "nest-state", candidate, "rust-state", false),
    )
    .await
    .unwrap_or_else(|error| panic!("contract run failed: {error}"));

    assert!(!report.passed);
    let failure = &report.scenarios[0];
    assert!(!failure.passed);
    assert!(failure.mismatches.iter().any(|item| item.field == "status"));
    assert!(failure.mismatches.iter().any(|item| item.field == "body"));
    assert!(
        failure
            .mismatches
            .iter()
            .any(|item| item.field == "header:x-request-id")
    );
    let serialized = serde_json::to_string(&report)
        .unwrap_or_else(|error| panic!("report serialization failed: {error}"));
    assert!(!serialized.contains("different"));
}

#[test]
fn command_runs_the_corpus_and_returns_a_json_report() {
    let reference = serve(2, |_, path| response_for(path, 200));
    let candidate = serve(2, |_, path| response_for(path, 200));
    let corpus = format!(
        "{}/tests/contract/corpus/smoke.json",
        env!("CARGO_MANIFEST_DIR").replace('\\', "/")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_contract-compare"))
        .args([
            "--corpus",
            &corpus,
            "--reference-url",
            &reference,
            "--reference-state",
            "nest-cli-state",
            "--candidate-url",
            &candidate,
            "--candidate-state",
            "rust-cli-state",
        ])
        .output()
        .unwrap_or_else(|error| panic!("contract command failed to start: {error}"));

    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("command report should be JSON: {error}"));
    assert_eq!(report["passed"], true);
    assert_eq!(report["scenario_count"], 2);
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn compares_an_isolated_mutation_and_its_observable_effect() {
    let reference_state = Arc::new(AtomicBool::new(false));
    let candidate_state = Arc::new(AtomicBool::new(false));
    let reference = stateful_server(Arc::clone(&reference_state));
    let candidate = stateful_server(Arc::clone(&candidate_state));
    let corpus: ContractCorpus = serde_json::from_str(&mutation_corpus())
        .unwrap_or_else(|error| panic!("mutation corpus should parse: {error}"));

    let report = run_corpus(
        &corpus,
        &config(reference, "nest-write", candidate, "rust-write", true),
    )
    .await
    .unwrap_or_else(|error| panic!("contract run failed: {error}"));

    assert!(report.passed);
    assert!(reference_state.load(Ordering::SeqCst));
    assert!(candidate_state.load(Ordering::SeqCst));
}

#[tokio::test]
async fn accepts_only_catalogued_dynamic_json_fields() {
    let reference = serve(1, |_, _| dynamic_response(&Uuid::new_v4().to_string()));
    let candidate = serve(1, |_, _| dynamic_response(&Uuid::new_v4().to_string()));
    let corpus: ContractCorpus = serde_json::from_str(&dynamic_corpus())
        .unwrap_or_else(|error| panic!("dynamic corpus should parse: {error}"));

    let report = run_corpus(
        &corpus,
        &config(reference, "nest-dynamic", candidate, "rust-dynamic", false),
    )
    .await
    .unwrap_or_else(|error| panic!("contract run failed: {error}"));

    assert!(report.passed);
}

#[tokio::test]
async fn keeps_session_cookies_isolated_between_targets() {
    let reference = cookie_server("reference-session");
    let candidate = cookie_server("candidate-session");
    let corpus: ContractCorpus = serde_json::from_str(&cookie_corpus())
        .unwrap_or_else(|error| panic!("cookie corpus should parse: {error}"));

    let report = run_corpus(
        &corpus,
        &config(reference, "nest-cookie", candidate, "rust-cookie", false),
    )
    .await
    .unwrap_or_else(|error| panic!("contract run failed: {error}"));

    assert!(report.passed);
}

#[test]
fn rejects_unknown_fields_and_duplicate_scenario_ids() {
    let unknown = r#"{
      "version":1,
      "unexpected":true,
      "scenarios":[]
    }"#;
    assert!(serde_json::from_str::<ContractCorpus>(unknown).is_err());

    let duplicate = corpus_with_scenarios(&format!("{},{}", scenario("same"), scenario("same")));
    let corpus: ContractCorpus = serde_json::from_str(&duplicate)
        .unwrap_or_else(|error| panic!("fixture should parse: {error}"));
    let error = config(
        "http://127.0.0.1:1/".to_owned(),
        "nest-state",
        "http://127.0.0.1:2/".to_owned(),
        "rust-state",
        false,
    )
    .validate(&corpus)
    .expect_err("duplicate IDs must fail validation");
    assert_eq!(error.public_message(), "scenario IDs must be unique");
}

#[test]
fn refuses_shared_state_and_mutations_without_explicit_opt_in() {
    let read_only = smoke_corpus();
    let shared = config(
        "http://127.0.0.1:1/".to_owned(),
        "shared",
        "http://127.0.0.1:2/".to_owned(),
        "shared",
        false,
    );
    assert_eq!(
        shared
            .validate(&read_only)
            .expect_err("shared state must fail")
            .public_message(),
        "reference and candidate state namespaces must be different"
    );

    let mutation: ContractCorpus = serde_json::from_str(&corpus_with_scenarios(
        &scenario_with_safety("mutation", "isolated_mutation"),
    ))
    .unwrap_or_else(|error| panic!("fixture should parse: {error}"));
    let isolated = config(
        "http://127.0.0.1:1/".to_owned(),
        "nest-state",
        "http://127.0.0.1:2/".to_owned(),
        "rust-state",
        false,
    );
    assert_eq!(
        isolated
            .validate(&mutation)
            .expect_err("mutation without opt-in must fail")
            .public_message(),
        "corpus contains isolated mutations; pass the explicit mutation opt-in"
    );
}

fn smoke_corpus() -> ContractCorpus {
    serde_json::from_str(include_str!("contract/corpus/smoke.json"))
        .unwrap_or_else(|error| panic!("smoke corpus should parse: {error}"))
}

fn stateful_server(state: Arc<AtomicBool>) -> String {
    serve(2, move |method, path| match (method, path) {
        ("POST", "/resource") => {
            state.store(true, Ordering::SeqCst);
            http_response(
                201,
                "application/json",
                &Uuid::new_v4().to_string(),
                r#"{"created":true}"#,
            )
        }
        ("GET", "/resource") => {
            let body = if state.load(Ordering::SeqCst) {
                r#"{"count":1}"#
            } else {
                r#"{"count":0}"#
            };
            http_response(200, "application/json", &Uuid::new_v4().to_string(), body)
        }
        _ => http_response(404, "application/json", &Uuid::new_v4().to_string(), "{}"),
    })
}

fn cookie_server(session: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("cookie listener failed: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("cookie listener address failed: {error}"));
    thread::spawn(move || {
        for request_number in 0..2 {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|error| panic!("cookie accept failed: {error}"));
            let mut bytes = [0_u8; 4096];
            let length = stream
                .read(&mut bytes)
                .unwrap_or_else(|error| panic!("cookie request read failed: {error}"));
            let request = String::from_utf8_lossy(&bytes[..length]);
            let response = if request_number == 0 {
                let body = r#"{"authenticated":true}"#;
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: session={session}; Path=/; HttpOnly; SameSite=Strict\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                let authenticated = request
                    .lines()
                    .any(|line| line == format!("cookie: session={session}"));
                let body = if authenticated {
                    r#"{"authorized":true}"#
                } else {
                    r#"{"authorized":false}"#
                };
                http_response(200, "application/json", &Uuid::new_v4().to_string(), body)
            };
            stream
                .write_all(response.as_bytes())
                .unwrap_or_else(|error| panic!("cookie response failed: {error}"));
        }
    });
    format!("http://{address}/")
}

fn config(
    reference_url: String,
    reference_state: &str,
    candidate_url: String,
    candidate_state: &str,
    allow_isolated_mutations: bool,
) -> RunConfig {
    RunConfig {
        reference: Target::parse("reference", &reference_url, reference_state)
            .unwrap_or_else(|error| panic!("reference target invalid: {error}")),
        candidate: Target::parse("candidate", &candidate_url, candidate_state)
            .unwrap_or_else(|error| panic!("candidate target invalid: {error}")),
        allow_isolated_mutations,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        request_timeout: Duration::from_secs(2),
    }
}

fn serve(requests: usize, responder: impl Fn(&str, &str) -> String + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("test listener failed: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("test listener address failed: {error}"));
    thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|error| panic!("test accept failed: {error}"));
            let (method, path) = request_line(&mut stream);
            let response = responder(&method, &path);
            stream
                .write_all(response.as_bytes())
                .unwrap_or_else(|error| panic!("test response failed: {error}"));
        }
    });
    format!("http://{address}/")
}

fn request_line(stream: &mut TcpStream) -> (String, String) {
    let mut bytes = [0_u8; 4096];
    let length = stream
        .read(&mut bytes)
        .unwrap_or_else(|error| panic!("test request read failed: {error}"));
    let request = String::from_utf8_lossy(&bytes[..length]);
    let mut parts = request
        .lines()
        .next()
        .map(str::split_whitespace)
        .into_iter()
        .flatten();
    (
        parts.next().unwrap_or("GET").to_owned(),
        parts.next().unwrap_or("/").to_owned(),
    )
}

fn response_for(path: &str, _status: u16) -> String {
    if path == "/health/live" {
        http_response(
            200,
            "application/json; charset=utf-8",
            &Uuid::new_v4().to_string(),
            r#"{"status":"ok"}"#,
        )
    } else {
        http_response(
            404,
            "application/json; charset=utf-8",
            &Uuid::new_v4().to_string(),
            r#"{"error":{"code":"route_not_found","message":"This route is not available."}}"#,
        )
    }
}

fn http_response(status: u16, content_type: &str, request_id: &str, body: &str) -> String {
    let reason = if status == 200 {
        "OK"
    } else if status == 201 {
        "Created"
    } else {
        "Not Found"
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Request-ID: {request_id}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn corpus_with_scenarios(scenarios: &str) -> String {
    format!(r#"{{"version":1,"scenarios":[{scenarios}]}}"#)
}

fn scenario(id: &str) -> String {
    scenario_with_safety(id, "read_only")
}

fn scenario_with_safety(id: &str, safety: &str) -> String {
    format!(
        r#"{{
          "id":"{id}",
          "safety":"{safety}",
          "request":{{"method":"GET","path":"/health/live","headers":{{}},"body":null}},
          "expected":{{"status":200,"headers":{{}},"body":{{"kind":"empty"}}}}
        }}"#
    )
}

fn mutation_corpus() -> String {
    r#"{
      "version":1,
      "scenarios":[
        {
          "id":"create_resource",
          "safety":"isolated_mutation",
          "request":{"method":"POST","path":"/resource","headers":{},"body":null},
          "expected":{"status":201,"headers":{},"body":{"kind":"exact_json","value":{"created":true}}}
        },
        {
          "id":"read_effect",
          "safety":"read_only",
          "request":{"method":"GET","path":"/resource","headers":{},"body":null},
          "expected":{"status":200,"headers":{},"body":{"kind":"exact_json","value":{"count":1}}}
        }
      ]
    }"#
    .to_owned()
}

fn dynamic_corpus() -> String {
    r#"{
      "version":1,
      "scenarios":[{
        "id":"dynamic_id",
        "safety":"read_only",
        "request":{"method":"GET","path":"/resource","headers":{},"body":null},
        "expected":{
          "status":200,
          "headers":{},
          "body":{
            "kind":"exact_json",
            "value":{"id":"generated-id","stable":"value"},
            "dynamic_fields":{"/id":{"kind":"uuid_v4"}}
          }
        }
      }]
    }"#
    .replace("generated-id", &Uuid::new_v4().to_string())
}

fn dynamic_response(id: &str) -> String {
    http_response(
        200,
        "application/json",
        &Uuid::new_v4().to_string(),
        &format!(r#"{{"id":"{id}","stable":"value"}}"#),
    )
}

fn cookie_corpus() -> String {
    r#"{
      "version":1,
      "scenarios":[
        {
          "id":"login",
          "safety":"read_only",
          "request":{"method":"GET","path":"/login","headers":{},"body":null},
          "expected":{
            "status":200,
            "headers":{"set-cookie":{"kind":"contains","value":"session="}},
            "body":{"kind":"exact_json","value":{"authenticated":true}}
          }
        },
        {
          "id":"authenticated_read",
          "safety":"read_only",
          "request":{"method":"GET","path":"/private","headers":{},"body":null},
          "expected":{"status":200,"headers":{},"body":{"kind":"exact_json","value":{"authorized":true}}}
        }
      ]
    }"#
    .to_owned()
}
