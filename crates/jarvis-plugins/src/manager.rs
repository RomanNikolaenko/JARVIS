//! Plugin management: reviews and approvals, the store, starting and
//! stopping servers, and registering their tools.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_events::{EventBus, EventCategory};
use jarvis_platform::PlatformAdapter;
use jarvis_security::{RiskLevel, SecretStore};
use jarvis_tools::{Tool, ToolRegistry};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{PluginError, Result};
use crate::launch::{self, LaunchConfig, ResolvedLaunch};
use crate::manifest::{self, MANIFEST_FILE, Manifest};
use crate::mcp::{ClientInfo, McpTool};
use crate::process::{LaunchSpec, LogTail, ServerProcess, inherited_environment};
use crate::store::{self, PluginRecord, Source, ToolChoice};
use crate::tool::{PluginTool, clean_description, definition_hash, tool_id};
use crate::types::{
    LaunchInfo, McpServerRequest, PluginInfo, PluginReview, PluginSecretInfo, PluginSourceKind,
    PluginStatus, PluginToolInfo, PluginToolUpdate, SecretRequirement, ServerDetails,
};

/// Reviews older than this must be made again.
const REVIEW_LIFETIME: Duration = Duration::from_secs(30 * 60);
const MAX_REVIEWS: usize = 20;
const MAX_PLUGINS: usize = 64;
/// Running this long resets the count of automatic restarts.
const HEALTHY_RUN: Duration = Duration::from_secs(10 * 60);
/// Pause between tool list refreshes asked for by a server.
const REFRESH_PAUSE: Duration = Duration::from_secs(1);
const LOG_TAIL_IN_ERRORS: usize = 6;

pub struct PluginManagerOptions {
    /// `plugins.json`, in the protected settings folder.
    pub store_path: PathBuf,
    /// Parent of the working folders of servers without one.
    pub work_dir: PathBuf,
    pub registry: Arc<ToolRegistry>,
    pub secrets: Arc<dyn SecretStore>,
    pub events: EventBus,
    pub adapter: Arc<dyn PlatformAdapter>,
    pub client: ClientInfo,
    /// Time for a server to start and answer the first request.
    pub connect_timeout: Duration,
    /// Time limit of a tool list request.
    pub request_timeout: Duration,
    /// Pauses before automatic restarts after a crash; their number is the
    /// most restarts in a row.
    pub restart_delays: Vec<Duration>,
}

impl PluginManagerOptions {
    pub fn new(
        store_path: PathBuf,
        work_dir: PathBuf,
        registry: Arc<ToolRegistry>,
        secrets: Arc<dyn SecretStore>,
        events: EventBus,
        adapter: Arc<dyn PlatformAdapter>,
        client: ClientInfo,
    ) -> Self {
        Self {
            store_path,
            work_dir,
            registry,
            secrets,
            events,
            adapter,
            client,
            connect_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(30),
            restart_delays: vec![
                Duration::from_secs(2),
                Duration::from_secs(10),
                Duration::from_secs(30),
            ],
        }
    }
}

/// The key of a plugin secret in the OS keychain.
pub fn secret_key(plugin_id: &str, name: &str) -> String {
    format!("plugin:{plugin_id}:{name}")
}

#[derive(Debug)]
struct Running {
    process: Arc<ServerProcess>,
    started: Instant,
}

#[derive(Debug, Default)]
struct Runtime {
    /// `None` until the plugin is started or stopped once.
    status: Option<PluginStatus>,
    /// Changes whenever the plugin is started or stopped, so stale tasks
    /// leave the state alone.
    generation: u64,
    running: Option<Running>,
    logs: LogTail,
    restarts: usize,
    server: Option<ServerDetails>,
    /// The last tool list (kept while the server is stopped).
    tools: Vec<McpTool>,
    tool_problems: Vec<String>,
    resolved_program: Option<String>,
    warnings: Vec<String>,
    secrets_set: BTreeSet<String>,
}

#[derive(Debug)]
struct PendingReview {
    review: PluginReview,
    record: PluginRecord,
    created: Instant,
}

#[derive(Debug, Default)]
struct State {
    records: Vec<PluginRecord>,
    runtime: HashMap<String, Runtime>,
    reviews: HashMap<String, PendingReview>,
}

impl State {
    fn record(&self, id: &str) -> Option<&PluginRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    fn record_mut(&mut self, id: &str) -> Option<&mut PluginRecord> {
        self.records.iter_mut().find(|r| r.id == id)
    }

    fn runtime(&mut self, id: &str) -> &mut Runtime {
        self.runtime.entry(id.to_owned()).or_default()
    }
}

struct Inner {
    options: PluginManagerOptions,
    state: Mutex<State>,
    changes: broadcast::Sender<()>,
    /// Keeps saves in order.
    save_lock: tokio::sync::Mutex<()>,
    shutdown: CancellationToken,
}

/// Why a start did not happen.
enum StartError {
    NeedsApproval(String),
    NeedsSecrets(Vec<String>),
    Failed(String),
}

/// What a start produced.
struct Started {
    process: Arc<ServerProcess>,
    tools: Vec<McpTool>,
    problems: Vec<String>,
    resolved: ResolvedLaunch,
}

/// Manages plugins. Cheap to clone.
#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugins", &self.inner.state().records.len())
            .finish_non_exhaustive()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn with_logs(error: &str, logs: &LogTail) -> String {
    let tail = logs.last(LOG_TAIL_IN_ERRORS);
    if tail.is_empty() {
        error.to_owned()
    } else {
        format!("{error}\nLast output of the server:\n{}", tail.join("\n"))
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn changed(&self) {
        let _ = self.changes.send(());
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock().await;
        let records = self.state().records.clone();
        store::save(&self.options.store_path, &records)
            .await
            .map_err(|err| PluginError::Store(err.to_string()))
    }

    fn default_cwd(&self, id: &str) -> PathBuf {
        self.options.work_dir.join(id)
    }

    fn secrets_set(&self, record: &PluginRecord) -> BTreeSet<String> {
        record
            .secrets
            .iter()
            .filter(|s| {
                self.options
                    .secrets
                    .has_secret(&secret_key(&record.id, &s.name))
                    .unwrap_or(false)
            })
            .map(|s| s.name.clone())
            .collect()
    }

    /// Registers the enabled tools of a running plugin (or removes them).
    fn register(&self, state: &mut State, id: &str) {
        let Some(record) = state.record(id).cloned() else {
            self.options.registry.remove_group(id);
            return;
        };
        let runtime = state.runtime(id);
        let Some(running) = &runtime.running else {
            self.options.registry.remove_group(id);
            return;
        };
        let mut taken = BTreeSet::new();
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        for tool in &runtime.tools {
            let tool_id = tool_id(&record.id, &tool.name, &mut taken);
            let choice = record.tools.get(&tool.name);
            if !choice.is_none_or(|c| c.enabled) {
                continue;
            }
            let (risk, _) = effective_risk(choice, tool);
            tools.push(Arc::new(PluginTool::new(
                tool_id,
                tool,
                &record.name,
                risk,
                Arc::clone(running.process.client()),
            )));
        }
        if let Err(err) = self.options.registry.set_group(id, tools) {
            self.options.registry.remove_group(id);
            self.options.events.error(
                EventCategory::Plugins,
                format!("Tools of plugin {} were not added: {err}", record.name),
            );
        }
    }

    fn info(&self, record: &PluginRecord, runtime: Option<&Runtime>) -> PluginInfo {
        let status = if !record.enabled {
            PluginStatus::Disabled
        } else if record.approved_fingerprint.is_none() {
            PluginStatus::NeedsApproval {
                reason: "The plugin has not been approved.".into(),
            }
        } else {
            runtime
                .and_then(|r| r.status.clone())
                .unwrap_or(PluginStatus::Starting)
        };
        let mut taken = BTreeSet::new();
        let tools = runtime
            .map(|r| r.tools.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|tool| {
                let choice = record.tools.get(&tool.name);
                let (risk, risk_reset) = effective_risk(choice, tool);
                PluginToolInfo {
                    id: tool_id(&record.id, &tool.name, &mut taken),
                    name: tool.name.clone(),
                    title: tool.title.clone().unwrap_or_else(|| tool.name.clone()),
                    description: clean_description(&tool.description),
                    enabled: choice.is_none_or(|c| c.enabled),
                    risk,
                    risk_reset,
                    hints: tool.hints.clone(),
                }
            })
            .collect();
        let secrets_set = runtime.map(|r| &r.secrets_set);
        PluginInfo {
            id: record.id.clone(),
            name: record.name.clone(),
            version: record.version.clone(),
            description: record.description.clone(),
            author: record.author.clone(),
            homepage: record.homepage.clone(),
            source: source_kind(&record.source),
            folder: record.folder().map(|f| f.display().to_string()),
            launch: launch_info(
                &record.launch,
                runtime.and_then(|r| r.resolved_program.clone()),
                record
                    .folder()
                    .unwrap_or_else(|| self.default_cwd(&record.id)),
            ),
            enabled: record.enabled,
            status,
            server: runtime.and_then(|r| r.server.clone()),
            tools,
            tool_problems: runtime.map(|r| r.tool_problems.clone()).unwrap_or_default(),
            secrets: record
                .secrets
                .iter()
                .map(|s| PluginSecretInfo {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    set: secrets_set.is_some_and(|set| set.contains(&s.name)),
                })
                .collect(),
            warnings: runtime.map(|r| r.warnings.clone()).unwrap_or_default(),
            fingerprint: record.approved_fingerprint.clone(),
            added_at_ms: record.added_at_ms,
        }
    }

    /// Resolves, checks and fingerprints a plugin (reads files).
    async fn inspect(&self, record: &PluginRecord) -> Result<(ResolvedLaunch, String)> {
        let record = record.clone();
        let default_cwd = self.default_cwd(&record.id);
        tokio::task::spawn_blocking(move || {
            let folder = record.folder();
            let manifest_bytes = match &folder {
                Some(folder) => Some(read_manifest(folder)?),
                None => None,
            };
            if let Some(bytes) = &manifest_bytes {
                Manifest::parse(bytes)?;
            }
            let mut resolved = launch::resolve(&record.launch, folder.as_deref(), &default_cwd)?;
            let fingerprint = launch::fingerprint(
                &mut resolved,
                &record.secret_names(),
                manifest_bytes.as_deref(),
            )?;
            Ok((resolved, fingerprint))
        })
        .await
        .map_err(|err| PluginError::Invalid(format!("checking the plugin failed: {err}")))?
    }

    async fn launch(&self, record: &PluginRecord, logs: &LogTail) -> Result<Started, StartError> {
        let (resolved, fingerprint) = self.inspect(record).await.map_err(|err| {
            StartError::NeedsApproval(format!("The plugin cannot be checked: {err}"))
        })?;
        if record.approved_fingerprint.as_deref() != Some(fingerprint.as_str()) {
            return Err(StartError::NeedsApproval(
                "The plugin changed since you approved it (its program, files or settings). Review it again.".into(),
            ));
        }
        let mut env: BTreeMap<String, String> = inherited_environment().into_iter().collect();
        env.extend(resolved.env.clone());
        let secrets = {
            let store = Arc::clone(&self.options.secrets);
            let keys: Vec<(String, String)> = record
                .secrets
                .iter()
                .map(|s| (s.name.clone(), secret_key(&record.id, &s.name)))
                .collect();
            tokio::task::spawn_blocking(move || {
                keys.into_iter()
                    .map(|(name, key)| {
                        let value = store.get_secret(&key);
                        (name, value)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|err| StartError::Failed(format!("reading secrets failed: {err}")))?
        };
        let mut missing = Vec::new();
        for (name, value) in secrets {
            match value {
                Ok(Some(value)) => {
                    env.insert(name, value);
                }
                Ok(None) => missing.push(name),
                Err(err) => {
                    return Err(StartError::Failed(format!(
                        "cannot read the secret {name}: {err}"
                    )));
                }
            }
        }
        if !missing.is_empty() {
            return Err(StartError::NeedsSecrets(missing));
        }
        if record.folder().is_none() && record.launch.cwd.is_none() {
            tokio::fs::create_dir_all(&resolved.cwd)
                .await
                .map_err(|err| {
                    StartError::Failed(format!("cannot create the working folder: {err}"))
                })?;
        }
        let spec = LaunchSpec {
            program: resolved.program.clone(),
            args: resolved.args.clone(),
            cwd: resolved.cwd.clone(),
            env: env.into_iter().collect(),
        };
        let process = ServerProcess::start(
            &spec,
            self.options.adapter.as_ref(),
            self.options.client.clone(),
            self.options.connect_timeout,
            logs,
        )
        .await
        .map_err(|err| StartError::Failed(with_logs(&err.to_string(), logs)))?;
        let process = Arc::new(process);
        let (tools, problems) = match process
            .client()
            .list_tools(self.options.request_timeout)
            .await
        {
            Ok(listed) => listed,
            Err(err) => {
                if process.description().has_tools {
                    process.shutdown().await;
                    return Err(StartError::Failed(with_logs(
                        &format!("listing the tools failed: {err}"),
                        logs,
                    )));
                }
                (Vec::new(), vec!["the server offers no tools".to_owned()])
            }
        };
        Ok(Started {
            process,
            tools,
            problems,
            resolved,
        })
    }
}

fn source_kind(source: &Source) -> PluginSourceKind {
    match source {
        Source::Folder { .. } => PluginSourceKind::Folder,
        Source::Command => PluginSourceKind::Command,
    }
}

fn launch_info(config: &LaunchConfig, resolved: Option<String>, cwd: PathBuf) -> LaunchInfo {
    LaunchInfo {
        program: config.program.clone(),
        resolved_program: resolved,
        args: config.args.clone(),
        cwd: config
            .cwd
            .clone()
            .unwrap_or_else(|| cwd.display().to_string()),
        env_names: config.env.keys().cloned().collect(),
    }
}

/// Risk for calls and whether a lowered risk was reset by a change.
fn effective_risk(choice: Option<&ToolChoice>, tool: &McpTool) -> (RiskLevel, bool) {
    match choice {
        Some(choice) if choice.risk != RiskLevel::High => {
            if choice.definition.as_deref() == Some(definition_hash(tool).as_str()) {
                (choice.risk, false)
            } else {
                (RiskLevel::High, true)
            }
        }
        _ => (RiskLevel::High, false),
    }
}

fn read_manifest(folder: &Path) -> Result<Vec<u8>> {
    let path = folder.join(MANIFEST_FILE);
    let metadata = std::fs::metadata(&path).map_err(|_| {
        PluginError::Invalid(format!("{} has no {MANIFEST_FILE}", folder.display()))
    })?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(PluginError::Invalid(format!(
            "{MANIFEST_FILE} is not a small file"
        )));
    }
    std::fs::read(&path).map_err(|err| PluginError::io(format!("reading {}", path.display()), err))
}

/// Starts a plugin if it is enabled and not running.
async fn start(inner: Arc<Inner>, id: String) {
    let (record, generation, logs) = {
        let mut state = inner.state();
        let Some(record) = state.record(&id).cloned() else {
            return;
        };
        if !record.enabled || record.approved_fingerprint.is_none() {
            return;
        }
        let runtime = state.runtime(&id);
        if runtime.running.is_some() || runtime.status == Some(PluginStatus::Starting) {
            return;
        }
        runtime.generation += 1;
        runtime.status = Some(PluginStatus::Starting);
        (record, runtime.generation, runtime.logs.clone())
    };
    inner.changed();

    let result = inner.launch(&record, &logs).await;
    let stale = {
        let mut state = inner.state();
        let current = state.runtime.get(&id).map(|r| r.generation) == Some(generation)
            && state.record(&id).is_some_and(|r| r.enabled)
            && !inner.shutdown.is_cancelled();
        apply_start(
            &inner, &mut state, &id, &record, generation, current, result,
        )
    };
    if let Some(process) = stale {
        process.shutdown().await;
        return;
    }
    inner.changed();
}

/// Records the outcome of a start; returns a process that is no longer
/// wanted.
fn apply_start(
    inner: &Arc<Inner>,
    state: &mut State,
    id: &str,
    record: &PluginRecord,
    generation: u64,
    current: bool,
    result: Result<Started, StartError>,
) -> Option<Arc<ServerProcess>> {
    let status = match result {
        Ok(started) if !current => return Some(started.process),
        Ok(started) => {
            let description = started.process.description().clone();
            let runtime = state.runtime(id);
            runtime.server = Some(ServerDetails {
                name: description.name.clone(),
                version: description.version.clone(),
                title: description.title.clone(),
                protocol_version: description.protocol_version.clone(),
                instructions: description.instructions.clone(),
            });
            runtime.tools = started.tools;
            runtime.tool_problems = started.problems;
            runtime.resolved_program = Some(started.resolved.program.display().to_string());
            runtime.warnings = started.resolved.warnings;
            runtime.running = Some(Running {
                process: Arc::clone(&started.process),
                started: Instant::now(),
            });
            runtime.status = Some(PluginStatus::Running);
            let count = runtime.tools.len();
            inner.register(state, id);
            inner.options.events.info(
                EventCategory::Plugins,
                format!(
                    "Plugin {} started: {} {} (MCP {}), {count} tool(s)",
                    record.name,
                    description.name,
                    description.version,
                    description.protocol_version
                ),
            );
            tokio::spawn(watch(
                Arc::clone(inner),
                id.to_owned(),
                generation,
                started.process,
            ));
            None
        }
        Err(StartError::NeedsApproval(reason)) => Some(PluginStatus::NeedsApproval { reason }),
        Err(StartError::NeedsSecrets(names)) => Some(PluginStatus::NeedsSecrets { names }),
        Err(StartError::Failed(error)) => {
            inner.options.events.warning(
                EventCategory::Plugins,
                format!(
                    "Plugin {} did not start: {}",
                    record.name,
                    first_line(&error)
                ),
            );
            Some(PluginStatus::Failed {
                error,
                retry_in_secs: None,
            })
        }
    };
    if let Some(status) = status
        && current
    {
        state.runtime(id).status = Some(status);
    }
    None
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// Follows a running server: tool list changes and its exit.
async fn watch(inner: Arc<Inner>, id: String, generation: u64, process: Arc<ServerProcess>) {
    let mut changes = process.client().tools_changed();
    loop {
        tokio::select! {
            reason = process.exited() => {
                on_exit(&inner, &id, generation, &reason);
                return;
            }
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
                refresh(&inner, &id, generation, &process).await;
                tokio::time::sleep(REFRESH_PAUSE).await;
            }
            () = inner.shutdown.cancelled() => return,
        }
    }
}

async fn refresh(inner: &Arc<Inner>, id: &str, generation: u64, process: &ServerProcess) {
    let listed = process
        .client()
        .list_tools(inner.options.request_timeout)
        .await;
    let mut state = inner.state();
    if state.runtime.get(id).map(|r| r.generation) != Some(generation) {
        return;
    }
    match listed {
        Ok((tools, problems)) => {
            let runtime = state.runtime(id);
            runtime.tools = tools;
            runtime.tool_problems = problems;
            inner.register(&mut state, id);
        }
        Err(err) => {
            state.runtime(id).tool_problems = vec![format!("refreshing the tools failed: {err}")];
        }
    }
    drop(state);
    inner.changed();
}

fn on_exit(inner: &Arc<Inner>, id: &str, generation: u64, reason: &str) {
    let mut state = inner.state();
    let name = state
        .record(id)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| id.to_owned());
    let delays = inner.options.restart_delays.clone();
    let Some(runtime) = state.runtime.get_mut(id) else {
        return;
    };
    if runtime.generation != generation {
        return;
    }
    let ran = runtime.running.take().map(|r| r.started.elapsed());
    if ran.is_some_and(|ran| ran >= HEALTHY_RUN) {
        runtime.restarts = 0;
    }
    let error = with_logs(reason, &runtime.logs);
    let delay = delays.get(runtime.restarts).copied();
    runtime.status = Some(PluginStatus::Failed {
        error,
        retry_in_secs: delay.map(|d| u32::try_from(d.as_secs().max(1)).unwrap_or(u32::MAX)),
    });
    if delay.is_some() {
        runtime.restarts += 1;
    }
    inner.register(&mut state, id);
    drop(state);
    inner.options.events.warning(
        EventCategory::Plugins,
        match delay {
            Some(delay) => format!(
                "Plugin {name} stopped unexpectedly ({reason}); restarting in {} s",
                delay.as_secs().max(1)
            ),
            None => format!("Plugin {name} stopped unexpectedly ({reason}); not restarted again"),
        },
    );
    inner.changed();
    if let Some(delay) = delay {
        let inner = Arc::clone(inner);
        let id = id.to_owned();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = inner.shutdown.cancelled() => return,
            }
            let unchanged =
                inner.state().runtime.get(&id).map(|r| r.generation) == Some(generation);
            if unchanged {
                start(inner, id).await;
            }
        });
    }
}

/// Stops a plugin's server and removes its tools.
async fn stop(inner: &Inner, id: &str) {
    let running = {
        let mut state = inner.state();
        inner.options.registry.remove_group(id);
        state.runtime.get_mut(id).and_then(|runtime| {
            runtime.generation += 1;
            runtime.status = None;
            runtime.running.take()
        })
    };
    if let Some(running) = running {
        running.process.shutdown().await;
    }
}

impl PluginManager {
    /// Loads `plugins.json`. Servers start with [`Self::start_enabled`].
    pub async fn open(options: PluginManagerOptions) -> Result<Self> {
        let loaded = store::load(&options.store_path)
            .await
            .map_err(|err| PluginError::Store(err.to_string()))?;
        if let Some((backup, problem)) = &loaded.moved_to {
            options.events.error(
                EventCategory::Plugins,
                format!(
                    "plugins.json could not be read ({problem}); it was kept as {} and plugins start empty",
                    backup.display()
                ),
            );
        }
        let inner = Arc::new(Inner {
            options,
            state: Mutex::new(State {
                records: loaded.plugins,
                ..State::default()
            }),
            changes: broadcast::channel(32).0,
            save_lock: tokio::sync::Mutex::new(()),
            shutdown: CancellationToken::new(),
        });
        let records = inner.state().records.clone();
        let secrets = {
            let inner = Arc::clone(&inner);
            tokio::task::spawn_blocking(move || {
                records
                    .iter()
                    .map(|r| (r.id.clone(), inner.secrets_set(r)))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default()
        };
        {
            let mut state = inner.state();
            for (id, set) in secrets {
                state.runtime(&id).secrets_set = set;
            }
        }
        Ok(Self { inner })
    }

    /// Starts every enabled, approved plugin in the background.
    pub fn start_enabled(&self) {
        let ids: Vec<String> = self
            .inner
            .state()
            .records
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.id.clone())
            .collect();
        for id in ids {
            tokio::spawn(start(Arc::clone(&self.inner), id));
        }
    }

    /// Fires on every change of a plugin (status, tools, settings).
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.changes.subscribe()
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let state = self.inner.state();
        state
            .records
            .iter()
            .map(|record| self.inner.info(record, state.runtime.get(&record.id)))
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<PluginInfo> {
        let state = self.inner.state();
        let record = state
            .record(id)
            .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
        Ok(self.inner.info(record, state.runtime.get(id)))
    }

    /// The server's recent stderr lines.
    pub fn logs(&self, id: &str) -> Result<Vec<String>> {
        let mut state = self.inner.state();
        if state.record(id).is_none() {
            return Err(PluginError::NotFound(id.to_owned()));
        }
        Ok(state.runtime(id).logs.last(200))
    }

    async fn make_review(&self, record: PluginRecord) -> Result<PluginReview> {
        let (resolved, fingerprint) = self.inner.inspect(&record).await?;
        let replaces = self.inner.state().record(&record.id).is_some();
        let review = PluginReview {
            review_id: Uuid::new_v4().to_string(),
            source: source_kind(&record.source),
            id: record.id.clone(),
            name: record.name.clone(),
            version: record.version.clone(),
            description: record.description.clone(),
            author: record.author.clone(),
            homepage: record.homepage.clone(),
            folder: record.folder().map(|f| f.display().to_string()),
            launch: LaunchInfo {
                program: record.launch.program.clone(),
                resolved_program: Some(resolved.program.display().to_string()),
                args: resolved.args.clone(),
                cwd: resolved.cwd.display().to_string(),
                env_names: resolved.env.keys().cloned().collect(),
            },
            secrets: record.secrets.clone(),
            fingerprint,
            warnings: resolved.warnings,
            replaces,
        };
        let mut state = self.inner.state();
        state
            .reviews
            .retain(|_, pending| pending.created.elapsed() < REVIEW_LIFETIME);
        if state.reviews.len() >= MAX_REVIEWS {
            return Err(PluginError::Invalid(
                "too many open reviews; finish or close one".into(),
            ));
        }
        state.reviews.insert(
            review.review_id.clone(),
            PendingReview {
                review: review.clone(),
                record,
                created: Instant::now(),
            },
        );
        Ok(review)
    }

    /// Reads a plugin folder for review. Nothing is saved or started.
    pub async fn review_folder(&self, folder: PathBuf) -> Result<PluginReview> {
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(PluginError::Invalid(format!(
                "{} is not a folder",
                folder.display()
            )));
        }
        let bytes = {
            let folder = folder.clone();
            tokio::task::spawn_blocking(move || read_manifest(&folder))
                .await
                .map_err(|err| PluginError::Invalid(err.to_string()))??
        };
        let manifest = Manifest::parse(&bytes)?;
        let record = PluginRecord {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            author: manifest.author,
            homepage: manifest.homepage,
            source: Source::Folder {
                path: folder.display().to_string(),
            },
            launch: LaunchConfig {
                program: manifest.server.command,
                args: manifest.server.args,
                cwd: None,
                env: manifest.server.env,
            },
            secrets: manifest.server.secrets,
            enabled: true,
            approved_fingerprint: None,
            tools: BTreeMap::new(),
            added_at_ms: now_ms(),
        };
        self.make_review(record).await
    }

    /// Checks an MCP server entered by the user. Nothing is saved or
    /// started.
    pub async fn review_command(&self, request: McpServerRequest) -> Result<PluginReview> {
        let name = request.name.trim().to_owned();
        manifest::check_name(&name)?;
        let program = request.program.trim().to_owned();
        if program.is_empty() || program.len() > 1024 {
            return Err(PluginError::Invalid(
                "the program is empty or too long".into(),
            ));
        }
        let env: BTreeMap<String, String> = request
            .env
            .iter()
            .map(|v| (v.name.trim().to_owned(), v.value.clone()))
            .collect();
        if env.len() != request.env.len() {
            return Err(PluginError::Invalid(
                "an environment variable is set twice".into(),
            ));
        }
        let secret_names: Vec<String> = request
            .secret_names
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        manifest::check_launch(
            &request.args,
            env.iter(),
            secret_names.iter().map(String::as_str),
        )?;
        let cwd = request
            .cwd
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        let record = PluginRecord {
            id: manifest::id_from_name(&name),
            name,
            version: None,
            description: None,
            author: None,
            homepage: None,
            source: Source::Command,
            launch: LaunchConfig {
                program,
                args: request.args,
                cwd,
                env,
            },
            secrets: secret_names
                .into_iter()
                .map(|name| SecretRequirement {
                    name,
                    description: None,
                })
                .collect(),
            enabled: true,
            approved_fingerprint: None,
            tools: BTreeMap::new(),
            added_at_ms: now_ms(),
        };
        self.make_review(record).await
    }

    /// A review of an installed plugin, to approve it again after a change.
    pub async fn review_existing(&self, id: &str) -> Result<PluginReview> {
        let record = self
            .inner
            .state()
            .record(id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
        let mut record = match record.folder() {
            // A manifest plugin is reviewed as its folder is now.
            Some(folder) => {
                let review = self.review_folder(folder).await?;
                let pending = self.inner.state().reviews.remove(&review.review_id);
                pending
                    .map(|p| p.record)
                    .ok_or_else(|| PluginError::Invalid("the review expired".into()))?
            }
            None => record,
        };
        if record.id != id {
            return Err(PluginError::Invalid(format!(
                "the manifest now has the id \"{}\"; remove the plugin and add it again",
                record.id
            )));
        }
        record.approved_fingerprint = None;
        self.make_review(record).await
    }

    /// Saves and starts a reviewed plugin, if nothing changed since the
    /// review. An installed plugin with the same id is replaced; its tool
    /// choices and secrets are kept.
    pub async fn install(&self, review_id: &str) -> Result<PluginInfo> {
        let pending = self
            .inner
            .state()
            .reviews
            .remove(review_id)
            .ok_or_else(|| {
                PluginError::NotFound("the review (it expired; review the plugin again)".into())
            })?;
        let mut record = pending.record;
        let (_, fingerprint) = self.inner.inspect(&record).await?;
        if fingerprint != pending.review.fingerprint {
            return Err(PluginError::NeedsApproval(
                "The plugin changed after the review. Review it again.".into(),
            ));
        }
        record.approved_fingerprint = Some(fingerprint);
        record.enabled = true;
        let replaced = {
            let state = self.inner.state();
            if state.record(&record.id).is_none() && state.records.len() >= MAX_PLUGINS {
                return Err(PluginError::Invalid(format!(
                    "at most {MAX_PLUGINS} plugins"
                )));
            }
            state.record(&record.id).cloned()
        };
        if let Some(existing) = &replaced {
            stop(&self.inner, &existing.id).await;
            record.tools = existing.tools.clone();
            record.added_at_ms = existing.added_at_ms;
        }
        let secrets_set = {
            let inner = Arc::clone(&self.inner);
            let record = record.clone();
            tokio::task::spawn_blocking(move || inner.secrets_set(&record))
                .await
                .unwrap_or_default()
        };
        {
            let mut state = self.inner.state();
            match state.record_mut(&record.id) {
                Some(existing) => *existing = record.clone(),
                None => state.records.push(record.clone()),
            }
            state.runtime(&record.id).secrets_set = secrets_set;
        }
        self.inner.save().await?;
        // Arguments are left out: users sometimes pass tokens there, and the
        // event log is also written to the log files.
        let fingerprint = record.approved_fingerprint.as_deref().unwrap_or_default();
        self.inner.options.events.info(
            EventCategory::Plugins,
            format!(
                "Plugin {} {}: runs {}, fingerprint {}…",
                record.name,
                if replaced.is_some() {
                    "approved again"
                } else {
                    "added"
                },
                record.launch.program,
                fingerprint.get(..16).unwrap_or(fingerprint)
            ),
        );
        self.inner.changed();
        tokio::spawn(start(Arc::clone(&self.inner), record.id.clone()));
        self.get(&record.id)
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginInfo> {
        let name = {
            let mut state = self.inner.state();
            let record = state
                .record_mut(id)
                .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
            record.enabled = enabled;
            record.name.clone()
        };
        self.inner.save().await?;
        if enabled {
            self.inner.state().runtime(id).restarts = 0;
            tokio::spawn(start(Arc::clone(&self.inner), id.to_owned()));
        } else {
            stop(&self.inner, id).await;
        }
        self.inner.options.events.info(
            EventCategory::Plugins,
            format!(
                "Plugin {name} {}",
                if enabled { "enabled" } else { "disabled" }
            ),
        );
        self.inner.changed();
        self.get(id)
    }

    /// Stops the server and starts it again.
    pub async fn restart(&self, id: &str) -> Result<PluginInfo> {
        if self.inner.state().record(id).is_none() {
            return Err(PluginError::NotFound(id.to_owned()));
        }
        stop(&self.inner, id).await;
        self.inner.state().runtime(id).restarts = 0;
        tokio::spawn(start(Arc::clone(&self.inner), id.to_owned()));
        self.get(id)
    }

    /// Asks the running server for its tools again.
    pub async fn refresh_tools(&self, id: &str) -> Result<PluginInfo> {
        let running = {
            let mut state = self.inner.state();
            if state.record(id).is_none() {
                return Err(PluginError::NotFound(id.to_owned()));
            }
            let runtime = state.runtime(id);
            runtime
                .running
                .as_ref()
                .map(|r| (Arc::clone(&r.process), runtime.generation))
        };
        let Some((process, generation)) = running else {
            return Err(PluginError::Invalid("the plugin is not running".into()));
        };
        refresh(&self.inner, id, generation, &process).await;
        self.get(id)
    }

    /// Turns a tool on or off and sets its risk. A risk below high is bound
    /// to the tool's current definition.
    pub async fn set_tool(&self, update: PluginToolUpdate) -> Result<PluginInfo> {
        {
            let mut state = self.inner.state();
            let definition = state
                .runtime(&update.plugin_id)
                .tools
                .iter()
                .find(|t| t.name == update.tool)
                .map(definition_hash);
            let record = state
                .record_mut(&update.plugin_id)
                .ok_or_else(|| PluginError::NotFound(update.plugin_id.clone()))?;
            let Some(definition) = definition else {
                return Err(PluginError::Invalid(format!(
                    "the plugin has no tool \"{}\"",
                    update.tool
                )));
            };
            record.tools.insert(
                update.tool.clone(),
                ToolChoice {
                    enabled: update.enabled,
                    risk: update.risk,
                    definition: (update.risk != RiskLevel::High).then_some(definition),
                },
            );
            self.inner.register(&mut state, &update.plugin_id);
        }
        self.inner.save().await?;
        self.inner.changed();
        self.get(&update.plugin_id)
    }

    fn secret_owner(&self, id: &str, name: &str) -> Result<()> {
        let state = self.inner.state();
        let record = state
            .record(id)
            .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
        if record.secrets.iter().any(|s| s.name == name) {
            Ok(())
        } else {
            Err(PluginError::Invalid(format!(
                "the plugin does not use a secret named {name}"
            )))
        }
    }

    /// Stores a secret in the OS keychain and starts the plugin if it was
    /// waiting for secrets.
    pub async fn set_secret(&self, id: &str, name: &str, value: &str) -> Result<PluginInfo> {
        self.secret_owner(id, name)?;
        if value.is_empty() || value.len() > 8192 {
            return Err(PluginError::Invalid(
                "the secret is empty or too long".into(),
            ));
        }
        let secrets = Arc::clone(&self.inner.options.secrets);
        let key = secret_key(id, name);
        let value = value.to_owned();
        tokio::task::spawn_blocking(move || secrets.set_secret(&key, &value))
            .await
            .map_err(|err| PluginError::Secret(err.to_string()))?
            .map_err(|err| PluginError::Secret(err.to_string()))?;
        let waiting = {
            let mut state = self.inner.state();
            let runtime = state.runtime(id);
            runtime.secrets_set.insert(name.to_owned());
            matches!(runtime.status, Some(PluginStatus::NeedsSecrets { .. }))
        };
        self.inner.changed();
        if waiting {
            tokio::spawn(start(Arc::clone(&self.inner), id.to_owned()));
        }
        self.get(id)
    }

    pub async fn clear_secret(&self, id: &str, name: &str) -> Result<PluginInfo> {
        self.secret_owner(id, name)?;
        let secrets = Arc::clone(&self.inner.options.secrets);
        let key = secret_key(id, name);
        tokio::task::spawn_blocking(move || secrets.delete_secret(&key))
            .await
            .map_err(|err| PluginError::Secret(err.to_string()))?
            .map_err(|err| PluginError::Secret(err.to_string()))?;
        self.inner.state().runtime(id).secrets_set.remove(name);
        self.inner.changed();
        self.get(id)
    }

    /// Stops the plugin and deletes its settings and keychain secrets. The
    /// plugin's folder is left alone.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let record = self
            .inner
            .state()
            .record(id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(id.to_owned()))?;
        stop(&self.inner, id).await;
        {
            let mut state = self.inner.state();
            state.records.retain(|r| r.id != id);
            state.runtime.remove(id);
        }
        self.inner.save().await?;
        let secrets = Arc::clone(&self.inner.options.secrets);
        let keys: Vec<String> = record
            .secrets
            .iter()
            .map(|s| secret_key(id, &s.name))
            .collect();
        let failed = tokio::task::spawn_blocking(move || {
            keys.iter()
                .filter(|key| secrets.delete_secret(key).is_err())
                .count()
        })
        .await
        .unwrap_or(0);
        if failed > 0 {
            self.inner.options.events.warning(
                EventCategory::Plugins,
                format!(
                    "{failed} secret(s) of plugin {} stay in the keychain",
                    record.name
                ),
            );
        }
        self.inner.options.events.info(
            EventCategory::Plugins,
            format!("Plugin {} removed", record.name),
        );
        self.inner.changed();
        Ok(())
    }

    /// Stops every server.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let ids: Vec<String> = self
            .inner
            .state()
            .records
            .iter()
            .map(|r| r.id.clone())
            .collect();
        futures::future::join_all(ids.iter().map(|id| stop(&self.inner, id))).await;
    }
}
