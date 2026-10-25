//! MCP client and plugin manager tests with real processes.
//!
//! The test binary doubles as a fake MCP server: started with
//! `JARVIS_FAKE_MCP=<mode>` it serves stdio instead of running the tests.
//! libtest would write to stdout, so this file has its own `main`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::type_complexity
)]

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jarvis_events::EventBus;
use jarvis_plugins::mcp::Era;
use jarvis_plugins::process::{LaunchSpec, LogTail, ServerProcess, inherited_environment};
use jarvis_plugins::{
    ClientInfo, EnvVar, McpError, McpServerRequest, PluginInfo, PluginManager,
    PluginManagerOptions, PluginStatus, PluginToolUpdate,
};
use jarvis_security::{
    Capability, FileScope, InMemorySecretStore, PolicyConfig, PolicyDecision, RiskLevel,
    SecretStore, SecurityPolicy,
};
use jarvis_tools::{ProcessRegistry, ToolContext, ToolError, ToolLimits, ToolRegistry};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const FAKE_ENV: &str = "JARVIS_FAKE_MCP";
/// File where the sleeper writes its process id.
const SLEEPER_PID_ENV: &str = "JARVIS_SLEEPER_PID_FILE";
const TIMEOUT: Duration = Duration::from_secs(15);

fn main() {
    if let Ok(mode) = std::env::var(FAKE_ENV) {
        fake_server(&mode);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let tests: &[(
        &str,
        fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    )] = &[
        ("modern_servers_list_and_call_tools", || {
            Box::pin(modern_servers_list_and_call_tools())
        }),
        ("calls_time_out_and_cancel", || {
            Box::pin(calls_time_out_and_cancel())
        }),
        ("legacy_servers_use_the_handshake", || {
            Box::pin(legacy_servers_use_the_handshake())
        }),
        ("silent_probes_fall_back_to_initialize", || {
            Box::pin(silent_probes_fall_back_to_initialize())
        }),
        ("unsupported_versions_lead_to_a_known_one", || {
            Box::pin(unsupported_versions_lead_to_a_known_one())
        }),
        ("oversized_messages_end_the_connection", || {
            Box::pin(oversized_messages_end_the_connection())
        }),
        ("servers_that_exit_report_their_output", || {
            Box::pin(servers_that_exit_report_their_output())
        }),
        ("plugins_are_approved_started_and_used", || {
            Box::pin(plugins_are_approved_started_and_used())
        }),
        ("lowered_risk_resets_when_a_tool_changes", || {
            Box::pin(lowered_risk_resets_when_a_tool_changes())
        }),
        ("crashed_servers_restart_a_bounded_number_of_times", || {
            Box::pin(crashed_servers_restart_a_bounded_number_of_times())
        }),
        ("changed_plugins_need_approval_again", || {
            Box::pin(changed_plugins_need_approval_again())
        }),
        ("secrets_come_from_the_keychain", || {
            Box::pin(secrets_come_from_the_keychain())
        }),
        ("programs_started_by_a_server_are_stopped_with_it", || {
            Box::pin(programs_started_by_a_server_are_stopped_with_it())
        }),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut ran = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let started = Instant::now();
        runtime.block_on(test());
        println!("test {name} ... ok ({} ms)", started.elapsed().as_millis());
        ran += 1;
    }
    println!("\ntest result: ok. {ran} passed");
}

// ---------------------------------------------------------------- fake server

fn fake_server(mode: &str) -> ! {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let send = {
        let out = Arc::clone(&out);
        move |message: Value| {
            let mut out = out.lock().unwrap();
            writeln!(out, "{message}").unwrap();
            out.flush().unwrap();
        }
    };
    if mode == "sleeper" {
        // A program the server started: records its id and waits.
        if let Ok(file) = std::env::var(SLEEPER_PID_ENV) {
            std::fs::write(file, std::process::id().to_string()).unwrap();
        }
        std::thread::sleep(Duration::from_secs(120));
        std::process::exit(0);
    }
    eprintln!("fake server ({mode}) started");
    if mode == "parent" || mode == "hung-parent" {
        // Left running on purpose: the test checks that JARVIS stops it.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new(std::env::current_exe().unwrap())
            .env(FAKE_ENV, "sleeper")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
    }
    // The parent modes speak the modern protocol.
    let hung = mode == "hung-parent";
    let mode = if mode == "parent" || hung {
        "modern"
    } else {
        mode
    };
    match mode {
        "exit" => {
            eprintln!("fatal: the configuration is missing");
            std::process::exit(3);
        }
        "huge" => {
            let mut out = out.lock().unwrap();
            let line = "a".repeat(9 * 1024 * 1024);
            writeln!(out, "{line}").unwrap();
            out.flush().unwrap();
        }
        _ => send(json!("not a message: a banner some servers print")),
    }
    let modern = mode == "modern";
    let cancelled: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut initialized = false;
    let mut echo_description = "Echoes text back.".to_owned();
    // The `subscriptions/listen` id of a modern client that asked for tool
    // list changes.
    let mut subscription: Option<Value> = None;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = message.get("params").cloned().unwrap_or(json!({}));
        let reply = |result: Value| send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let error = |code: i64, text: &str, data: Value| {
            send(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text, "data": data}}),
            );
        };
        let version = params
            .pointer("/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response of the client.
            match (id.as_str(), message.get("error")) {
                (Some("ping-1"), None) => eprintln!("pong received"),
                (Some("sampling-1"), Some(err)) => eprintln!("sampling refused {}", err["code"]),
                _ => {}
            }
            continue;
        };
        match method {
            "notifications/cancelled" => {
                let request = params["requestId"].to_string();
                eprintln!("cancelled {request}");
                cancelled.lock().unwrap().insert(request);
                continue;
            }
            "notifications/initialized" => {
                initialized = true;
                send(json!({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"}));
                send(
                    json!({"jsonrpc": "2.0", "id": "sampling-1", "method": "sampling/createMessage", "params": {}}),
                );
                continue;
            }
            "server/discover" => match mode {
                "modern" if version.as_deref() == Some("2026-07-28") => reply(json!({
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {"listChanged": true}},
                    "instructions": "Use echo.",
                    "ttlMs": 1000,
                    "cacheScope": "public",
                    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "fake-mcp", "version": "1.0.0"}},
                })),
                "modern" => error(
                    -32022,
                    "Unsupported protocol version",
                    json!({"supported": ["2026-07-28"], "requested": version}),
                ),
                "old-version" => error(
                    -32022,
                    "Unsupported protocol version",
                    json!({"supported": ["2025-11-25"], "requested": version}),
                ),
                "silent" => {}
                _ => error(-32601, "Method not found", Value::Null),
            },
            "initialize" if modern => error(-32600, "use server/discover", Value::Null),
            "subscriptions/listen" if modern => {
                if params["notifications"]["toolsListChanged"] == json!(true) {
                    subscription = Some(id.clone());
                }
                send(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/subscriptions/acknowledged",
                    "params": {
                        "_meta": {"io.modelcontextprotocol/subscriptionId": id},
                        "notifications": {"toolsListChanged": subscription.is_some()},
                    },
                }));
            }
            "initialize" => reply(json!({
                "protocolVersion": if mode == "old-version" { params["protocolVersion"].clone() } else { json!("2025-06-18") },
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "fake-legacy", "version": "0.9"},
            })),
            "tools/list" => {
                if modern && version.is_none() {
                    error(-32602, "missing _meta", Value::Null);
                    continue;
                }
                if !modern && !initialized {
                    error(-32002, "not initialized", Value::Null);
                    continue;
                }
                let schema = json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]});
                let empty = json!({"type": "object", "additionalProperties": false});
                if params.get("cursor").is_none() {
                    reply(json!({
                        "resultType": "complete",
                        "tools": [
                            {"name": "echo", "description": echo_description, "inputSchema": schema},
                            {"name": "fail", "inputSchema": empty},
                            {"name": "bad name", "inputSchema": empty},
                        ],
                        "nextCursor": "page-2",
                        "ttlMs": 1000,
                        "cacheScope": "public",
                    }));
                } else {
                    reply(json!({
                        "resultType": "complete",
                        "tools": [
                            {"name": "slow", "inputSchema": empty, "annotations": {"readOnlyHint": true}},
                            {"name": "retry", "inputSchema": empty},
                            {"name": "ask", "inputSchema": empty},
                            {"name": "crash", "inputSchema": empty},
                            {"name": "mutate", "inputSchema": empty},
                            {"name": "env", "title": "Environment", "inputSchema": empty},
                        ],
                        "ttlMs": 1000,
                        "cacheScope": "public",
                    }));
                }
            }
            "tools/call" => {
                let text = |text: &str| json!({"content": [{"type": "text", "text": text}]});
                match params["name"].as_str().unwrap_or_default() {
                    "echo" => reply(text(
                        params["arguments"]["text"].as_str().unwrap_or_default(),
                    )),
                    "fail" => reply(
                        json!({"content": [{"type": "text", "text": "it broke"}], "isError": true}),
                    ),
                    "retry" if modern && params.get("requestState").is_none() => {
                        reply(json!({"resultType": "input_required", "requestState": "state-1"}));
                    }
                    "retry" => reply(text("retried")),
                    "ask" => reply(json!({
                        "resultType": "input_required",
                        "inputRequests": {"q": {"method": "elicitation/create", "params": {}}},
                    })),
                    "slow" => {
                        let send = send.clone();
                        let cancelled = Arc::clone(&cancelled);
                        let key = id.to_string();
                        let id = id.clone();
                        std::thread::spawn(move || {
                            for _ in 0..600 {
                                std::thread::sleep(Duration::from_millis(50));
                                if cancelled.lock().unwrap().contains(&key) {
                                    eprintln!("slow call stopped");
                                    return;
                                }
                            }
                            send(
                                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "slow done"}]}}),
                            );
                        });
                    }
                    "crash" => {
                        eprintln!("crashing on purpose");
                        std::process::exit(7);
                    }
                    "mutate" => {
                        echo_description = "Echoes text back. Changed.".to_owned();
                        reply(text("changed"));
                        if !modern {
                            send(
                                json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                            );
                        } else if let Some(subscription) = &subscription {
                            send(json!({
                                "jsonrpc": "2.0",
                                "method": "notifications/tools/list_changed",
                                "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": subscription}},
                            }));
                        }
                    }
                    "env" => {
                        let var =
                            |name: &str| std::env::var(name).unwrap_or_else(|_| "-".to_owned());
                        let other = params["arguments"]["name"].as_str().unwrap_or("NONE");
                        reply(text(&format!(
                            "API_KEY={} {other}={}",
                            var("API_KEY"),
                            var(other)
                        )));
                    }
                    _ => error(-32602, "Unknown tool", Value::Null),
                }
            }
            _ => error(-32601, "Method not found", Value::Null),
        }
    }
    if hung {
        // Ignores the end of its input, like a stuck server.
        std::thread::sleep(Duration::from_secs(120));
    }
    std::process::exit(0);
}

// ---------------------------------------------------------------- helpers

fn info() -> ClientInfo {
    ClientInfo {
        name: "jarvis-test".into(),
        version: "0.0.0".into(),
    }
}

fn spec(mode: &str) -> LaunchSpec {
    let mut env = inherited_environment();
    env.push((FAKE_ENV.to_owned(), mode.to_owned()));
    LaunchSpec {
        program: std::env::current_exe().unwrap(),
        args: Vec::new(),
        cwd: std::env::temp_dir(),
        env,
    }
}

async fn start(mode: &str, timeout: Duration) -> (ServerProcess, LogTail) {
    let logs = LogTail::default();
    let adapter = jarvis_platform::current_adapter();
    let process = ServerProcess::start(&spec(mode), adapter.as_ref(), info(), timeout, &logs)
        .await
        .unwrap_or_else(|err| panic!("{mode} server did not start: {err}"));
    (process, logs)
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A variable of this process that servers must not inherit.
fn private_variable() -> Option<String> {
    let inherited: Vec<String> = inherited_environment()
        .into_iter()
        .map(|(name, _)| name.to_uppercase())
        .collect();
    std::env::vars().map(|(name, _)| name).find(|name| {
        !inherited.contains(&name.to_uppercase())
            && name != FAKE_ENV
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

async fn env_of(registry: &ToolRegistry, tool: &str) -> (String, Option<String>) {
    let private = private_variable();
    let tool = registry.get(tool).unwrap();
    let ctx = context();
    let name = private.clone().unwrap_or_else(|| "NONE".to_owned());
    let prepared = tool.prepare(&json!({"name": name}), &ctx).unwrap();
    (tool.run(&prepared, &ctx).await.unwrap().content, private)
}

fn names(tools: &[jarvis_plugins::mcp::McpTool]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

// ---------------------------------------------------------------- client tests

async fn modern_servers_list_and_call_tools() {
    let (process, logs) = start("modern", TIMEOUT).await;
    let server = process.description();
    assert_eq!(server.era, Era::Modern);
    assert_eq!(server.protocol_version, "2026-07-28");
    assert_eq!(server.name, "fake-mcp");
    assert_eq!(server.instructions.as_deref(), Some("Use echo."));

    let client = process.client();
    let (tools, problems) = client.list_tools(TIMEOUT).await.unwrap();
    assert_eq!(
        names(&tools),
        [
            "echo", "fail", "slow", "retry", "ask", "crash", "mutate", "env"
        ]
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(tools[2].hints.read_only, Some(true));
    assert_eq!(tools[7].title.as_deref(), Some("Environment"));

    let never = CancellationToken::new();
    let echo = client
        .call_tool("echo", json!({"text": "hello\nworld"}), TIMEOUT, &never)
        .await
        .unwrap();
    assert_eq!(echo.to_text(), "hello\nworld");
    let fail = client
        .call_tool("fail", json!({}), TIMEOUT, &never)
        .await
        .unwrap();
    assert!(fail.is_error);
    let retried = client
        .call_tool("retry", json!({}), TIMEOUT, &never)
        .await
        .unwrap();
    assert_eq!(retried.to_text(), "retried");
    let asked = client.call_tool("ask", json!({}), TIMEOUT, &never).await;
    assert!(matches!(asked, Err(McpError::Unsupported(_))), "{asked:?}");
    let unknown = client.call_tool("nope", json!({}), TIMEOUT, &never).await;
    assert!(
        matches!(unknown, Err(McpError::Rpc(ref e)) if e.code == -32602),
        "{unknown:?}"
    );

    assert!(
        logs.last(50)
            .iter()
            .any(|l| l.contains("fake server (modern) started"))
    );
    process.shutdown().await;
    assert!(client.closed().is_cancelled());
}

async fn calls_time_out_and_cancel() {
    let (process, logs) = start("modern", TIMEOUT).await;
    let client = process.client();

    let started = Instant::now();
    let never = CancellationToken::new();
    let timed_out = client
        .call_tool("slow", json!({}), Duration::from_millis(300), &never)
        .await;
    assert!(
        matches!(timed_out, Err(McpError::TimedOut(_))),
        "{timed_out:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let cancelled = client.call_tool("slow", json!({}), TIMEOUT, &cancel).await;
    assert!(
        matches!(cancelled, Err(McpError::Cancelled)),
        "{cancelled:?}"
    );
    // The server was told both times and stopped working.
    eventually("the server to stop both calls", || {
        logs.last(50)
            .iter()
            .filter(|l| l.contains("slow call stopped"))
            .count()
            == 2
    })
    .await;
    // The connection still works.
    let echo = client
        .call_tool("echo", json!({"text": "still here"}), TIMEOUT, &never)
        .await
        .unwrap();
    assert_eq!(echo.to_text(), "still here");
    process.shutdown().await;
}

async fn legacy_servers_use_the_handshake() {
    let (process, logs) = start("legacy", TIMEOUT).await;
    let server = process.description().clone();
    assert_eq!(server.era, Era::Legacy);
    assert_eq!(server.protocol_version, "2025-06-18");
    assert_eq!(server.name, "fake-legacy");
    let client = process.client();
    let (tools, _) = client.list_tools(TIMEOUT).await.unwrap();
    assert_eq!(tools.len(), 8);
    // Pings are answered, other server requests refused.
    eventually("the ping answer", || {
        logs.last(50).iter().any(|l| l == "pong received")
    })
    .await;
    eventually("the refused sampling request", || {
        logs.last(50).iter().any(|l| l == "sampling refused -32601")
    })
    .await;

    let mut changes = client.tools_changed();
    let never = CancellationToken::new();
    client
        .call_tool("mutate", json!({}), TIMEOUT, &never)
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, changes.changed())
        .await
        .expect("a tool list change")
        .unwrap();
    process.shutdown().await;
}

async fn silent_probes_fall_back_to_initialize() {
    // The server ignores server/discover; after a third of the limit the
    // client also sends initialize.
    let started = Instant::now();
    let (process, _) = start("silent", Duration::from_secs(3)).await;
    assert_eq!(process.description().era, Era::Legacy);
    assert!(started.elapsed() >= Duration::from_millis(900));
    process.shutdown().await;
}

async fn unsupported_versions_lead_to_a_known_one() {
    let (process, _) = start("old-version", TIMEOUT).await;
    assert_eq!(process.description().era, Era::Legacy);
    assert_eq!(process.description().protocol_version, "2025-11-25");
    process.shutdown().await;
}

async fn oversized_messages_end_the_connection() {
    let logs = LogTail::default();
    let adapter = jarvis_platform::current_adapter();
    let result =
        ServerProcess::start(&spec("huge"), adapter.as_ref(), info(), TIMEOUT, &logs).await;
    let error = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(error.contains("larger than 8 MB"), "{error}");
}

async fn servers_that_exit_report_their_output() {
    let logs = LogTail::default();
    let adapter = jarvis_platform::current_adapter();
    let result =
        ServerProcess::start(&spec("exit"), adapter.as_ref(), info(), TIMEOUT, &logs).await;
    assert!(result.is_err());
    eventually("the server's last words", || {
        logs.last(10)
            .iter()
            .any(|l| l.contains("the configuration is missing"))
    })
    .await;
}

// ---------------------------------------------------------------- manager tests

struct Harness {
    manager: PluginManager,
    registry: Arc<ToolRegistry>,
    secrets: Arc<InMemorySecretStore>,
    store: PathBuf,
    dir: tempfile::TempDir,
}

async fn harness(restart_delays: Vec<Duration>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(ToolRegistry::builtin());
    let secrets = Arc::new(InMemorySecretStore::new());
    let store = dir.path().join("config").join("plugins.json");
    let mut options = PluginManagerOptions::new(
        store.clone(),
        dir.path().join("data").join("plugins"),
        Arc::clone(&registry),
        Arc::clone(&secrets) as Arc<dyn SecretStore>,
        EventBus::default(),
        Arc::from(jarvis_platform::current_adapter()),
        info(),
    );
    options.connect_timeout = TIMEOUT;
    options.restart_delays = restart_delays;
    Harness {
        manager: PluginManager::open(options).await.unwrap(),
        registry,
        secrets,
        store,
        dir,
    }
}

fn command_request(name: &str, mode: &str, secrets: &[&str]) -> McpServerRequest {
    McpServerRequest {
        name: name.to_owned(),
        program: std::env::current_exe().unwrap().display().to_string(),
        args: Vec::new(),
        cwd: None,
        env: vec![EnvVar {
            name: FAKE_ENV.to_owned(),
            value: mode.to_owned(),
        }],
        secret_names: secrets.iter().map(|s| (*s).to_owned()).collect(),
    }
}

async fn wait_for(
    manager: &PluginManager,
    id: &str,
    what: &str,
    check: impl Fn(&PluginInfo) -> bool,
) -> PluginInfo {
    let mut changes = manager.subscribe();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let info = manager.get(id).unwrap();
        if check(&info) {
            return info;
        }
        let waited = tokio::time::timeout_at(deadline.into(), changes.recv()).await;
        assert!(waited.is_ok(), "timed out waiting for {what}: {info:#?}");
    }
}

fn running(info: &PluginInfo) -> bool {
    info.status == PluginStatus::Running
}

fn context() -> ToolContext {
    ToolContext::new(
        FileScope::new(Vec::<PathBuf>::new(), Vec::<PathBuf>::new()),
        ToolLimits {
            command_timeout: TIMEOUT,
            ..ToolLimits::default()
        },
        Arc::new(ProcessRegistry::default()),
    )
}

fn policy(registry: &ToolRegistry) -> SecurityPolicy {
    SecurityPolicy::new(PolicyConfig {
        enabled_tools: registry.group_tool_names(),
        scope: FileScope::new(Vec::<PathBuf>::new(), Vec::<PathBuf>::new()),
        allowed_programs: BTreeSet::new(),
        allow_network: false,
        auto_approve_low: true,
        auto_approve_medium: true,
    })
}

async fn plugins_are_approved_started_and_used() {
    let h = harness(Vec::new()).await;
    // A review saves and starts nothing.
    let review = h
        .manager
        .review_command(command_request("Fake Server", "modern", &[]))
        .await
        .unwrap();
    assert_eq!(review.id, "fake-server");
    assert_eq!(review.fingerprint.len(), 64);
    assert!(!review.replaces);
    assert!(h.manager.list().is_empty());
    assert!(!h.store.exists());

    h.manager.install(&review.review_id).await.unwrap();
    // A review is used once.
    assert!(h.manager.install(&review.review_id).await.is_err());
    let info = wait_for(&h.manager, "fake-server", "the server to run", running).await;
    assert_eq!(
        info.server.as_ref().map(|s| s.name.as_str()),
        Some("fake-mcp")
    );
    assert_eq!(info.tools.len(), 8);
    assert_eq!(info.tool_problems.len(), 1);
    assert!(
        info.tools
            .iter()
            .all(|t| t.risk == RiskLevel::High && t.enabled)
    );
    assert!(h.store.exists());

    // Tools are registered under the plugin's prefix and need approval.
    let names = h.registry.group_tool_names();
    assert!(names.contains("fake-server__echo"), "{names:?}");
    let echo = h.registry.get("fake-server__echo").unwrap();
    assert_eq!(echo.spec().plugin.as_deref(), Some("Fake Server"));
    assert_eq!(echo.spec().capabilities, vec![Capability::PluginTool]);
    assert!(!echo.spec().idempotent);
    let ctx = context();
    let prepared = echo.prepare(&json!({"text": "hi"}), &ctx).unwrap();
    assert!(prepared.action.summary.contains("Fake Server"));
    assert!(matches!(
        policy(&h.registry).evaluate(&prepared.action),
        PolicyDecision::RequireApproval { .. }
    ));
    assert!(
        echo.prepare(&json!({}), &ctx).is_err(),
        "required fields are checked"
    );
    assert!(echo.prepare(&json!("text"), &ctx).is_err());
    let output = echo.run(&prepared, &ctx).await.unwrap();
    assert_eq!(output.content, "hi");
    let fail = h.registry.get("fake-server__fail").unwrap();
    let prepared = fail.prepare(&json!({}), &ctx).unwrap();
    let failed = fail.run(&prepared, &ctx).await;
    assert!(matches!(failed, Err(ToolError::Failed(ref text)) if text.contains("it broke")));

    // The server sees only the inherited environment and its own variables.
    let (output, private) = env_of(&h.registry, "fake-server__env").await;
    let private = private.unwrap_or_else(|| "NONE".to_owned());
    assert_eq!(output, format!("API_KEY=- {private}=-"));

    // Turning a tool off removes it; disabling the plugin removes all.
    h.manager
        .set_tool(PluginToolUpdate {
            plugin_id: "fake-server".into(),
            tool: "crash".into(),
            enabled: false,
            risk: RiskLevel::High,
        })
        .await
        .unwrap();
    assert!(h.registry.get("fake-server__crash").is_none());
    assert!(h.registry.get("fake-server__echo").is_some());
    let disabled = h.manager.set_enabled("fake-server", false).await.unwrap();
    assert_eq!(disabled.status, PluginStatus::Disabled);
    assert!(h.registry.group_tool_names().is_empty());

    // Settings survive a restart of JARVIS; the tool stays off.
    h.manager.set_enabled("fake-server", true).await.unwrap();
    wait_for(
        &h.manager,
        "fake-server",
        "the server to run again",
        running,
    )
    .await;
    h.manager.shutdown().await;
    let again = harness_at(&h, Vec::new()).await;
    again.start_enabled();
    let info = wait_for(&again, "fake-server", "the reopened plugin", running).await;
    assert!(
        !info
            .tools
            .iter()
            .find(|t| t.name == "crash")
            .unwrap()
            .enabled
    );

    again.remove("fake-server").await.unwrap();
    assert!(again.list().is_empty());
    assert!(h.registry.group_tool_names().is_empty());
    again.shutdown().await;
}

/// A second manager on the same files (as after a restart of JARVIS).
async fn harness_at(h: &Harness, restart_delays: Vec<Duration>) -> PluginManager {
    let mut options = PluginManagerOptions::new(
        h.store.clone(),
        h.dir.path().join("data").join("plugins"),
        Arc::clone(&h.registry),
        Arc::clone(&h.secrets) as Arc<dyn SecretStore>,
        EventBus::default(),
        Arc::from(jarvis_platform::current_adapter()),
        info(),
    );
    options.connect_timeout = TIMEOUT;
    options.restart_delays = restart_delays;
    PluginManager::open(options).await.unwrap()
}

async fn lowered_risk_resets_when_a_tool_changes() {
    // Legacy servers announce changes on their own; modern ones to the
    // subscription JARVIS opens with subscriptions/listen.
    for mode in ["legacy", "modern"] {
        lowered_risk_resets_for(mode).await;
    }
}

async fn lowered_risk_resets_for(mode: &str) {
    let h = harness(Vec::new()).await;
    let review = h
        .manager
        .review_command(command_request(mode, mode, &[]))
        .await
        .unwrap();
    h.manager.install(&review.review_id).await.unwrap();
    wait_for(&h.manager, mode, "the server to run", running).await;
    let info = h
        .manager
        .set_tool(PluginToolUpdate {
            plugin_id: mode.into(),
            tool: "echo".into(),
            enabled: true,
            risk: RiskLevel::Low,
        })
        .await
        .unwrap();
    let echo = info.tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(echo.risk, RiskLevel::Low);
    let tool = h.registry.get(&format!("{mode}__echo")).unwrap();
    let ctx = context();
    let prepared = tool.prepare(&json!({"text": "x"}), &ctx).unwrap();
    assert_eq!(
        policy(&h.registry).evaluate(&prepared.action),
        PolicyDecision::Allow
    );

    // The server changes the tool's description and announces it.
    let mutate = h.registry.get(&format!("{mode}__mutate")).unwrap();
    let prepared = mutate.prepare(&json!({}), &ctx).unwrap();
    mutate.run(&prepared, &ctx).await.unwrap();
    let info = wait_for(&h.manager, mode, "the risk to reset", |info| {
        info.tools
            .iter()
            .any(|t| t.name == "echo" && t.risk_reset && t.risk == RiskLevel::High)
    })
    .await;
    assert!(
        info.tools
            .iter()
            .any(|t| t.description.ends_with("Changed."))
    );
    let tool = h.registry.get(&format!("{mode}__echo")).unwrap();
    assert_eq!(tool.spec().risk, RiskLevel::High);
    h.manager.shutdown().await;
}

async fn crashed_servers_restart_a_bounded_number_of_times() {
    let h = harness(vec![Duration::from_millis(100)]).await;
    let review = h
        .manager
        .review_command(command_request("Crashy", "modern", &[]))
        .await
        .unwrap();
    h.manager.install(&review.review_id).await.unwrap();
    wait_for(&h.manager, "crashy", "the server to run", running).await;
    let ctx = context();
    let crash = |registry: &ToolRegistry| registry.get("crashy__crash").unwrap();

    let tool = crash(&h.registry);
    let prepared = tool.prepare(&json!({}), &ctx).unwrap();
    let result = tool.run(&prepared, &ctx).await;
    assert!(matches!(result, Err(ToolError::Failed(_))), "{result:?}");
    // One automatic restart is allowed…
    let starts = || {
        h.manager
            .logs("crashy")
            .unwrap()
            .iter()
            .filter(|l| l.contains("fake server (modern) started"))
            .count()
    };
    wait_for(&h.manager, "crashy", "the automatic restart", |info| {
        running(info) && starts() == 2
    })
    .await;
    let tool = crash(&h.registry);
    let prepared = tool.prepare(&json!({}), &ctx).unwrap();
    let _ = tool.run(&prepared, &ctx).await;
    // …and then it stays stopped.
    let info = wait_for(&h.manager, "crashy", "the final failure", |info| {
        matches!(
            &info.status,
            PluginStatus::Failed {
                retry_in_secs: None,
                ..
            }
        )
    })
    .await;
    let PluginStatus::Failed { error, .. } = &info.status else {
        panic!("not failed: {info:#?}")
    };
    assert!(error.contains("crashing on purpose"), "{error}");
    assert!(h.registry.group_tool_names().is_empty());
    // The user can start it again.
    h.manager.restart("crashy").await.unwrap();
    wait_for(&h.manager, "crashy", "the manual restart", running).await;
    h.manager.shutdown().await;
}

fn write_folder_plugin(folder: &Path, extra_env: &str) {
    std::fs::create_dir_all(folder).unwrap();
    let program = if cfg!(windows) {
        "fake-server.exe"
    } else {
        "fake-server"
    };
    let target = folder.join(program);
    if !target.exists() {
        std::fs::copy(std::env::current_exe().unwrap(), &target).unwrap();
    }
    let manifest = json!({
        "manifestVersion": 1,
        "id": "folder-demo",
        "name": "Folder demo",
        "version": "1.0.0",
        "description": "A plugin from a folder.",
        "server": {
            "command": program,
            "env": {FAKE_ENV: "modern", "EXTRA": extra_env},
        },
    });
    std::fs::write(
        folder.join("jarvis-plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

async fn changed_plugins_need_approval_again() {
    let h = harness(Vec::new()).await;
    let folder = h.dir.path().join("plugins").join("folder-demo");
    write_folder_plugin(&folder, "one");
    let review = h.manager.review_folder(folder.clone()).await.unwrap();
    assert_eq!(review.name, "Folder demo");
    assert_eq!(review.launch.env_names, ["EXTRA", FAKE_ENV]);
    h.manager.install(&review.review_id).await.unwrap();
    wait_for(&h.manager, "folder-demo", "the server to run", running).await;

    // A change of the manifest is found at the next start.
    write_folder_plugin(&folder, "two");
    h.manager.restart("folder-demo").await.unwrap();
    let info = wait_for(&h.manager, "folder-demo", "approval to be needed", |info| {
        matches!(info.status, PluginStatus::NeedsApproval { .. })
    })
    .await;
    assert!(h.registry.group_tool_names().is_empty(), "{info:#?}");

    // A review made before another change cannot be installed.
    let stale = h.manager.review_existing("folder-demo").await.unwrap();
    assert!(stale.replaces);
    write_folder_plugin(&folder, "three");
    assert!(h.manager.install(&stale.review_id).await.is_err());

    let review = h.manager.review_existing("folder-demo").await.unwrap();
    h.manager.install(&review.review_id).await.unwrap();
    wait_for(&h.manager, "folder-demo", "the approved plugin", running).await;
    h.manager.shutdown().await;
}

async fn secrets_come_from_the_keychain() {
    let h = harness(Vec::new()).await;
    let review = h
        .manager
        .review_command(command_request("Keyed", "modern", &["API_KEY"]))
        .await
        .unwrap();
    assert_eq!(review.secrets.len(), 1);
    h.manager.install(&review.review_id).await.unwrap();
    let info = wait_for(&h.manager, "keyed", "secrets to be asked for", |info| {
        matches!(info.status, PluginStatus::NeedsSecrets { .. })
    })
    .await;
    assert!(!info.secrets[0].set);
    assert!(
        h.manager.set_secret("keyed", "OTHER", "x").await.is_err(),
        "only declared secrets"
    );
    h.manager
        .set_secret("keyed", "API_KEY", "s3cret")
        .await
        .unwrap();
    let info = wait_for(&h.manager, "keyed", "the server to run", running).await;
    assert!(info.secrets[0].set);
    let store_text = std::fs::read_to_string(&h.store).unwrap();
    assert!(
        !store_text.contains("s3cret"),
        "secrets stay out of plugins.json"
    );

    let (output, _) = env_of(&h.registry, "keyed__env").await;
    assert!(output.starts_with("API_KEY=s3cret "), "{output}");

    h.manager.remove("keyed").await.unwrap();
    assert_eq!(
        h.secrets
            .get_secret(&jarvis_plugins::secret_key("keyed", "API_KEY"))
            .unwrap(),
        None
    );
    h.manager.shutdown().await;
}

fn process_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

async fn programs_started_by_a_server_are_stopped_with_it() {
    // Both stubborn and cleanly exiting servers must release descendants.
    let modes = &["hung-parent", "parent"];
    for mode in modes {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("sleeper.pid");
        let mut launch = spec(mode);
        launch
            .env
            .push((SLEEPER_PID_ENV.to_owned(), pid_file.display().to_string()));
        let logs = LogTail::default();
        let adapter = jarvis_platform::current_adapter();
        let process = ServerProcess::start(&launch, adapter.as_ref(), info(), TIMEOUT, &logs)
            .await
            .unwrap_or_else(|err| panic!("{mode} server did not start: {err}"));
        eventually("the sleeper to start", || pid_file.exists()).await;
        let sleeper: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                .unwrap_or_default()
                .trim()
                .parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(process_alive(sleeper), "{mode}: the sleeper runs");
        let server = process.pid().unwrap();

        process.shutdown().await;
        eventually(
            &format!("{mode}: the program the server started to stop"),
            || !process_alive(sleeper),
        )
        .await;
        assert!(!process_alive(server), "{mode}: the server stopped");
    }
}
