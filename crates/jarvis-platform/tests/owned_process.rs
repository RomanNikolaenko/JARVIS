//! Regression coverage for descendants surviving their root process.
#![allow(clippy::expect_used)]

use std::process::{Command, Stdio};
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MODE: &str = "JARVIS_OWNED_TEST_MODE";
const PID_FILE: &str = "JARVIS_OWNED_TEST_PID";

fn fixture(mode: &str, pid_file: &std::path::Path) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--ignored", "--exact", "process_fixture", "--nocapture"])
        .env(MODE, mode)
        .env(PID_FILE, pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    jarvis_platform::current_adapter().prepare_background_command(&mut command);
    command
}

#[test]
#[ignore = "subprocess fixture, called by the ownership tests"]
fn process_fixture() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let path = std::path::PathBuf::from(std::env::var_os(PID_FILE).expect("pid path"));
    if mode == "leaf" {
        std::fs::write(path, std::process::id().to_string()).expect("write leaf pid");
        std::thread::sleep(Duration::from_secs(30));
    } else {
        let mut child = fixture("leaf", &path).spawn().expect("spawn descendant");
        // Reaping on a separate thread intentionally does not keep the root alive.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if mode == "live-root" {
            std::thread::sleep(Duration::from_secs(30));
        }
    }
}

fn running(pid: u32) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(Pid::from_u32(pid)).is_some_and(|process| {
        !matches!(
            process.status(),
            sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
        )
    })
}

async fn descendant(path: &std::path::Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("descendant started")
}

async fn stopped(pid: u32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while running(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("descendant stopped");
}

#[tokio::test]
async fn dropping_an_owned_process_stops_a_live_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("leaf.pid");
    let child = jarvis_platform::spawn_owned(fixture("live-root", &path)).expect("spawn root");
    let root = child.id().expect("root pid");
    let leaf = descendant(&path).await;
    assert!(running(leaf));
    drop(child);
    stopped(leaf).await;
    stopped(root).await;
}

#[tokio::test]
async fn root_exit_does_not_release_ownership_of_descendants() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("leaf.pid");
    let mut child =
        jarvis_platform::spawn_owned(fixture("exiting-root", &path)).expect("spawn root");
    tokio::time::timeout(Duration::from_secs(10), child.wait_root())
        .await
        .expect("root exited without waiting for leaf")
        .expect("root status");
    let leaf = descendant(&path).await;
    assert!(running(leaf));
    drop(child);
    stopped(leaf).await;
}
