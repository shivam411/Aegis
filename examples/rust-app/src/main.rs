#[tokio::main]
async fn main() {
    println!("Aegis Sample Rust Service starting on port 8080...");
    // Simulating HTTP service
    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
}
