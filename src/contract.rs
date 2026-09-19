use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::Duration,
};

use reqwest::{
    Method, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1_048_576;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractCorpus {
    pub version: u32,
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub safety: ScenarioSafety,
    pub request: RequestSpec,
    pub expected: ResponseExpectation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioSafety {
    ReadOnly,
    IsolatedMutation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: Option<RequestBody>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestBody {
    Json { value: Value },
    Text { value: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseExpectation {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, HeaderExpectation>,
    pub body: BodyExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeaderExpectation {
    Exact { value: String },
    Contains { value: String },
    Present,
    UuidV4,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BodyExpectation {
    ExactJson {
        value: Value,
        #[serde(default)]
        dynamic_fields: BTreeMap<String, DynamicJsonField>,
    },
    ExactText {
        value: String,
    },
    Empty,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DynamicJsonField {
    Any,
    UuidV4,
    NonEmptyString,
    Integer,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub base_url: Url,
    pub state_namespace: String,
}

impl Target {
    pub fn parse(
        name: impl Into<String>,
        base_url: &str,
        state_namespace: impl Into<String>,
    ) -> Result<Self, HarnessError> {
        let name = name.into();
        let state_namespace = state_namespace.into();
        if !valid_label(&name) {
            return Err(HarnessError::configuration(
                "target name must contain 1 to 64 safe characters",
            ));
        }
        if !valid_namespace(&state_namespace) {
            return Err(HarnessError::configuration(
                "state namespace must contain 1 to 128 safe characters",
            ));
        }
        let base_url = Url::parse(base_url)
            .map_err(|_| HarnessError::configuration("target base URL is invalid"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !base_url.path().ends_with('/')
        {
            return Err(HarnessError::configuration(
                "target base URL must be an HTTP(S) origin or end with a slash, without credentials, query, or fragment",
            ));
        }
        Ok(Self {
            name,
            base_url,
            state_namespace,
        })
    }
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub reference: Target,
    pub candidate: Target,
    pub allow_isolated_mutations: bool,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
}

impl RunConfig {
    pub fn validate(&self, corpus: &ContractCorpus) -> Result<(), HarnessError> {
        validate_corpus(corpus)?;
        if self.reference.base_url == self.candidate.base_url {
            return Err(HarnessError::configuration(
                "reference and candidate URLs must be different",
            ));
        }
        if self.reference.state_namespace == self.candidate.state_namespace {
            return Err(HarnessError::configuration(
                "reference and candidate state namespaces must be different",
            ));
        }
        if self.max_response_bytes == 0 {
            return Err(HarnessError::configuration(
                "maximum response size must be greater than zero",
            ));
        }
        if self.request_timeout.is_zero() {
            return Err(HarnessError::configuration(
                "request timeout must be greater than zero",
            ));
        }
        if corpus
            .scenarios
            .iter()
            .any(|scenario| scenario.safety == ScenarioSafety::IsolatedMutation)
            && !self.allow_isolated_mutations
        {
            return Err(HarnessError::configuration(
                "corpus contains isolated mutations; pass the explicit mutation opt-in",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub passed: bool,
    pub scenario_count: usize,
    pub scenarios: Vec<ScenarioReport>,
}

#[derive(Debug, Serialize)]
pub struct ScenarioReport {
    pub id: String,
    pub passed: bool,
    pub mismatches: Vec<Mismatch>,
}

#[derive(Debug, Serialize)]
pub struct Mismatch {
    pub target: String,
    pub field: String,
    pub reason: String,
}

#[derive(Debug)]
struct ObservedResponse {
    status: u16,
    headers: BTreeMap<String, Vec<String>>,
    body: Vec<u8>,
}

pub async fn run_corpus(
    corpus: &ContractCorpus,
    config: &RunConfig,
) -> Result<RunReport, HarnessError> {
    config.validate(corpus)?;
    let reference_client = build_client(config.request_timeout)?;
    let candidate_client = build_client(config.request_timeout)?;

    let mut reports = Vec::with_capacity(corpus.scenarios.len());
    for scenario in &corpus.scenarios {
        let reference = execute(
            &reference_client,
            &config.reference,
            scenario,
            config.max_response_bytes,
        )
        .await;
        let candidate = execute(
            &candidate_client,
            &config.candidate,
            scenario,
            config.max_response_bytes,
        )
        .await;
        let mut mismatches = Vec::new();

        match &reference {
            Ok(response) => validate_response(
                &config.reference.name,
                response,
                &scenario.expected,
                &mut mismatches,
            ),
            Err(error) => mismatches.push(Mismatch {
                target: config.reference.name.clone(),
                field: "transport".to_owned(),
                reason: error.public_message().to_owned(),
            }),
        }
        match &candidate {
            Ok(response) => validate_response(
                &config.candidate.name,
                response,
                &scenario.expected,
                &mut mismatches,
            ),
            Err(error) => mismatches.push(Mismatch {
                target: config.candidate.name.clone(),
                field: "transport".to_owned(),
                reason: error.public_message().to_owned(),
            }),
        }
        if let (Ok(reference), Ok(candidate)) = (&reference, &candidate) {
            compare_targets(reference, candidate, &scenario.expected, &mut mismatches);
        }
        reports.push(ScenarioReport {
            id: scenario.id.clone(),
            passed: mismatches.is_empty(),
            mismatches,
        });
    }
    Ok(RunReport {
        passed: reports.iter().all(|scenario| scenario.passed),
        scenario_count: reports.len(),
        scenarios: reports,
    })
}

fn build_client(request_timeout: Duration) -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        .cookie_store(true)
        .redirect(Policy::none())
        .no_proxy()
        .timeout(request_timeout)
        .user_agent("histae-contract-compare/0.1")
        .build()
        .map_err(|_| HarnessError::transport("could not build the HTTP client"))
}

fn validate_corpus(corpus: &ContractCorpus) -> Result<(), HarnessError> {
    if corpus.version != 1 {
        return Err(HarnessError::corpus("unsupported corpus version"));
    }
    if corpus.scenarios.is_empty() {
        return Err(HarnessError::corpus(
            "corpus must contain at least one scenario",
        ));
    }
    let mut ids = BTreeSet::new();
    for scenario in &corpus.scenarios {
        if !valid_label(&scenario.id) {
            return Err(HarnessError::corpus(
                "scenario ID must contain 1 to 64 safe characters",
            ));
        }
        if !ids.insert(&scenario.id) {
            return Err(HarnessError::corpus("scenario IDs must be unique"));
        }
        if !scenario.request.path.starts_with('/')
            || scenario.request.path.starts_with("//")
            || scenario.request.path.chars().any(char::is_whitespace)
        {
            return Err(HarnessError::corpus(
                "request path must be an absolute API path without whitespace",
            ));
        }
        scenario
            .request
            .method
            .parse::<Method>()
            .map_err(|_| HarnessError::corpus("request method is invalid"))?;
        for (name, value) in &scenario.request.headers {
            validate_request_header(name, value)?;
        }
        for name in scenario.expected.headers.keys() {
            name.parse::<HeaderName>()
                .map_err(|_| HarnessError::corpus("expected response header name is invalid"))?;
        }
        if let BodyExpectation::ExactJson {
            value,
            dynamic_fields,
        } = &scenario.expected.body
        {
            for pointer in dynamic_fields.keys() {
                if !pointer.starts_with('/') || value.pointer(pointer).is_none() {
                    return Err(HarnessError::corpus(
                        "dynamic JSON field must be a valid pointer present in the expected body",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_request_header(name: &str, value: &str) -> Result<(), HarnessError> {
    let name = name
        .parse::<HeaderName>()
        .map_err(|_| HarnessError::corpus("request header name is invalid"))?;
    if matches!(name.as_str(), "host" | "content-length" | "connection") {
        return Err(HarnessError::corpus(
            "request corpus cannot override transport-managed headers",
        ));
    }
    value
        .parse::<HeaderValue>()
        .map_err(|_| HarnessError::corpus("request header value is invalid"))?;
    Ok(())
}

async fn execute(
    client: &reqwest::Client,
    target: &Target,
    scenario: &Scenario,
    max_response_bytes: usize,
) -> Result<ObservedResponse, HarnessError> {
    let method = scenario
        .request
        .method
        .parse::<Method>()
        .map_err(|_| HarnessError::corpus("request method is invalid"))?;
    let url = target
        .base_url
        .join(scenario.request.path.trim_start_matches('/'))
        .map_err(|_| HarnessError::corpus("request path could not be joined to target"))?;
    let mut request = client.request(method, url);
    for (name, value) in &scenario.request.headers {
        request = request.header(name, value);
    }
    if let Some(body) = &scenario.request.body {
        request = match body {
            RequestBody::Json { value } => {
                let bytes = serde_json::to_vec(value)
                    .map_err(|_| HarnessError::corpus("JSON request body is invalid"))?;
                if !scenario
                    .request
                    .headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("content-type"))
                {
                    request = request.header("content-type", "application/json");
                }
                request.body(bytes)
            }
            RequestBody::Text { value } => request.body(value.clone()),
        };
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| HarnessError::transport("request failed"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(HarnessError::transport(
            "response exceeds the configured size limit",
        ));
    }
    let status = response.status().as_u16();
    let headers = collect_headers(response.headers());
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| HarnessError::transport("response body could not be read"))?
    {
        if body.len().saturating_add(chunk.len()) > max_response_bytes {
            return Err(HarnessError::transport(
                "response exceeds the configured size limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(ObservedResponse {
        status,
        headers,
        body,
    })
}

fn collect_headers(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    for name in headers.keys() {
        let values = headers
            .get_all(name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|_| "<non-utf8>".to_owned())
            })
            .collect();
        result.insert(name.as_str().to_owned(), values);
    }
    result
}

fn validate_response(
    target: &str,
    response: &ObservedResponse,
    expected: &ResponseExpectation,
    mismatches: &mut Vec<Mismatch>,
) {
    if response.status != expected.status {
        mismatches.push(Mismatch {
            target: target.to_owned(),
            field: "status".to_owned(),
            reason: format!("expected {}, received {}", expected.status, response.status),
        });
    }
    for (name, rule) in &expected.headers {
        let key = name.to_ascii_lowercase();
        let values = response.headers.get(&key);
        let valid = match rule {
            HeaderExpectation::Exact { value } => {
                values.is_some_and(|values| values.len() == 1 && values[0] == *value)
            }
            HeaderExpectation::Contains { value } => values
                .is_some_and(|values| values.iter().any(|candidate| candidate.contains(value))),
            HeaderExpectation::Present => values.is_some_and(|values| !values.is_empty()),
            HeaderExpectation::UuidV4 => {
                values.is_some_and(|values| values.len() == 1 && is_canonical_uuid_v4(&values[0]))
            }
        };
        if !valid {
            mismatches.push(Mismatch {
                target: target.to_owned(),
                field: format!("header:{key}"),
                reason: "response header does not satisfy its contract rule".to_owned(),
            });
        }
    }
    match &expected.body {
        BodyExpectation::ExactJson {
            value,
            dynamic_fields,
        } => match serde_json::from_slice::<Value>(&response.body) {
            Ok(actual) => {
                validate_dynamic_json_fields(target, &actual, dynamic_fields, mismatches);
                if normalized_json(actual, dynamic_fields)
                    != normalized_json(value.clone(), dynamic_fields)
                {
                    mismatches.push(Mismatch {
                        target: target.to_owned(),
                        field: "body".to_owned(),
                        reason: "JSON body differs from the expected contract".to_owned(),
                    });
                }
            }
            Err(_) => mismatches.push(Mismatch {
                target: target.to_owned(),
                field: "body".to_owned(),
                reason: "response body is not valid JSON".to_owned(),
            }),
        },
        BodyExpectation::ExactText { value } => {
            if response.body != value.as_bytes() {
                mismatches.push(Mismatch {
                    target: target.to_owned(),
                    field: "body".to_owned(),
                    reason: "text body differs from the expected contract".to_owned(),
                });
            }
        }
        BodyExpectation::Empty => {
            if !response.body.is_empty() {
                mismatches.push(Mismatch {
                    target: target.to_owned(),
                    field: "body".to_owned(),
                    reason: "response body must be empty".to_owned(),
                });
            }
        }
    }
}

fn compare_targets(
    reference: &ObservedResponse,
    candidate: &ObservedResponse,
    expected: &ResponseExpectation,
    mismatches: &mut Vec<Mismatch>,
) {
    if reference.status != candidate.status {
        mismatches.push(Mismatch {
            target: "comparison".to_owned(),
            field: "status".to_owned(),
            reason: "reference and candidate status codes differ".to_owned(),
        });
    }
    for (name, rule) in &expected.headers {
        if matches!(rule, HeaderExpectation::Exact { .. }) {
            let key = name.to_ascii_lowercase();
            if reference.headers.get(&key) != candidate.headers.get(&key) {
                mismatches.push(Mismatch {
                    target: "comparison".to_owned(),
                    field: format!("header:{key}"),
                    reason: "reference and candidate headers differ".to_owned(),
                });
            }
        }
    }
    let equal = match &expected.body {
        BodyExpectation::ExactJson { dynamic_fields, .. } => {
            match (
                serde_json::from_slice::<Value>(&reference.body),
                serde_json::from_slice::<Value>(&candidate.body),
            ) {
                (Ok(reference), Ok(candidate)) => {
                    normalized_json(reference, dynamic_fields)
                        == normalized_json(candidate, dynamic_fields)
                }
                _ => false,
            }
        }
        BodyExpectation::ExactText { .. } | BodyExpectation::Empty => {
            reference.body == candidate.body
        }
    };
    if !equal {
        mismatches.push(Mismatch {
            target: "comparison".to_owned(),
            field: "body".to_owned(),
            reason: "reference and candidate bodies differ".to_owned(),
        });
    }
}

fn validate_dynamic_json_fields(
    target: &str,
    actual: &Value,
    fields: &BTreeMap<String, DynamicJsonField>,
    mismatches: &mut Vec<Mismatch>,
) {
    for (pointer, rule) in fields {
        let valid = actual.pointer(pointer).is_some_and(|value| match rule {
            DynamicJsonField::Any => true,
            DynamicJsonField::UuidV4 => value.as_str().is_some_and(is_canonical_uuid_v4),
            DynamicJsonField::NonEmptyString => {
                value.as_str().is_some_and(|value| !value.is_empty())
            }
            DynamicJsonField::Integer => value
                .as_number()
                .is_some_and(|value| value.is_i64() || value.is_u64()),
        });
        if !valid {
            mismatches.push(Mismatch {
                target: target.to_owned(),
                field: format!("body:{pointer}"),
                reason: "dynamic JSON field does not satisfy its contract rule".to_owned(),
            });
        }
    }
}

fn normalized_json(mut value: Value, fields: &BTreeMap<String, DynamicJsonField>) -> Value {
    for pointer in fields.keys() {
        if let Some(field) = value.pointer_mut(pointer) {
            *field = Value::String(format!("<dynamic:{pointer}>"));
        }
    }
    value
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn is_canonical_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
        })
}

#[derive(Debug)]
pub struct HarnessError {
    kind: &'static str,
    message: &'static str,
}

impl HarnessError {
    fn configuration(message: &'static str) -> Self {
        Self {
            kind: "configuration",
            message,
        }
    }

    fn corpus(message: &'static str) -> Self {
        Self {
            kind: "corpus",
            message,
        }
    }

    fn transport(message: &'static str) -> Self {
        Self {
            kind: "transport",
            message,
        }
    }

    pub fn public_message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} error: {}", self.kind, self.message)
    }
}

impl Error for HarnessError {}
