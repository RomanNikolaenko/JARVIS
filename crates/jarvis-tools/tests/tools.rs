//! Built-in tools against real files and processes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jarvis_security::{FileScope, RiskLevel};
use jarvis_tools::{
    DEFAULT_ENABLED, ProcessRegistry, ToolContext, ToolError, ToolLimits, ToolOutput, ToolRegistry,
    resolve_program,
};
use serde_json::{Value, json};

struct Workspace {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    ctx: ToolContext,
    tools: ToolRegistry,
}

fn workspace() -> Workspace {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let scope = FileScope::new([root.clone()], [dir.path().join("jarvis-data")]);
    let root = scope.roots()[0].clone();
    Workspace {
        _dir: dir,
        root,
        ctx: ToolContext::new(
            scope,
            ToolLimits::default(),
            Arc::new(ProcessRegistry::default()),
        ),
        tools: ToolRegistry::builtin(),
    }
}

impl Workspace {
    fn prepare(&self, tool: &str, input: Value) -> Result<jarvis_tools::PreparedCall, ToolError> {
        self.tools
            .get(tool)
            .expect("tool")
            .prepare(&input, &self.ctx)
    }

    async fn call(&self, tool: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let call = self.prepare(tool, input)?;
        self.tools
            .get(tool)
            .expect("tool")
            .run(&call, &self.ctx)
            .await
    }
}

fn sleeper(seconds: u32) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "ping",
            vec!["-n".into(), (seconds + 1).to_string(), "127.0.0.1".into()],
        )
    } else {
        ("sleep", vec![seconds.to_string()])
    }
}

#[test]
fn the_registry_lists_every_builtin_tool() {
    let specs = ToolRegistry::builtin().specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    for name in [
        "list_directory",
        "read_file",
        "search_files",
        "write_file",
        "edit_file",
        "copy_path",
        "move_path",
        "delete_path",
        "system_info",
        "list_processes",
        "launch_program",
        "stop_program",
        "run_command",
        "fetch_url",
        "open_url",
        "git",
    ] {
        assert!(names.contains(&name), "{name}");
    }
    for name in DEFAULT_ENABLED {
        let spec = specs
            .iter()
            .find(|s| s.name == *name)
            .expect("default tool exists");
        assert_eq!(
            spec.risk,
            RiskLevel::Low,
            "{name} is enabled by default, so it must only read"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn file_tools_work_inside_the_workspace() {
    let w = workspace();
    let created = w
        .call(
            "write_file",
            json!({"path": "notes/todo.md", "content": "# Todo\n- ship Atlas\n- write docs\n"}),
        )
        .await
        .expect("create");
    assert!(created.summary.contains("todo.md"));
    assert!(w.root.join("notes/todo.md").is_file());

    // Overwriting needs the flag and is a high-risk action.
    assert!(matches!(
        w.prepare(
            "write_file",
            json!({"path": "notes/todo.md", "content": "x"})
        ),
        Err(ToolError::InvalidInput(_))
    ));
    let overwrite = w
        .prepare(
            "write_file",
            json!({"path": "notes/todo.md", "content": "x", "overwrite": true}),
        )
        .expect("prepare");
    assert_eq!(overwrite.action.risk, RiskLevel::High);

    let read = w
        .call(
            "read_file",
            json!({"path": "notes/todo.md", "startLine": 2, "maxLines": 1}),
        )
        .await
        .expect("read");
    assert_eq!(read.content, "- ship Atlas");

    w.call(
        "edit_file",
        json!({"path": "notes/todo.md", "find": "ship Atlas", "replace": "ship Atlas v2"}),
    )
    .await
    .expect("edit");
    assert!(
        std::fs::read_to_string(w.root.join("notes/todo.md"))
            .unwrap()
            .contains("Atlas v2")
    );
    assert!(matches!(
        w.call(
            "edit_file",
            json!({"path": "notes/todo.md", "find": "missing", "replace": "x"})
        )
        .await,
        Err(ToolError::Failed(_))
    ));

    let listing = w.call("list_directory", json!({})).await.expect("list");
    assert!(listing.content.contains("notes/"));
    // Models often send an empty path for "the workspace" (seen live).
    let blank = w
        .call("list_directory", json!({"path": " "}))
        .await
        .expect("an empty path lists the workspace");
    assert_eq!(blank.content, listing.content);
    w.call("search_files", json!({"path": "", "text": "docs"}))
        .await
        .expect("an empty path searches the workspace");

    let by_name = w
        .call("search_files", json!({"name": "*.md"}))
        .await
        .expect("search by name");
    assert!(by_name.content.contains("todo.md"));
    let by_text = w
        .call("search_files", json!({"text": "WRITE DOCS"}))
        .await
        .expect("search text");
    // The model sees paths relative to the workspace folder.
    assert_eq!(by_text.content, "notes/todo.md:3: - write docs");
    assert!(listing.content.starts_with(".:\n"), "{}", listing.content);

    w.call(
        "copy_path",
        json!({"source": "notes/todo.md", "destination": "backup/todo.md"}),
    )
    .await
    .expect("copy");
    w.call(
        "move_path",
        json!({"source": "backup/todo.md", "destination": "backup/old.md"}),
    )
    .await
    .expect("move");
    assert!(w.root.join("backup/old.md").is_file());
    assert!(!w.root.join("backup/todo.md").exists());

    let delete = w
        .prepare("delete_path", json!({"path": "backup/old.md"}))
        .expect("prepare delete");
    assert_eq!(delete.action.risk, RiskLevel::High);
    w.call("delete_path", json!({"path": "backup/old.md"}))
        .await
        .expect("delete");
    assert!(
        matches!(
            w.call("delete_path", json!({"path": "notes"})).await,
            Err(ToolError::Failed(_))
        ),
        "non-empty folders are not deleted"
    );
    assert!(
        matches!(
            w.prepare("delete_path", json!({"path": "."})),
            Err(ToolError::InvalidInput(_))
        ),
        "the workspace itself is not deleted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn file_tools_refuse_escapes_binary_files_and_unknown_fields() {
    let w = workspace();
    assert!(matches!(
        w.prepare("read_file", json!({"path": "../outside.txt"})),
        Err(ToolError::Scope(_))
    ));
    assert!(matches!(
        w.prepare("read_file", json!({"path": "x", "mode": "raw"})),
        Err(ToolError::InvalidInput(_))
    ));
    std::fs::write(w.root.join("image.bin"), [0u8, 159, 146, 150]).unwrap();
    assert!(matches!(
        w.call("read_file", json!({"path": "image.bin"})).await,
        Err(ToolError::Failed(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_run_without_a_shell_with_timeouts_and_cancellation() {
    if resolve_program("git").is_none() {
        eprintln!("git not installed; skipping");
        return;
    }
    let w = workspace();
    let version = w
        .call(
            "run_command",
            json!({"program": "git", "args": ["--version"]}),
        )
        .await
        .expect("git --version");
    assert!(version.content.contains("exit code: 0"));
    assert!(version.content.contains("git version"));

    // Shell syntax is just an argument: `whoami` does not run.
    let chained = w
        .call(
            "run_command",
            json!({"program": "git", "args": ["--version", "&&", "whoami"]}),
        )
        .await
        .expect("git ignores the extra arguments");
    let stdout = chained
        .content
        .split("--- stdout ---")
        .nth(1)
        .and_then(|s| s.split("--- stderr ---").next())
        .unwrap_or_default();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert!(lines[0].starts_with("git version"));

    assert!(matches!(
        w.call(
            "run_command",
            json!({"program": "definitely-not-a-program-xyz"})
        )
        .await,
        Err(ToolError::Failed(_))
    ));

    let (program, args) = sleeper(10);
    let started = std::time::Instant::now();
    let timed_out = w
        .call(
            "run_command",
            json!({"program": program, "args": args, "timeoutSecs": 1}),
        )
        .await;
    assert!(
        matches!(timed_out, Err(ToolError::TimedOut(1))),
        "{timed_out:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(8));

    let cancel = w.ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let cancelled = w
        .call("run_command", json!({"program": program, "args": args}))
        .await;
    assert!(
        matches!(cancelled, Err(ToolError::Cancelled)),
        "{cancelled:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn launched_programs_can_be_stopped_others_cannot() {
    let w = workspace();
    let (program, args) = sleeper(30);
    if resolve_program(program).is_none() {
        return;
    }
    let started = w
        .call("launch_program", json!({"program": program, "args": args}))
        .await
        .expect("launch");
    let pid = w.ctx.processes.list()[0].0;
    assert!(started.content.contains(&pid.to_string()));

    assert!(
        matches!(
            w.prepare("stop_program", json!({"pid": std::process::id()})),
            Err(ToolError::InvalidInput(_))
        ),
        "processes JARVIS did not start are off limits"
    );
    w.call("stop_program", json!({"pid": pid}))
        .await
        .expect("stop");
    assert!(w.ctx.processes.list().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn system_tools_report_the_machine() {
    let w = workspace();
    let info = w.call("system_info", json!({})).await.expect("info");
    assert!(info.content.contains("Memory:"));
    let processes = w
        .call("list_processes", json!({"sortBy": "memory", "limit": 5}))
        .await
        .expect("processes");
    assert!(processes.content.lines().count() >= 2);
    assert!(matches!(
        w.prepare("list_processes", json!({"sortBy": "name"})),
        Err(ToolError::InvalidInput(_))
    ));
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(resolve_program("git").unwrap())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git")
}

#[tokio::test(flavor = "multi_thread")]
async fn git_reads_repositories_without_running_their_config() {
    if resolve_program("git").is_none() {
        return;
    }
    let w = workspace();
    let repo = w.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git(&repo, &["init", "-q"]).status.success());
    std::fs::write(repo.join("readme.md"), "hello\n").unwrap();
    // A hostile repository: fsmonitor runs a command on `git status`.
    let marker = repo.join("pwned.txt");
    assert!(
        git(
            &repo,
            &["config", "core.fsmonitor", "echo pwned > pwned.txt"]
        )
        .status
        .success()
    );
    let _ = git(&repo, &["status"]);
    let config_runs_code = marker.exists();
    let _ = std::fs::remove_file(&marker);

    let status = w
        .call("git", json!({"command": "status", "repo": "repo"}))
        .await
        .expect("status");
    assert!(status.content.contains("readme.md"), "{}", status.content);
    assert!(!marker.exists(), "the tool must not run repository hooks");
    if !config_runs_code {
        eprintln!(
            "this git does not run core.fsmonitor commands; the check above is not conclusive"
        );
    }

    assert!(matches!(
        w.prepare("git", json!({"command": "show", "target": "--output=x"})),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        w.prepare("git", json!({"command": "push"})),
        Err(ToolError::InvalidInput(_))
    ));
}
