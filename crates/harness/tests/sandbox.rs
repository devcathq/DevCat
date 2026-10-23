//! The OS sandbox on macOS and Linux: commands write in the project only, and read anywhere.
#![cfg(any(target_os = "macos", target_os = "linux"))]
use devcat_harness::tools::{Git, Shell, Tool, ToolCtx};
use serde_json::json;
use std::path::PathBuf;

/// A project and a folder next to it, both in the home folder (the temp folders are writable).
fn folders() -> (PathBuf, PathBuf, PathBuf) {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let root = home.join(format!("devcat-sandbox-{}", uuid::Uuid::new_v4()));
    let (proj, outside) = (root.join("proj"), root.join("outside"));
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "still here\n").unwrap();
    (root, proj, outside)
}

fn usable() -> bool {
    match devcat_harness::sandbox::available() {
        Ok(()) => true,
        // macOS always has it; a Linux kernel may not
        Err(e) if cfg!(target_os = "linux") => {
            eprintln!("skipped, no sandbox here: {e}");
            false
        }
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test]
async fn sandboxed_commands_write_only_in_the_project() {
    if !usable() {
        return;
    }
    let (root, proj, outside) = folders();
    let ctx = ToolCtx { cwd: proj.clone(), sandbox: true };
    let sh = |command: String| Shell.run(json!({ "command": command }), &ctx);

    let inside = sh("echo inside > in.txt && cat in.txt && echo quiet > /dev/null".into()).await;
    assert!(!inside.is_error && inside.content.contains("inside"), "{inside:?}");
    assert!(proj.join("in.txt").exists());

    let out = sh(format!("echo out > '{}'", outside.join("x.txt").display())).await;
    assert!(out.is_error, "writing outside the project should fail: {out:?}");
    assert!(!outside.join("x.txt").exists());

    let rm = sh(format!("rm '{}'", outside.join("keep.txt").display())).await;
    assert!(rm.is_error && outside.join("keep.txt").exists(), "{rm:?}");

    // reading anywhere is fine, and so are the temp folders
    let read = sh(format!("cat '{}'", outside.join("keep.txt").display())).await;
    assert!(!read.is_error && read.content.contains("still here"), "{read:?}");
    let tmp = sh("f=$(mktemp) && echo t > \"$f\" && cat \"$f\" && rm \"$f\"".into()).await;
    assert!(!tmp.is_error && tmp.content.contains('t'), "{tmp:?}");

    // git works in the project
    let git = |args: &[&str]| Git.run(json!({ "args": args }), &ctx);
    assert!(!git(&["init", "-q"]).await.is_error);
    // the identity goes in the project's own .git/config (the tool takes the subcommand first)
    assert!(!git(&["config", "user.email", "a@b.c"]).await.is_error);
    assert!(!git(&["config", "user.name", "A"]).await.is_error);
    let commit = git(&["commit", "-q", "--allow-empty", "-m", "sandboxed"]).await;
    assert!(!commit.is_error, "{commit:?}");
    assert!(git(&["log", "--oneline"]).await.content.contains("sandboxed"));

    // the same command without the sandbox writes outside
    let free = Shell.run(json!({ "command": format!("echo out > '{}'", outside.join("y.txt").display()) }), &ToolCtx::at(&proj)).await;
    assert!(!free.is_error && outside.join("y.txt").exists(), "{free:?}");
    let _ = std::fs::remove_dir_all(root);
}
