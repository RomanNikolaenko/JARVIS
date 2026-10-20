//! System tools: information, processes, starting and stopping programs.
//! Only programs JARVIS started can be stopped.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::command::{base_command, command_line, resolve_program, validate_args};
use crate::util::{parse, to_args, truncate};
use crate::{PreparedCall, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(SystemInfo::new()),
        Arc::new(ListProcesses::new()),
        Arc::new(LaunchProgram::new()),
        Arc::new(StopProgram::new()),
    ]
}

fn spec(
    name: &str,
    title: &str,
    description: &str,
    parameters: serde_json::Value,
    capability: Capability,
    risk: RiskLevel,
    idempotent: bool,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        parameters,
        capabilities: vec![capability],
        risk,
        idempotent,
    }
}

fn action(spec: &ToolSpec, summary: String) -> ActionRequest {
    ActionRequest {
        tool: spec.name.clone(),
        capabilities: spec.capabilities.clone(),
        risk: spec.risk,
        summary,
        paths: Vec::new(),
        program: None,
        url: None,
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// Programs started by `launch_program`, so they can be stopped.
#[derive(Debug, Default)]
pub struct ProcessRegistry {
    running: Mutex<HashMap<u32, Launched>>,
}

#[derive(Debug)]
struct Launched {
    program: String,
    stop: CancellationToken,
}

impl ProcessRegistry {
    fn running(&self) -> MutexGuard<'_, HashMap<u32, Launched>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `(pid, program)` of programs JARVIS started that still run.
    pub fn list(&self) -> Vec<(u32, String)> {
        let mut list: Vec<(u32, String)> = self
            .running()
            .iter()
            .map(|(pid, l)| (*pid, l.program.clone()))
            .collect();
        list.sort();
        list
    }

    fn program(&self, pid: u32) -> Option<String> {
        self.running().get(&pid).map(|l| l.program.clone())
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SystemInfo(ToolSpec);

impl SystemInfo {
    fn new() -> Self {
        Self(spec(
            "system_info",
            "System information",
            "Returns the operating system, CPU, CPU load and memory use.",
            json!({"type": "object", "properties": {}}),
            Capability::SystemRead,
            RiskLevel::Low,
            true,
        ))
    }
}

#[async_trait]
impl Tool for SystemInfo {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let _: NoArgs = parse(input)?;
        Ok(PreparedCall {
            action: action(&self.0, "Read system information".to_owned()),
            args: json!({}),
        })
    }

    async fn run(&self, _call: &PreparedCall, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let content = tokio::task::spawn_blocking(|| {
            let mut system = sysinfo::System::new();
            system.refresh_cpu_usage();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            system.refresh_cpu_usage();
            system.refresh_memory();
            let gib = |bytes: u64| bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            let cpu = system.cpus().first().map(|c| c.brand().trim().to_owned()).unwrap_or_default();
            format!(
                "OS: {} {}\nCPU: {cpu} ({} logical cores), load {:.0}%\nMemory: {:.1} GiB used of {:.1} GiB\nUptime: {} h",
                sysinfo::System::name().unwrap_or_default(),
                sysinfo::System::os_version().unwrap_or_default(),
                system.cpus().len(),
                system.global_cpu_usage(),
                gib(system.used_memory()),
                gib(system.total_memory()),
                sysinfo::System::uptime() / 3600
            )
        })
        .await
        .map_err(|err| ToolError::Failed(err.to_string()))?;
        Ok(ToolOutput {
            summary: "Read system information".to_owned(),
            content,
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProcessArgs {
    #[serde(default)]
    sort_by: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug)]
struct ListProcesses(ToolSpec);

impl ListProcesses {
    fn new() -> Self {
        Self(spec(
            "list_processes",
            "List processes",
            "Lists running processes with CPU and memory use. `sortBy`: \"cpu\" (default) or \"memory\"; `limit` up to 50.",
            json!({"type": "object", "properties": {
                "sortBy": {"type": "string", "enum": ["cpu", "memory"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            }}),
            Capability::SystemRead,
            RiskLevel::Low,
            true,
        ))
    }
}

#[async_trait]
impl Tool for ListProcesses {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: ProcessArgs = parse(input)?;
        if let Some(sort) = &args.sort_by
            && sort != "cpu"
            && sort != "memory"
        {
            return Err(ToolError::InvalidInput(
                "sortBy must be \"cpu\" or \"memory\"".to_owned(),
            ));
        }
        Ok(PreparedCall {
            action: action(&self.0, "List running processes".to_owned()),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ProcessArgs = parse(&call.args)?;
        let by_memory = args.sort_by.as_deref() == Some("memory");
        let limit = args.limit.unwrap_or(15).clamp(1, 50) as usize;
        let mut rows = tokio::task::spawn_blocking(move || {
            let mut system = sysinfo::System::new();
            system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            system
                .processes()
                .iter()
                .map(|(pid, p)| {
                    (
                        pid.as_u32(),
                        p.name().to_string_lossy().into_owned(),
                        p.cpu_usage(),
                        p.memory(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|err| ToolError::Failed(err.to_string()))?;
        if by_memory {
            rows.sort_by_key(|row| std::cmp::Reverse(row.3));
        } else {
            rows.sort_by(|a, b| b.2.total_cmp(&a.2));
        }
        let lines: Vec<String> = rows
            .iter()
            .take(limit)
            .map(|(pid, name, cpu, memory)| {
                format!("{pid}\t{name}\tCPU {cpu:.1}%\t{} MiB", memory / 1024 / 1024)
            })
            .collect();
        Ok(ToolOutput {
            summary: format!("Listed {} processes", lines.len()),
            content: truncate(
                &format!("pid\tname\tcpu\tmemory\n{}", lines.join("\n")),
                ctx.limits.max_output_chars,
            ),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug)]
struct LaunchProgram(ToolSpec);

impl LaunchProgram {
    fn new() -> Self {
        Self(spec(
            "launch_program",
            "Start a program",
            "Starts a program from the allowed list in the background (it keeps running) and returns its process id. Always asks the user first.",
            json!({"type": "object", "required": ["program"], "properties": {
                "program": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "cwd": {"type": "string"}
            }}),
            Capability::ProcessSpawn,
            RiskLevel::High,
            false,
        ))
    }
}

#[async_trait]
impl Tool for LaunchProgram {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: LaunchArgs = parse(input)?;
        validate_args(&args.args)?;
        let cwd = ctx.scope.resolve(args.cwd.as_deref().unwrap_or("."))?;
        let summary = format!(
            "Start `{}` in {}",
            command_line(&args.program, &args.args),
            cwd.display()
        );
        args.cwd = Some(cwd.display().to_string());
        let mut request = action(&self.0, summary);
        request.paths = vec![cwd];
        request.program = Some(args.program.clone());
        Ok(PreparedCall {
            action: request,
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: LaunchArgs = parse(&call.args)?;
        let cwd = ctx
            .scope
            .ensure_inside(Path::new(args.cwd.as_deref().unwrap_or_default()))?;
        let program = resolve_program(&args.program).ok_or_else(|| {
            ToolError::Failed(format!("\"{}\" was not found in PATH", args.program))
        })?;
        let mut command = tokio::process::Command::from(base_command(&program, &args.args, &cwd));
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|err| ToolError::io("starting the program", err))?;
        let pid = child
            .id()
            .ok_or_else(|| ToolError::Failed("the program exited immediately".to_owned()))?;
        let stop = CancellationToken::new();
        ctx.processes.running().insert(
            pid,
            Launched {
                program: args.program.clone(),
                stop: stop.clone(),
            },
        );
        let registry = Arc::clone(&ctx.processes);
        tokio::spawn(async move {
            tokio::select! {
                _ = child.wait() => {}
                () = stop.cancelled() => {
                    let _ = child.kill().await;
                }
            }
            registry.running().remove(&pid);
        });
        Ok(ToolOutput {
            summary: format!("Started {} (pid {pid})", args.program),
            content: format!("Started {} with process id {pid}", args.program),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopArgs {
    pid: u32,
}

#[derive(Debug)]
struct StopProgram(ToolSpec);

impl StopProgram {
    fn new() -> Self {
        Self(spec(
            "stop_program",
            "Stop a program",
            "Stops a program that `launch_program` started earlier, by process id. Other processes cannot be stopped.",
            json!({"type": "object", "required": ["pid"], "properties": {"pid": {"type": "integer"}}}),
            Capability::ProcessControl,
            RiskLevel::High,
            false,
        ))
    }
}

#[async_trait]
impl Tool for StopProgram {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: StopArgs = parse(input)?;
        let program = ctx.processes.program(args.pid).ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "process {} was not started by JARVIS (or has ended)",
                args.pid
            ))
        })?;
        Ok(PreparedCall {
            action: action(&self.0, format!("Stop {program} (pid {})", args.pid)),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: StopArgs = parse(&call.args)?;
        let stop = ctx
            .processes
            .running()
            .get(&args.pid)
            .map(|l| l.stop.clone())
            .ok_or_else(|| ToolError::Failed(format!("process {} has already ended", args.pid)))?;
        stop.cancel();
        for _ in 0..50 {
            if ctx.processes.program(args.pid).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(ToolOutput {
            summary: format!("Stopped process {}", args.pid),
            content: format!("Process {} was stopped", args.pid),
        })
    }
}
