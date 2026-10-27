#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ticket_tracker::server::run().await
}
