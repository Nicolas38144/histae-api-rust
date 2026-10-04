#[tokio::main]
async fn main() -> std::process::ExitCode {
    histae_api_rust::app::binary_main(histae_api_rust::app::Component::Outbox).await
}
