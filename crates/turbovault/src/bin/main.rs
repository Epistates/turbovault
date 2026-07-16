//! TurboVault Server CLI entry point.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    turbovault::cli::run_from_env().await
}
