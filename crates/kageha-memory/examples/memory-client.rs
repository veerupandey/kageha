use kageha_contracts::{MemoryAccess, MemoryPort};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = kageha_memory::http::HttpMemory::new(
        &std::env::var("KAGEHA_MEMORY_ENDPOINT")?,
        std::env::var("KAGEHA_MEMORY_TOKEN")?,
    )?;
    println!("{}", serde_json::to_string(&client.descriptor().await?)?);
    println!(
        "{}",
        serde_json::to_string(
            &client
                .search(
                    &MemoryAccess {
                        namespace: "example".into(),
                        actor: "example".into()
                    },
                    "",
                    10
                )
                .await?
        )?
    );
    Ok(())
}
