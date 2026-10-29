mod account;
mod crypto;
mod deezer;
mod native;
mod plugin;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    native::run().await
}
