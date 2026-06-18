use std::{path::Path, sync::Arc};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name: &str| {
        args.windows(2).find(|v|v[0]==name).map(|v|v[1].clone()).ok_or_else(||format!("Required: {name}; usage: kageha-memory --state PATH --bind 127.0.0.1:PORT --token-file PATH"))
    };
    let token = std::fs::read_to_string(arg("--token-file")?)?
        .trim()
        .to_owned();
    let port = Arc::new(kageha_memory::sqlite::SqliteMemory::open(Path::new(&arg(
        "--state",
    )?))?);
    let listener = tokio::net::TcpListener::bind(arg("--bind")?).await?;
    eprintln!("memory listening on {}", listener.local_addr()?);
    axum::serve(listener, kageha_memory::http::router(port, token)?)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
