// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(all(not(debug_assertions), feature = "gui"), windows_subsystem = "windows")]

fn main() {
    // ssh asking for a saved SSH session's password: see claude/remote.rs.
    if let Some(code) = devcat_lib::askpass_main() {
        std::process::exit(code);
    }
    // Helper mode runs before (and instead of) anything Tauri: see network/helper.rs.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--vpn-helper") {
        let args: Option<Vec<String>> = std::env::args_os().skip(2).map(|a| a.into_string().ok()).collect();
        let code = match args {
            Some(args) => devcat_lib::vpn_helper_main(&args),
            None => {
                eprintln!("DevCat's passwordless helper: arguments must be valid UTF-8");
                10
            }
        };
        std::process::exit(code);
    }
    if let Some(why) = devcat_lib::gui_refusal() {
        eprintln!("{why}");
        std::process::exit(1);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = devcat_lib::cli_main(&args) {
        std::process::exit(code);
    }
    #[cfg(feature = "gui")]
    devcat_lib::run();
    // The CLI build: no app to start.
    #[cfg(not(feature = "gui"))]
    std::process::exit(devcat_lib::cli_main(&["help".to_string()]).unwrap_or(2));
}
