//! How children of the app start and end on Windows: never in the app's own
//! folder (which would hold its install open), and never outliving it.
#![cfg(windows)]

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set when this test binary runs as [`app_with_children`], the stand-in
/// for the app in [`contained_children_end_with_the_app`]: the file it
/// writes its children's pids to (its stdout would be inherited by them).
const APP: &str = "PI_RPC_TEST_APP_WITH_CHILDREN";

/// A console program that runs for a minute, then exits.
fn long_running() -> Command {
    let mut command = pi_rpc::command("ping");
    command
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Not a test of its own: the app [`contained_children_end_with_the_app`]
/// starts and lets exit. It starts a contained child, a contained shell that
/// then starts a grandchild (as a backend's extension starts `engram serve`),
/// and a child left out (an editor opened for the user), prints their pids
/// and exits without stopping any of them.
#[test]
fn app_with_children() {
    let Some(pids) = std::env::var_os(APP) else {
        return;
    };
    let contained = long_running().spawn().unwrap();
    pi_rpc::contain(&contained);
    let mut shell = pi_rpc::command("powershell");
    shell
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Start-Process ping -ArgumentList '-n','60','127.0.0.1' \
             -WindowStyle Hidden -PassThru).Id; Start-Sleep 60",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut shell = shell.spawn().unwrap();
    pi_rpc::contain(&shell);
    let mut grandchild = String::new();
    std::io::BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut grandchild)
        .unwrap();
    let free = long_running().spawn().unwrap();
    let line = format!(
        "{} {} {} {}",
        contained.id(),
        shell.id(),
        grandchild.trim(),
        free.id()
    );
    std::fs::write(pids, line).unwrap();
    std::process::exit(0);
}

fn running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: the handle is only queried, then closed.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(process);
        alive
    }
}

/// When the app ends (it exits or crashes), every process it contained ends
/// with it, grandchildren included; a process it left out keeps running.
#[test]
fn contained_children_end_with_the_app() {
    let file = std::env::temp_dir().join(format!("pi-rpc-job-{}", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "app_with_children", "--test-threads=1"])
        .env(APP, &file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    let pids: Vec<u32> = text
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    let [child, shell, grandchild, free] = pids[..] else {
        panic!("expected four pids, got {pids:?}");
    };
    let contained = [child, shell, grandchild];
    let deadline = Instant::now() + Duration::from_secs(10);
    while contained.iter().any(|pid| running(*pid)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let survivors: Vec<u32> = contained
        .iter()
        .copied()
        .filter(|pid| running(*pid))
        .collect();
    let free_alive = running(free);
    for pid in survivors.iter().chain([&free]) {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    assert!(survivors.is_empty(), "outlived the app: {survivors:?}");
    assert!(
        free_alive,
        "a process left out of the job ended with the app"
    );
}

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
