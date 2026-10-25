//! Connects JARVIS's MCP client to a server and shows what it offers: the
//! negotiated protocol, the tools, and optionally the result of one call.
//! For plugin authors and interoperability checks.
//!
//! ```text
//! cargo run -p jarvis-plugins --example mcp-probe -- [--call <tool> <json>] -- <program> [args...]
//! cargo run -p jarvis-plugins --example mcp-probe -- --call word_count '{"text":"a b"}' -- node examples/plugins/text-tools/server.mjs
//! ```
//!
//! The server gets the same minimal environment as a plugin.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use jarvis_plugins::ClientInfo;
use jarvis_plugins::mcp::Era;
use jarvis_plugins::process::{LaunchSpec, LogTail, ServerProcess, inherited_environment};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(split) = args.iter().position(|a| a == "--") else {
        eprintln!("usage: mcp-probe [--call <tool> <json>] -- <program> [args...]");
        return ExitCode::from(2);
    };
    let (options, command) = (&args[..split], &args[split + 1..]);
    let call = match options {
        [] => None,
        [flag, tool, input] if flag == "--call" => match serde_json::from_str(input) {
            Ok(value) => Some((tool.clone(), value)),
            Err(err) => {
                eprintln!("the tool input is not JSON: {err}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("usage: mcp-probe [--call <tool> <json>] -- <program> [args...]");
            return ExitCode::from(2);
        }
    };
    let Some((program, program_args)) = command.split_first() else {
        eprintln!("no server program given");
        return ExitCode::from(2);
    };
    let program = if PathBuf::from(program).is_absolute() {
        PathBuf::from(program)
    } else if let Some(found) = jarvis_tools::resolve_program(program) {
        found
    } else {
        eprintln!("{program} was not found in PATH");
        return ExitCode::from(2);
    };

    let spec = LaunchSpec {
        program,
        args: program_args.to_vec(),
        cwd: std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()),
        env: inherited_environment(),
    };
    let logs = LogTail::default();
    let adapter = jarvis_platform::current_adapter();
    let info = ClientInfo {
        name: "jarvis-mcp-probe".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let process = match ServerProcess::start(&spec, adapter.as_ref(), info, TIMEOUT, &logs).await {
        Ok(process) => process,
        Err(err) => {
            eprintln!("the server did not start: {err}");
            for line in logs.last(20) {
                eprintln!("  stderr: {line}");
            }
            return ExitCode::FAILURE;
        }
    };
    let server = process.description();
    println!(
        "server: {} {}{} — MCP {} ({})",
        server.name,
        server.version,
        server
            .title
            .as_ref()
            .map(|t| format!(" \"{t}\""))
            .unwrap_or_default(),
        server.protocol_version,
        match server.era {
            Era::Modern => "server/discover",
            Era::Legacy => "initialize handshake",
        }
    );

    let mut status = ExitCode::SUCCESS;
    if server.tools_list_changed {
        match process
            .client()
            .subscribe_tool_changes(Duration::from_secs(5))
            .await
        {
            Ok(true) => println!("tool list changes: subscribed"),
            Ok(false) => println!("tool list changes: announced without a subscription"),
            Err(err) => println!("tool list changes: subscription failed: {err}"),
        }
    }
    let mut changes = process.client().tools_changed();
    match process.client().list_tools(TIMEOUT).await {
        Ok((tools, problems)) => {
            println!("tools: {}", tools.len());
            for tool in &tools {
                let mut hints = Vec::new();
                if tool.hints.read_only == Some(true) {
                    hints.push("read-only");
                }
                if tool.hints.destructive == Some(true) {
                    hints.push("destructive");
                }
                let description: String = tool.description.chars().take(90).collect();
                println!("  {} {:?} — {description}", tool.name, hints);
            }
            for problem in problems {
                println!("  skipped: {problem}");
            }
        }
        Err(err) => {
            println!("listing tools failed: {err}");
            status = ExitCode::FAILURE;
        }
    }

    if let Some((tool, input)) = call {
        let never = CancellationToken::new();
        match process
            .client()
            .call_tool(&tool, input, TIMEOUT, &never)
            .await
        {
            Ok(result) => {
                println!(
                    "call {tool}: {}\n{}",
                    if result.is_error { "tool error" } else { "ok" },
                    result.to_text()
                );
                if result.is_error {
                    status = ExitCode::FAILURE;
                }
            }
            Err(err) => {
                println!("call {tool} failed: {err}");
                status = ExitCode::FAILURE;
            }
        }
    }
    // A change the call caused (e.g. `trigger-list-changed`).
    if tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .is_ok_and(|changed| changed.is_ok())
    {
        println!("the server announced a tool list change");
    }
    process.shutdown().await;
    status
}
