//! How children of the app start and end on Windows: never in the app's own
//! folder (which would hold its install open), and never outliving it.
#![cfg(windows)]

use std::path::{Path, PathBuf};

/// What `cmd /C cd` prints for `command`: the folder the child started in.
fn child_cwd(command: &mut std::process::Command) -> PathBuf {
    let output = command.args(["/C", "cd"]).output().unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}

#[test]
fn children_start_in_a_neutral_folder_not_the_apps() {
    let neutral = pi_rpc::neutral_dir().expect("a neutral folder");
    assert!(neutral.is_dir());
    let here = std::env::current_dir().unwrap();
    assert!(!same_dir(neutral, &here));

    let started = child_cwd(&mut pi_rpc::command("cmd"));
    assert!(
        same_dir(&started, neutral),
        "started in {started:?}, expected {neutral:?}"
    );
}

#[test]
fn an_explicit_working_folder_still_wins() {
    let dir = std::env::temp_dir();
    let mut command = pi_rpc::command("cmd");
    command.current_dir(&dir);
    let started = child_cwd(&mut command);
    assert!(same_dir(&started, &dir), "started in {started:?}");
}
