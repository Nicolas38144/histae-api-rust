#[tokio::main]
async fn main() -> std::process::ExitCode {
    histae_api_rust::bootstrap::binary_main(histae_api_rust::bootstrap::Component::AdminBootstrap)
        .await
}
