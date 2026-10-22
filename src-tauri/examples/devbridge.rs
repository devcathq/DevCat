//! Dev-only: serve git/fs commands over HTTP for browser-based UI testing (see src/devbridge.rs).
#[tokio::main]
async fn main() -> std::io::Result<()> {
    // ssh asking for a saved session's password (this binary is SSH_ASKPASS here, like the app).
    if let Some(code) = devcat_lib::askpass_main() {
        std::process::exit(code);
    }
    let port = std::env::var("DEVBRIDGE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1430);
    devcat_lib::devbridge::serve(port).await
}
