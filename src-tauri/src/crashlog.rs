//! Panics are written to a log file (message, code location, thread, backtrace) so a crash in a
//! release build can be diagnosed: macOS `~/Library/Logs/DevCat/panic.log`, elsewhere
//! `<local data dir>/dev.devcat.app/logs/panic.log`. Release builds unwind (Cargo.toml), so a
//! panic in a background task fails only that task instead of the whole app.
use std::io::Write;
use std::path::PathBuf;

const MAX_BYTES: u64 = 512 * 1024;

pub fn log_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs/DevCat/panic.log"))
    } else {
        dirs::data_local_dir().map(|d| d.join(crate::APP_ID).join("logs/panic.log"))
    }
}

pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = log_path() {
            let _ = write_entry(&path, &entry(info));
        }
        default(info);
    }));
}

fn entry(info: &std::panic::PanicHookInfo<'_>) -> String {
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(non-string panic payload)".into());
    let loc = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    format!(
        "=== {} DevCat {} ({}) thread '{thread}'\npanic at {loc}: {msg}\n{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        env!("CARGO_PKG_VERSION"),
        crate::network::helper::version(),
        std::backtrace::Backtrace::force_capture(),
    )
}

fn write_entry(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Keep the file small: start over when it grows past MAX_BYTES.
    let truncate = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES);
    let mut f = std::fs::OpenOptions::new().create(true).append(!truncate).write(true).truncate(truncate).open(path)?;
    f.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_and_caps_the_log() {
        let d = std::env::temp_dir().join(format!("devcat-crashlog-{}", uuid::Uuid::new_v4()));
        let p = d.join("panic.log");
        super::write_entry(&p, "one\n").unwrap();
        super::write_entry(&p, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
        super::write_entry(&p, &"x".repeat(super::MAX_BYTES as usize + 1)).unwrap();
        super::write_entry(&p, "fresh\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(d);
    }
}
