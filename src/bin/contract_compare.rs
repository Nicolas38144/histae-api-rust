use std::{env, fs, process::ExitCode, time::Duration};

use histae_api_rust::contract::{
    ContractCorpus, DEFAULT_MAX_RESPONSE_BYTES, RunConfig, Target, run_corpus,
};

#[tokio::main]
async fn main() -> ExitCode {
    match execute().await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("contract_compare_failed code={message}");
            ExitCode::from(2)
        }
    }
}

async fn execute() -> Result<bool, &'static str> {
    let arguments = Arguments::parse(env::args().skip(1))?;
    let source = fs::read_to_string(&arguments.corpus).map_err(|_| "corpus_read_failed")?;
    let corpus: ContractCorpus =
        serde_json::from_str(&source).map_err(|_| "corpus_parse_failed")?;
    let reference = Target::parse(
        "reference",
        &arguments.reference_url,
        arguments.reference_state,
    )
    .map_err(|_| "reference_configuration_invalid")?;
    let candidate = Target::parse(
        "candidate",
        &arguments.candidate_url,
        arguments.candidate_state,
    )
    .map_err(|_| "candidate_configuration_invalid")?;
    let report = run_corpus(
        &corpus,
        &RunConfig {
            reference,
            candidate,
            allow_isolated_mutations: arguments.allow_isolated_mutations,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            request_timeout: Duration::from_secs(10),
        },
    )
    .await
    .map_err(|_| "contract_run_failed")?;
    let output = serde_json::to_string_pretty(&report).map_err(|_| "report_serialize_failed")?;
    println!("{output}");
    Ok(report.passed)
}

struct Arguments {
    corpus: String,
    reference_url: String,
    reference_state: String,
    candidate_url: String,
    candidate_state: String,
    allow_isolated_mutations: bool,
}

impl Arguments {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, &'static str> {
        let mut corpus = None;
        let mut reference_url = None;
        let mut reference_state = None;
        let mut candidate_url = None;
        let mut candidate_state = None;
        let mut allow_isolated_mutations = false;
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            let destination = match argument.as_str() {
                "--corpus" => &mut corpus,
                "--reference-url" => &mut reference_url,
                "--reference-state" => &mut reference_state,
                "--candidate-url" => &mut candidate_url,
                "--candidate-state" => &mut candidate_state,
                "--allow-isolated-mutations" => {
                    allow_isolated_mutations = true;
                    continue;
                }
                _ => return Err("invalid_argument"),
            };
            if destination.is_some() {
                return Err("duplicate_argument");
            }
            *destination = Some(arguments.next().ok_or("missing_argument_value")?);
        }
        Ok(Self {
            corpus: corpus.ok_or("missing_corpus")?,
            reference_url: reference_url.ok_or("missing_reference_url")?,
            reference_state: reference_state.ok_or("missing_reference_state")?,
            candidate_url: candidate_url.ok_or("missing_candidate_url")?,
            candidate_state: candidate_state.ok_or("missing_candidate_state")?,
            allow_isolated_mutations,
        })
    }
}
