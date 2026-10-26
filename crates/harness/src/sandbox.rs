//! The OS sandbox for the commands the agent runs (`shell`, `git`): when it's on, a command can
//! read anything but write only in the project folder, the temp folders and `/dev` (`/dev/null`,
//! the terminal). It limits writes, not reading or the network, so commands still ask as commands.
//! On macOS a shell command can't write in the project's `.git` either (hooks and config there run
//! on the next git call, outside the sandbox); the `git` tool can. Landlock can't leave out a
//! folder of an allowed one, so on Linux `.git` stays writable.
//!
//! - macOS: Seatbelt, through `sandbox-exec` with a profile that denies file writes elsewhere.
//! - Linux: Landlock (Linux 5.13+). The ruleset is built in DevCat; the child only turns it on
//!   (two system calls) between fork and exec.
//! - Windows: not yet.
//!
//! It fails closed: a command that should run sandboxed and can't is not run.
use std::path::{Path, PathBuf};

/// Whether this computer can sandbox commands, and why not.
pub fn available() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if Path::new(SANDBOX_EXEC).exists() { Ok(()) } else { Err(format!("{SANDBOX_EXEC} is missing")) }
    }
    #[cfg(target_os = "linux")]
    {
        landlock::abi().map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("The sandbox works on macOS and Linux; Windows support comes later.".into())
    }
}

/// Folders a sandboxed command may write in: the project, the temp folders and `/dev`, as they
/// really are (symlinks resolved: on macOS `/tmp` is `/private/tmp`).
pub fn writable(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.to_path_buf(), std::env::temp_dir()];
    if cfg!(unix) {
        dirs.extend(["/tmp", "/var/tmp", "/dev"].map(PathBuf::from));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if let Ok(real) = std::fs::canonicalize(&d)
            && !out.contains(&real)
        {
            out.push(real);
        }
    }
    out
}

/// Kept until the command has started (Linux: the ruleset it turns on).
pub struct Guard {
    #[cfg(target_os = "linux")]
    _ruleset: Option<std::os::fd::OwnedFd>,
}

/// `program args` as a command, inside the sandbox when `sandboxed` (writes only in `cwd` and the
/// folders of [`writable`]; with `protect_git`, not in `cwd`'s `.git` on macOS).
pub fn command(program: &str, args: &[String], cwd: &Path, sandboxed: bool, protect_git: bool) -> Result<(tokio::process::Command, Guard), String> {
    if !sandboxed {
        let mut c = tokio::process::Command::new(program);
        c.args(args);
        return Ok((c, guard_none()));
    }
    #[cfg(target_os = "macos")]
    {
        available()?;
        let mut c = tokio::process::Command::new(SANDBOX_EXEC);
        let git: Vec<PathBuf> = if protect_git { std::fs::canonicalize(cwd.join(".git")).into_iter().collect() } else { vec![] };
        c.args(["-p", &seatbelt_profile(&writable(cwd), &git), "--", program]).args(args);
        Ok((c, Guard {}))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = protect_git;
        let ruleset = landlock::ruleset(&writable(cwd))?;
        let fd = std::os::fd::AsRawFd::as_raw_fd(&ruleset);
        let mut c = tokio::process::Command::new(program);
        c.args(args);
        // SAFETY: only system calls between fork and exec, nothing that allocates or locks
        unsafe {
            c.pre_exec(move || landlock::restrict_self(fd));
        }
        Ok((c, Guard { _ruleset: Some(ruleset) }))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (program, args, cwd, protect_git);
        Err(available().unwrap_err())
    }
}

fn guard_none() -> Guard {
    Guard {
        #[cfg(target_os = "linux")]
        _ruleset: None,
    }
}

#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Everything is allowed but writing files, which is allowed only beneath `writable`, and not
/// beneath `read_only` after all. (In Seatbelt profiles the last rule that matches wins.)
#[cfg(any(target_os = "macos", test))]
fn seatbelt_profile(writable: &[PathBuf], read_only: &[PathBuf]) -> String {
    let quote = |p: &Path| p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let paths = |list: &[PathBuf]| list.iter().map(|p| format!("(subpath \"{}\")", quote(p))).collect::<Vec<_>>().join(" ");
    let mut p = format!("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* {})\n", paths(writable));
    if !read_only.is_empty() {
        p.push_str(&format!("(deny file-write* {})\n", paths(read_only)));
    }
    p
}

#[cfg(target_os = "linux")]
mod landlock {
    //! Landlock without a crate: three system calls (the numbers are the same on every Linux
    //! architecture) and the two structures they take.
    use std::ffi::{c_long, c_ulong};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;

    const CREATE_RULESET: c_long = 444;
    const ADD_RULE: c_long = 445;
    const RESTRICT_SELF: c_long = 446;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: u32 = 1;
    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    const O_PATH: i32 = 0o10_000_000;

    // what writing takes (reading, running and listing stay free)
    const WRITE_FILE: u64 = 1 << 1;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    /// ABI 2: moving and linking files between folders.
    const REFER: u64 = 1 << 13;
    /// ABI 3: truncating files.
    const TRUNCATE: u64 = 1 << 14;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneath {
        allowed_access: u64,
        parent_fd: i32,
    }

    unsafe extern "C" {
        fn syscall(num: c_long, ...) -> c_long;
        fn prctl(option: i32, ...) -> i32;
    }

    /// The Landlock version this kernel speaks.
    pub fn abi() -> Result<c_long, String> {
        // SAFETY: asks for the version; no memory is passed
        let v = unsafe { syscall(CREATE_RULESET, std::ptr::null::<RulesetAttr>(), 0usize, CREATE_RULESET_VERSION) };
        if v < 1 {
            return Err(format!("Landlock isn't available on this Linux ({}); it needs Linux 5.13 or newer with Landlock enabled.", io::Error::last_os_error()));
        }
        Ok(v)
    }

    /// A ruleset that leaves writing only beneath `writable`.
    pub fn ruleset(writable: &[PathBuf]) -> Result<OwnedFd, String> {
        let abi = abi()?;
        let mut handled = WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM;
        if abi >= 2 {
            handled |= REFER;
        }
        if abi >= 3 {
            handled |= TRUNCATE;
        }
        let attr = RulesetAttr { handled_access_fs: handled };
        // SAFETY: `attr` is valid for its size
        let fd = unsafe { syscall(CREATE_RULESET, &attr as *const RulesetAttr, std::mem::size_of::<RulesetAttr>(), 0u32) };
        if fd < 0 {
            return Err(format!("Couldn't set up the sandbox: {}", io::Error::last_os_error()));
        }
        // SAFETY: a new descriptor we own
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        for dir in writable {
            let Ok(f) = std::fs::OpenOptions::new().read(true).custom_flags(O_PATH).open(dir) else { continue };
            let rule = PathBeneath { allowed_access: handled, parent_fd: f.as_raw_fd() };
            // SAFETY: `rule` is valid for the call; `f` stays open until after it
            let r = unsafe { syscall(ADD_RULE, ruleset.as_raw_fd(), RULE_PATH_BENEATH, &rule as *const PathBeneath, 0u32) };
            if r != 0 {
                return Err(format!("Couldn't allow writing in {}: {}", dir.display(), io::Error::last_os_error()));
            }
        }
        Ok(ruleset)
    }

    /// In the child, between fork and exec: no new privileges, then the ruleset.
    pub fn restrict_self(ruleset: i32) -> io::Result<()> {
        // SAFETY: plain system calls
        unsafe {
            if prctl(PR_SET_NO_NEW_PRIVS, 1 as c_ulong, 0 as c_ulong, 0 as c_ulong, 0 as c_ulong) != 0 {
                return Err(io::Error::last_os_error());
            }
            if syscall(RESTRICT_SELF, ruleset, 0u32) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_allows_writes_only_beneath_the_folders() {
        let p = seatbelt_profile(&[PathBuf::from("/Users/me/my \"proj\""), PathBuf::from("/private/tmp")], &[]);
        assert!(p.starts_with("(version 1)\n(allow default)\n(deny file-write*)\n"));
        assert!(p.contains(r#"(allow file-write* (subpath "/Users/me/my \"proj\"") (subpath "/private/tmp"))"#), "{p}");
        // .git after the project, so it wins
        let p = seatbelt_profile(&[PathBuf::from("/p")], &[PathBuf::from("/p/.git")]);
        assert!(p.ends_with("(allow file-write* (subpath \"/p\"))\n(deny file-write* (subpath \"/p/.git\"))\n"), "{p}");
    }

    #[test]
    fn writable_folders_are_resolved_and_unique() {
        let cwd = std::env::temp_dir();
        let w = writable(&cwd);
        assert!(!w.is_empty() && w.iter().all(|p| p.is_absolute()));
        let mut d = w.clone();
        d.dedup();
        assert_eq!(d.len(), w.len());
    }
}
