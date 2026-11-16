// No commands for the web page: the app's Rust side drives the service (src/lib.rs).
fn main() {
    tauri_plugin::Builder::new(&[]).android_path("android").build();
}
