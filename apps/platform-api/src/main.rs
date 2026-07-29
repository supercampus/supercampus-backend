#![forbid(unsafe_code)]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    supercampus_platform_api::run().await
}
