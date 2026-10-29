//! Agent Orchestrator: runs tasks through planner → executor → verifier,
//! with recovery, limits, pause/resume/cancel and a persisted journal.
//!
//! Every tool call goes through the Security Policy Engine; calls that need
//! approval wait for the user (the task shows "waiting for approval"). The
//! orchestrator never repeats an action on its own: after a failure the
//! model is told what happened and decides what to do next, and risky
//! actions need approval every time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jarvis_ai::ChatMessage;
use jarvis_security::{
    ApprovalBroker, ApprovalOutcome, ApprovalRequest, AuditEvent, AuditLog, NewAuditEntry,
    PolicyDecision, RiskLevel, SecurityPolicy,
};
use jarvis_tools::{ProcessRegistry, ToolContext, ToolLimits, ToolRegistry};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::protocol::{self, Reply};
use crate::store::TaskStore;
use crate::types::{
    AgentTask, MAX_DURATION_SECS, MAX_GOAL_CHARS, MAX_STEPS, StepKind, StepOutcome, TaskLimits,
    TaskStatus, TaskStep, TaskSummary,
};

/// Model calls in a row that may fail before the task fails.
const MAX_MODEL_ERRORS: u32 = 2;
/// Replies in a row that are not valid protocol JSON.
const MAX_INVALID_REPLIES: u32 = 3;
/// Tool calls in a row that may fail or be denied.
const MAX_FAILED_ACTIONS: u32 = 3;
/// Answers that the verifier may send back.
const MAX_VERIFICATION_ROUNDS: u32 = 1;
const MAX_STEP_TEXT: usize = 4000;
const TOOL_RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// A chat model the agent talks to.
#[async_trait]
pub trait AgentModel: Send + Sync {
    fn provider_id(&self) -> Option<String>;
    fn model(&self) -> String;
    /// Characters of conversation the model can take (its context minus
    /// room for the answer).
    fn context_chars(&self) -> usize;
    async fn complete(&self, messages: Vec<ChatMessage>) -> Result<String>;

    fn supports_native_tools(&self) -> bool {
        false
    }

    async fn complete_tools(
        &self,
        _messages: Vec<jarvis_ai::ToolMessage>,
        _tools: Vec<jarvis_ai::ToolDefinition>,
    ) -> Result<jarvis_ai::ToolChatResponse> {
        Err(AgentError::Model("native tools are unavailable".into()))
    }
}

/// Settings read before every step, so changes apply to running tasks.
#[derive(Debug, Clone)]
pub struct AgentSettings {
    pub policy: SecurityPolicy,
    pub tool_limits: ToolLimits,
    pub approval_timeout: Duration,
}

pub type SettingsSource = Arc<dyn Fn() -> AgentSettings + Send + Sync>;

/// What the orchestrator uses from its host.
#[derive(Clone)]
pub struct AgentEnvironment {
    pub store: TaskStore,
    pub tools: Arc<ToolRegistry>,
    pub approvals: Arc<ApprovalBroker>,
    pub audit: Option<AuditLog>,
    pub processes: Arc<ProcessRegistry>,
    pub settings: SettingsSource,
    pub max_concurrent: usize,
}

/// A new task.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub goal: String,
    pub limits: TaskLimits,
}

struct Control {
    cancel: CancellationToken,
    pause: watch::Sender<bool>,
}

pub struct AgentManager {
    env: AgentEnvironment,
    /// Serialize read/modify/save lifecycle transitions across await points.
    transitions: tokio::sync::Mutex<()>,
    /// Failed checkpoints remain visible even while the database rejects writes.
    unsaved: Mutex<HashMap<Uuid, AgentTask>>,
    running: Mutex<HashMap<Uuid, Control>>,
    updates: broadcast::Sender<AgentTask>,
    /// Cancelled on shutdown: running tasks pause instead of failing.
    shutdown: CancellationToken,
}

impl std::fmt::Debug for AgentManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentManager")
            .field("running", &self.running().len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

impl AgentManager {
    pub fn new(env: AgentEnvironment) -> Arc<Self> {
        Arc::new(Self {
            env,
            transitions: tokio::sync::Mutex::new(()),
            unsaved: Mutex::default(),
            running: Mutex::default(),
            updates: broadcast::channel(64).0,
            shutdown: CancellationToken::new(),
        })
    }

    fn running(&self) -> MutexGuard<'_, HashMap<Uuid, Control>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn unsaved(&self) -> MutexGuard<'_, HashMap<Uuid, AgentTask>> {
        self.unsaved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every change of a task (status, new step).
    pub fn subscribe(&self) -> broadcast::Receiver<AgentTask> {
        self.updates.subscribe()
    }

    /// Pauses interrupted tasks, or fails them when a tool has no durable result.
    pub async fn recover(&self) -> Result<usize> {
        let _transition = self.transitions.lock().await;
        if !self.running().is_empty() {
            return Err(AgentError::Invalid(
                "cannot recover tasks while agents are running".into(),
            ));
        }
        let unfinished: Vec<_> = self
            .env
            .store
            .unfinished()
            .await?
            .into_iter()
            .filter(|task| !self.unsaved().contains_key(&task.id))
            .collect();
        for mut task in unfinished.iter().cloned() {
            task.status = TaskStatus::Paused;
            task.error =
                Some("JARVIS was closed while the task ran; resume it to continue".to_owned());
            for step in &mut task.steps {
                if step.outcome == StepOutcome::InProgress {
                    step.outcome = StepOutcome::Unknown;
                    step.output = Some(format!(
                        "Operation {}:{} has no durable result. It may have completed or partly happened. Check its effects manually before starting a new task; this task cannot be resumed.",
                        task.id, step.index
                    ));
                    task.error = step.output.clone();
                    task.status = TaskStatus::Failed;
                    task.finished_at_ms = Some(now_ms());
                }
            }
            task.updated_at_ms = now_ms();
            self.env.store.save(&task).await?;
        }
        Ok(unfinished.len())
    }

    pub async fn start(
        self: &Arc<Self>,
        new: NewTask,
        model: Arc<dyn AgentModel>,
    ) -> Result<AgentTask> {
        let goal = new.goal.trim().to_owned();
        if goal.is_empty() {
            return Err(AgentError::Invalid("the goal is empty".to_owned()));
        }
        if goal.chars().count() > MAX_GOAL_CHARS {
            return Err(AgentError::Invalid(format!(
                "the goal may have at most {MAX_GOAL_CHARS} characters"
            )));
        }
        let limits = TaskLimits {
            max_steps: new.limits.max_steps.clamp(1, MAX_STEPS),
            max_duration_secs: new.limits.max_duration_secs.clamp(30, MAX_DURATION_SECS),
        };
        let now = now_ms();
        let task = AgentTask {
            id: Uuid::new_v4(),
            goal,
            status: TaskStatus::Pending,
            provider_id: model.provider_id(),
            model: Some(model.model()),
            limits,
            plan: Vec::new(),
            steps: Vec::new(),
            result: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
        };
        let _transition = self.transitions.lock().await;
        self.launch(task.clone(), model).await?;
        Ok(task)
    }

    async fn launch(self: &Arc<Self>, task: AgentTask, model: Arc<dyn AgentModel>) -> Result<()> {
        // Caller holds transitions across any read that selected this task.
        if self.shutdown.is_cancelled() {
            return Err(AgentError::Invalid(
                "the agent manager is shutting down".into(),
            ));
        }
        let control = {
            let mut running = self.running();
            if running.contains_key(&task.id) {
                return Err(AgentError::Invalid("the task is already running".into()));
            }
            if running.len() >= self.env.max_concurrent {
                return Err(AgentError::LimitExceeded(format!(
                    "at most {} tasks can run at the same time",
                    self.env.max_concurrent
                )));
            }
            let control = Control {
                cancel: CancellationToken::new(),
                pause: watch::Sender::new(false),
            };
            let handles = (control.cancel.clone(), control.pause.subscribe());
            running.insert(task.id, control);
            handles
        };
        // Covers failed saves and dropped launch futures, then moves into
        // the spawned run so panic/unwind also releases the reservation.
        let reservation = RunningReservation {
            manager: Arc::clone(self),
            id: task.id,
        };
        self.env.store.save(&task).await?;
        let _ = self.updates.send(task.clone());
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let _reservation = reservation;
            let mut run = Run::new(Arc::clone(&manager), task, model, control.0, control.1);
            run.execute().await;
        });
        Ok(())
    }

    /// Stops a task. A paused task that is not running is marked cancelled.
    pub async fn cancel(&self, id: Uuid) -> Result<bool> {
        // Active work must remain cancellable even while another lifecycle
        // operation is waiting on storage. Recheck after locking for a run
        // that may have started in the meantime.
        if let Some(control) = self.running().get(&id) {
            control.cancel.cancel();
            return Ok(true);
        }
        let _transition = self.transitions.lock().await;
        if let Some(control) = self.running().get(&id) {
            control.cancel.cancel();
            return Ok(true);
        }
        let mut task = self.get(id).await?;
        if task.status.is_finished() {
            return Ok(false);
        }
        task.status = TaskStatus::Cancelled;
        task.finished_at_ms = Some(now_ms());
        task.updated_at_ms = now_ms();
        self.env.store.save(&task).await?;
        let _ = self.updates.send(task);
        Ok(true)
    }

    /// Pauses a running task before its next step.
    pub fn pause(&self, id: Uuid) -> bool {
        self.running()
            .get(&id)
            .is_some_and(|control| control.pause.send(true).is_ok())
    }

    /// Continues a paused task (also one interrupted by a restart).
    pub async fn resume(self: &Arc<Self>, id: Uuid, model: Arc<dyn AgentModel>) -> Result<()> {
        let _transition = self.transitions.lock().await;
        if let Some(control) = self.running().get(&id) {
            control.pause.send_replace(false);
            return Ok(());
        }
        let mut task = self.get(id).await?;
        if task
            .steps
            .iter()
            .any(|step| matches!(step.outcome, StepOutcome::InProgress | StepOutcome::Unknown))
        {
            return Err(AgentError::Invalid(
                "An action has an unknown result. Check its effects manually before starting a new task; it cannot be resumed.".into(),
            ));
        }
        if task.status != TaskStatus::Paused {
            return Err(AgentError::Invalid(
                "only paused tasks can be resumed".to_owned(),
            ));
        }
        task.error = None;
        task.provider_id = model.provider_id();
        task.model = Some(model.model());
        self.launch(task, model).await
    }

    pub async fn get(&self, id: Uuid) -> Result<AgentTask> {
        if let Some(task) = self.unsaved().get(&id).cloned() {
            return Ok(task);
        }
        let saved = self.env.store.get(id).await;
        // A checkpoint may have failed while this read was in flight.
        self.unsaved().get(&id).cloned().map(Ok).unwrap_or(saved)
    }

    pub async fn list(&self, limit: u32) -> Result<Vec<TaskSummary>> {
        let mut tasks = self.env.store.list(limit).await?;
        for task in self.unsaved().values() {
            tasks.retain(|row| row.id != task.id);
            tasks.push(TaskSummary::from(task));
        }
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at_ms));
        tasks.truncate(limit.clamp(1, 500) as usize);
        Ok(tasks)
    }

    pub async fn delete_finished(&self) -> Result<usize> {
        let _transition = self.transitions.lock().await;
        let unsaved: Vec<_> = self.unsaved().values().cloned().collect();
        for task in unsaved {
            if self.running().contains_key(&task.id) {
                continue;
            }
            // Persist the terminal snapshot before clearing it. If storage
            // still fails, retain it and return the error to the caller.
            self.env.store.save(&task).await?;
            self.unsaved().remove(&task.id);
        }
        self.env.store.delete_finished().await
    }

    pub fn running_count(&self) -> usize {
        self.running().len()
    }

    /// Pauses running tasks (they can be resumed after a restart) and
    /// waits briefly for them to save their state.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        for _ in 0..50 {
            if self.running().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

struct RunningReservation {
    manager: Arc<AgentManager>,
    id: Uuid,
}

impl Drop for RunningReservation {
    fn drop(&mut self) {
        self.manager.running().remove(&self.id);
    }
}

// ---------------------------------------------------------------------------

enum Stop {
    Cancelled,
    Shutdown,
    TimedOut,
    PersistenceFailed,
}

struct Run {
    manager: Arc<AgentManager>,
    task: AgentTask,
    model: Arc<dyn AgentModel>,
    cancel: CancellationToken,
    pause: watch::Receiver<bool>,
    /// Model replies and the observations that answered them.
    history: Vec<(String, String)>,
    started: Instant,
    persistence_error: Option<String>,
}

impl Run {
    fn new(
        manager: Arc<AgentManager>,
        task: AgentTask,
        model: Arc<dyn AgentModel>,
        cancel: CancellationToken,
        pause: watch::Receiver<bool>,
    ) -> Self {
        let history = rebuild_history(&task.steps);
        Self {
            manager,
            task,
            model,
            cancel,
            pause,
            history,
            started: Instant::now(),
            persistence_error: None,
        }
    }

    fn env(&self) -> &AgentEnvironment {
        &self.manager.env
    }

    async fn checkpoint(&mut self) {
        self.task.updated_at_ms = now_ms();
        self.apply_persistence_failure();
        if let Err(err) = self.env().store.save(&self.task).await {
            tracing::warn!("cannot save agent task {}: {err}", self.task.id);
            self.persistence_error
                .get_or_insert_with(|| format!("Cannot save the task journal: {err}"));
            self.apply_persistence_failure();
            self.manager
                .unsaved()
                .insert(self.task.id, self.task.clone());
        } else {
            self.manager.unsaved().remove(&self.task.id);
        }
        let _ = self.manager.updates.send(self.task.clone());
    }

    fn apply_persistence_failure(&mut self) {
        if let Some(error) = &self.persistence_error {
            self.task.status = TaskStatus::Failed;
            self.task.result = None;
            self.task.error = Some(format!(
                "{error}. The task stopped; completed or partial actions must be checked before retrying."
            ));
            self.task.finished_at_ms = Some(now_ms());
        }
    }

    fn push_step(&mut self, mut step: TaskStep) {
        step.index = self.task.steps.len() as u32;
        self.task.steps.push(step);
    }

    fn note(&mut self, text: impl Into<String>) {
        self.push_step(TaskStep {
            index: 0,
            kind: StepKind::Note,
            thought: None,
            tool: None,
            input: None,
            summary: None,
            decision: None,
            outcome: StepOutcome::Info,
            output: Some(text.into()),
            started_at_ms: now_ms(),
            duration_ms: 0,
        });
    }

    async fn finish(&mut self, status: TaskStatus, result: Option<String>, error: Option<String>) {
        self.task.status = status;
        self.task.result = result;
        self.task.error = error;
        if status.is_finished() {
            self.task.finished_at_ms = Some(now_ms());
        }
        self.checkpoint().await;
    }

    fn stop_requested(&self) -> Option<Stop> {
        if self.persistence_error.is_some() {
            Some(Stop::PersistenceFailed)
        } else if self.cancel.is_cancelled() {
            Some(Stop::Cancelled)
        } else if self.manager.shutdown.is_cancelled() {
            Some(Stop::Shutdown)
        } else if Instant::now() >= self.deadline() {
            Some(Stop::TimedOut)
        } else {
            None
        }
    }

    fn deadline(&self) -> Instant {
        self.started + Duration::from_secs(u64::from(self.task.limits.max_duration_secs))
    }

    async fn stopped(&mut self, stop: Stop) {
        match stop {
            Stop::PersistenceFailed => {
                self.finish(TaskStatus::Failed, None, None).await;
            }
            Stop::TimedOut => {
                self.finish(
                    TaskStatus::Failed,
                    None,
                    Some(format!(
                        "The time limit ({} s) was reached",
                        self.task.limits.max_duration_secs
                    )),
                )
                .await;
            }
            Stop::Cancelled => {
                self.note("Cancelled by the user");
                self.finish(TaskStatus::Cancelled, None, None).await;
            }
            Stop::Shutdown => {
                self.finish(
                    TaskStatus::Paused,
                    None,
                    Some("JARVIS was closed while the task ran; resume it to continue".to_owned()),
                )
                .await;
            }
        }
    }

    async fn execute(&mut self) {
        self.task.status = TaskStatus::Running;
        self.checkpoint().await;

        if self.task.plan.is_empty() {
            match self.plan().await {
                Ok(()) => {}
                Err(stop) => return self.stopped(stop).await,
            }
        }

        let (mut model_errors, mut invalid, mut failed_actions, mut verifications) = (0, 0, 0, 0);
        loop {
            if let Some(stop) = self.stop_requested() {
                return self.stopped(stop).await;
            }
            let paused = *self.pause.borrow();
            if paused && let Err(stop) = self.wait_while_paused().await {
                return self.stopped(stop).await;
            }
            let steps_used = self
                .task
                .steps
                .iter()
                .filter(|s| matches!(s.kind, StepKind::Action | StepKind::Answer))
                .count() as u32;
            if steps_used >= self.task.limits.max_steps {
                let limit = self.task.limits.max_steps;
                return self
                    .finish(
                        TaskStatus::Failed,
                        None,
                        Some(format!(
                            "The step limit ({limit}) was reached before the goal"
                        )),
                    )
                    .await;
            }
            let started = Instant::now();
            let reply = match self.ask_action().await {
                Ok(reply) => {
                    model_errors = 0;
                    reply
                }
                Err(Ok(err)) => {
                    model_errors += 1;
                    self.note(format!("The model failed: {err}"));
                    self.checkpoint().await;
                    if model_errors > MAX_MODEL_ERRORS {
                        return self
                            .finish(TaskStatus::Failed, None, Some(err.to_string()))
                            .await;
                    }
                    continue;
                }
                Err(Err(stop)) => return self.stopped(stop).await,
            };

            match protocol::parse_reply(&reply) {
                Err(feedback) => {
                    invalid += 1;
                    self.note(format!(
                        "The model's reply was not a valid action ({feedback}): {}",
                        clip(reply.trim(), 300)
                    ));
                    self.history.push((clip(&reply, 2000), feedback.to_owned()));
                    self.checkpoint().await;
                    if invalid >= MAX_INVALID_REPLIES {
                        return self
                            .finish(
                                TaskStatus::Failed,
                                None,
                                Some("The model did not follow the action format".to_owned()),
                            )
                            .await;
                    }
                }
                Ok(Reply::Final { thought, answer }) => {
                    invalid = 0;
                    self.push_step(TaskStep {
                        index: 0,
                        kind: StepKind::Answer,
                        thought: thought.clone(),
                        tool: None,
                        input: None,
                        summary: None,
                        decision: None,
                        outcome: StepOutcome::Succeeded,
                        output: Some(clip(&answer, MAX_STEP_TEXT)),
                        started_at_ms: now_ms(),
                        duration_ms: started.elapsed().as_millis() as u64,
                    });
                    self.checkpoint().await;
                    verifications += 1;
                    match self.verify(&answer).await {
                        Ok(None) => {
                            if self
                                .task
                                .steps
                                .last()
                                .is_some_and(|step| step.outcome == StepOutcome::Unverified)
                            {
                                return self.finish(TaskStatus::Unverified, Some(answer), Some("The answer could not be verified. Review it before relying on it.".into())).await;
                            }
                            return self.finish(TaskStatus::Completed, Some(answer), None).await;
                        }
                        Ok(Some(feedback)) => {
                            if verifications > MAX_VERIFICATION_ROUNDS {
                                return self
                                    .finish(
                                        TaskStatus::Failed,
                                        Some(answer),
                                        Some(format!("Answer verification failed: {feedback}")),
                                    )
                                    .await;
                            }
                            let reply = serde_json::json!({"thought": thought, "final": answer})
                                .to_string();
                            self.history.push((
                                reply,
                                format!("A check of your answer found a problem: {feedback}\nContinue working on the goal."),
                            ));
                        }
                        Err(stop) => return self.stopped(stop).await,
                    }
                }
                Ok(Reply::Action {
                    thought,
                    tool,
                    input,
                }) => {
                    invalid = 0;
                    let reply =
                        serde_json::json!({"thought": thought, "tool": tool, "input": input})
                            .to_string();
                    let action_index = self.task.steps.len();
                    let (mut step, stop) = self.act(thought, &tool, &input, started).await;
                    let status = match step.outcome {
                        StepOutcome::Succeeded => "ok",
                        StepOutcome::Denied => "denied",
                        _ => "error",
                    };
                    let observed = protocol::observation(
                        &tool,
                        status,
                        step.output.as_deref().unwrap_or_default(),
                    );
                    if step.outcome == StepOutcome::Succeeded {
                        failed_actions = 0;
                    } else {
                        failed_actions += 1;
                    }
                    if self.task.steps.len() > action_index {
                        step.index = action_index as u32;
                        self.task.steps[action_index] = step;
                    } else {
                        self.push_step(step);
                    }
                    self.history.push((reply, observed));
                    self.checkpoint().await;
                    // The interrupted action stays in the journal, so a
                    // resumed task knows it may have partly happened.
                    if let Some(stop) = stop {
                        return self.stopped(stop).await;
                    }
                    if failed_actions >= MAX_FAILED_ACTIONS {
                        return self
                            .finish(
                                TaskStatus::Failed,
                                None,
                                Some(format!(
                                    "{MAX_FAILED_ACTIONS} actions in a row failed or were denied"
                                )),
                            )
                            .await;
                    }
                }
            }
        }
    }

    async fn plan(&mut self) -> std::result::Result<(), Stop> {
        let started = Instant::now();
        let enabled = self.enabled_specs();
        let messages = protocol::plan_messages(&self.task.goal, &enabled);
        let (plan, output) = match self.ask(messages).await {
            Ok(reply) => match protocol::parse_plan(&reply) {
                Some(plan) => (plan, None),
                None => (
                    Vec::new(),
                    Some("The planner gave no usable plan; working without one".to_owned()),
                ),
            },
            Err(Ok(err)) => (
                Vec::new(),
                Some(format!("Planning failed ({err}); working without a plan")),
            ),
            Err(Err(stop)) => return Err(stop),
        };
        self.task.plan = plan.clone();
        self.push_step(TaskStep {
            index: 0,
            kind: StepKind::Plan,
            thought: None,
            tool: None,
            input: None,
            summary: None,
            decision: None,
            outcome: if plan.is_empty() {
                StepOutcome::Failed
            } else {
                StepOutcome::Succeeded
            },
            output: output.or_else(|| Some(plan.join("\n"))),
            started_at_ms: now_ms(),
            duration_ms: started.elapsed().as_millis() as u64,
        });
        self.checkpoint().await;
        Ok(())
    }

    /// `Ok(None)` when the answer is accepted (or cannot be checked).
    async fn verify(&mut self, answer: &str) -> std::result::Result<Option<String>, Stop> {
        let started = Instant::now();
        let evidence: Vec<String> = self
            .task
            .steps
            .iter()
            .filter(|s| s.kind == StepKind::Action)
            .map(|s| {
                format!(
                    "- {}: {}",
                    s.summary.as_deref().unwrap_or("action"),
                    clip(s.output.as_deref().unwrap_or_default(), 600)
                )
            })
            .collect();
        let evidence = if evidence.is_empty() {
            "(no tools were used)".to_owned()
        } else {
            evidence.join("\n")
        };
        let messages = protocol::verify_messages(&self.task.goal, answer, &evidence);
        let verdict = match self.ask(messages).await {
            Ok(reply) => protocol::parse_verdict(&reply),
            Err(Ok(_)) => None,
            Err(Err(stop)) => return Err(stop),
        };
        let (outcome, output, feedback) = match verdict {
            Some((true, _)) => (
                StepOutcome::Succeeded,
                "The answer was checked".to_owned(),
                None,
            ),
            None => (
                StepOutcome::Unverified,
                "The answer could not be checked".to_owned(),
                None,
            ),
            Some((false, feedback)) => {
                let feedback = if feedback.is_empty() {
                    "the answer does not reach the goal".to_owned()
                } else {
                    feedback
                };
                (StepOutcome::Failed, feedback.clone(), Some(feedback))
            }
        };
        self.push_step(TaskStep {
            index: 0,
            kind: StepKind::Verification,
            thought: None,
            tool: None,
            input: None,
            summary: None,
            decision: None,
            outcome,
            output: Some(clip(&output, MAX_STEP_TEXT)),
            started_at_ms: now_ms(),
            duration_ms: started.elapsed().as_millis() as u64,
        });
        self.checkpoint().await;
        Ok(feedback)
    }

    /// One tool call: prepare → policy → approval → run → audit. Returns
    /// the journal step and, when the task was stopped meanwhile, why.
    async fn act(
        &mut self,
        thought: Option<String>,
        tool_name: &str,
        input: &serde_json::Value,
        started: Instant,
    ) -> (TaskStep, Option<Stop>) {
        let settings = (self.env().settings)();
        let mut step = TaskStep {
            index: 0,
            kind: StepKind::Action,
            thought,
            tool: Some(tool_name.to_owned()),
            input: Some(input.to_string()),
            summary: None,
            decision: None,
            outcome: StepOutcome::Failed,
            output: None,
            started_at_ms: now_ms(),
            duration_ms: 0,
        };
        let finish = |mut step: TaskStep, outcome: StepOutcome, output: String| {
            step.outcome = outcome;
            step.output = Some(clip(&output, MAX_STEP_TEXT));
            step.duration_ms = started.elapsed().as_millis() as u64;
            step
        };

        let Some(tool) = self.env().tools.get(tool_name) else {
            let output = format!("There is no tool named \"{tool_name}\"");
            return (finish(step, StepOutcome::Failed, output), None);
        };
        let mut ctx = ToolContext::new(
            settings.policy.scope().clone(),
            settings.tool_limits,
            Arc::clone(&self.env().processes),
        );
        // Cancelled by the user, shutdown or the overall task deadline.
        ctx.cancel = self.cancel.child_token();
        let _shutdown_guard = {
            let shutdown = self.manager.shutdown.clone();
            let token = ctx.cancel.clone();
            let deadline = self.deadline();
            AbortOnDrop(tokio::spawn(async move {
                tokio::select! {
                    () = shutdown.cancelled() => {},
                    () = tokio::time::sleep_until(deadline) => {},
                }
                token.cancel();
            }))
        };
        let prepared = match tool.prepare(input, &ctx) {
            Ok(prepared) => prepared,
            // A path outside the workspace is a refusal, and is audited.
            Err(err @ jarvis_tools::ToolError::Scope(_)) => {
                let reason = err.to_string();
                self.record(NewAuditEntry {
                    task_id: Some(self.task.id),
                    tool: tool_name.to_owned(),
                    action: clip(&input.to_string(), 2000),
                    risk: tool.spec().risk,
                    event: AuditEvent::PolicyDenied,
                    detail: reason.clone(),
                })
                .await;
                step.decision = Some(format!("refused by policy: {reason}"));
                let output = format!("Refused by the security policy: {reason}");
                return (finish(step, StepOutcome::Denied, output), None);
            }
            Err(err) => return (finish(step, StepOutcome::Failed, err.to_string()), None),
        };
        let action = prepared.action.clone();
        step.summary = Some(action.summary.clone());
        let task_id = self.task.id;
        let audit = |event: AuditEvent, detail: String| NewAuditEntry {
            task_id: Some(task_id),
            tool: action.tool.clone(),
            action: action.summary.clone(),
            risk: action.risk,
            event,
            detail,
        };

        let decision = settings.policy.evaluate(&action);
        match decision {
            PolicyDecision::Deny { reason } => {
                self.record(audit(AuditEvent::PolicyDenied, reason.clone()))
                    .await;
                step.decision = Some(format!("refused by policy: {reason}"));
                let output = format!("Refused by the security policy: {reason}");
                return (finish(step, StepOutcome::Denied, output), None);
            }
            PolicyDecision::RequireApproval { reason } => {
                self.task.status = TaskStatus::WaitingForApproval;
                self.checkpoint().await;
                if let Some(stop) = self.stop_requested() {
                    return (
                        finish(
                            step,
                            StepOutcome::Denied,
                            "Not run: the task stopped before approval".into(),
                        ),
                        Some(stop),
                    );
                }
                let request = ApprovalRequest {
                    task_id: Some(self.task.id),
                    tool: action.tool.clone(),
                    risk: action.risk,
                    summary: action.summary.clone(),
                    reason: reason.clone(),
                    details: clip(
                        &serde_json::to_string_pretty(input).unwrap_or_default(),
                        MAX_STEP_TEXT,
                    ),
                };
                let approvals = Arc::clone(&self.env().approvals);
                let outcome = approvals
                    .request(request, settings.approval_timeout, &ctx.cancel)
                    .await;
                self.task.status = TaskStatus::Running;
                self.checkpoint().await;
                let (event, text) = match outcome {
                    ApprovalOutcome::Approved => (AuditEvent::Approved, "approved by you"),
                    ApprovalOutcome::Denied => (AuditEvent::Denied, "denied by you"),
                    ApprovalOutcome::TimedOut => (AuditEvent::TimedOut, "not approved in time"),
                    ApprovalOutcome::Cancelled
                        if matches!(self.stop_requested(), Some(Stop::TimedOut)) =>
                    {
                        (AuditEvent::TimedOut, "task time limit reached")
                    }
                    ApprovalOutcome::Cancelled => (AuditEvent::Cancelled, "cancelled"),
                };
                self.record(audit(event, reason)).await;
                step.decision = Some(text.to_owned());
                match outcome {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Cancelled => {
                        let output =
                            "Not run: the task was stopped before the user decided".to_owned();
                        let stop = self.stop_requested().unwrap_or(Stop::Cancelled);
                        return (finish(step, StepOutcome::Denied, output), Some(stop));
                    }
                    _ => {
                        let output = format!("The user did not allow this action ({text})");
                        return (finish(step, StepOutcome::Denied, output), None);
                    }
                }
            }
            PolicyDecision::Allow => {
                self.record(audit(AuditEvent::Allowed, clip(&input.to_string(), 2000)))
                    .await;
                step.decision = Some(
                    if action.risk == RiskLevel::Low {
                        "allowed (read-only)"
                    } else {
                        "allowed by your settings"
                    }
                    .to_owned(),
                );
            }
        }

        // A late approval must never start work after cancellation or expiry.
        if let Some(stop) = self.stop_requested() {
            self.record(audit(
                if matches!(&stop, Stop::TimedOut) {
                    AuditEvent::TimedOut
                } else {
                    AuditEvent::Cancelled
                },
                "stopped before execution".into(),
            ))
            .await;
            return (
                finish(
                    step,
                    StepOutcome::Denied,
                    "Not run: the task was stopped before execution".into(),
                ),
                Some(stop),
            );
        }
        // Save authorization and intent before any tool effects. The stable
        // operation identity is (task.id, step.index); replace this entry when
        // the outcome is known instead of appending a second action.
        step.index = self.task.steps.len() as u32;
        step.outcome = StepOutcome::InProgress;
        step.output = Some("Execution intent saved; no result recorded yet".into());
        self.push_step(step.clone());
        self.checkpoint().await;
        if let Some(stop) = self.stop_requested() {
            return (
                finish(
                    step,
                    StepOutcome::Denied,
                    "Not run: the task stopped before execution".into(),
                ),
                Some(stop),
            );
        }
        let deadline = self.deadline().min(Instant::now() + TOOL_RUN_TIMEOUT);
        let result = tokio::time::timeout_at(deadline, tool.run(&prepared, &ctx)).await;
        match result {
            Ok(Ok(output)) => {
                self.record(audit(AuditEvent::Succeeded, output.summary.clone()))
                    .await;
                let limit = (self.model.context_chars() / 4)
                    .clamp(1000, settings.tool_limits.max_output_chars);
                let output = clip(&output.content, limit);
                (finish(step, StepOutcome::Succeeded, output), None)
            }
            Ok(Err(jarvis_tools::ToolError::Cancelled)) => {
                self.record(audit(AuditEvent::Failed, "interrupted".to_owned()))
                    .await;
                let output = "Interrupted: the task was stopped while this ran; \
                              it may have partly happened"
                    .to_owned();
                let stop = self.stop_requested().unwrap_or(Stop::Cancelled);
                (finish(step, StepOutcome::Failed, output), Some(stop))
            }
            Ok(Err(err)) => {
                self.record(audit(AuditEvent::Failed, err.to_string()))
                    .await;
                (finish(step, StepOutcome::Failed, err.to_string()), None)
            }
            Err(_) => {
                ctx.cancel.cancel();
                self.record(audit(AuditEvent::Failed, "timed out".to_owned()))
                    .await;
                let output =
                    "The tool did not finish in time; it may have partly happened".to_owned();
                (
                    finish(step, StepOutcome::Failed, output),
                    self.stop_requested(),
                )
            }
        }
    }

    async fn record(&mut self, entry: NewAuditEntry) {
        if let Some(audit) = &self.env().audit
            && let Err(err) = audit.record(entry).await
        {
            tracing::warn!("cannot write the audit log: {err}");
            self.persistence_error
                .get_or_insert_with(|| format!("Cannot write the audit log: {err}"));
        }
    }

    /// `Err(Ok(error))`: the model failed; `Err(Err(stop))`: the task stopped.
    async fn ask_action(
        &self,
    ) -> std::result::Result<String, std::result::Result<AgentError, Stop>> {
        if let Some(stop) = self.stop_requested() {
            return Err(Err(stop));
        }
        if !self.model.supports_native_tools() {
            return self.ask(self.messages()).await;
        }
        let specs = self.enabled_specs();
        let settings = (self.env().settings)();
        let turn = crate::native_protocol::NativeTurn::new(&specs);
        let system = protocol::native_system_prompt(&turn.specs, settings.policy.scope().roots());
        let messages = turn.messages(self.messages_with_system(system, turn.schema_chars()));
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Err(Stop::Cancelled)),
            () = self.manager.shutdown.cancelled() => Err(Err(Stop::Shutdown)),
            () = tokio::time::sleep_until(self.deadline()) => Err(Err(Stop::TimedOut)),
            reply = self.model.complete_tools(messages, turn.definitions()) => {
                reply.map(|response| turn.normalize(response)).map_err(Ok)
            },
        }
    }

    async fn ask(
        &self,
        messages: Vec<ChatMessage>,
    ) -> std::result::Result<String, std::result::Result<AgentError, Stop>> {
        if let Some(stop) = self.stop_requested() {
            return Err(Err(stop));
        }
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Err(Stop::Cancelled)),
            () = self.manager.shutdown.cancelled() => Err(Err(Stop::Shutdown)),
            () = tokio::time::sleep_until(self.deadline()) => Err(Err(Stop::TimedOut)),
            reply = self.model.complete(messages) => reply.map_err(Ok),
        }
    }

    async fn wait_while_paused(&mut self) -> std::result::Result<(), Stop> {
        self.task.status = TaskStatus::Paused;
        self.note("Paused by the user");
        self.checkpoint().await;
        while *self.pause.borrow() {
            if let Some(stop) = self.stop_requested() {
                return Err(stop);
            }
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Stop::Cancelled),
                () = self.manager.shutdown.cancelled() => return Err(Stop::Shutdown),
                () = tokio::time::sleep_until(self.deadline()) => return Err(Stop::TimedOut),
                changed = self.pause.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
        }
        self.task.status = TaskStatus::Running;
        self.note("Resumed");
        self.checkpoint().await;
        Ok(())
    }

    fn enabled_specs(&self) -> Vec<jarvis_tools::ToolSpec> {
        let settings = (self.env().settings)();
        let enabled = &settings.policy.config().enabled_tools;
        self.env()
            .tools
            .specs()
            .into_iter()
            .filter(|spec| enabled.contains(&spec.name))
            .collect()
    }

    /// System prompt, goal and as much recent history as fits the model.
    fn messages(&self) -> Vec<ChatMessage> {
        let settings = (self.env().settings)();
        let system =
            protocol::system_prompt(&self.enabled_specs(), settings.policy.scope().roots());
        self.messages_with_system(system, 0)
    }

    fn messages_with_system(&self, system: String, schema_chars: usize) -> Vec<ChatMessage> {
        let goal = protocol::goal_message(&self.task.goal, &self.task.plan);
        let budget = self.model.context_chars();
        let mut used = system.len() + goal.len() + schema_chars;
        let mut kept = Vec::new();
        for (reply, observation) in self.history.iter().rev() {
            let size = reply.len() + observation.len();
            if used + size > budget && !kept.is_empty() {
                break;
            }
            used += size;
            kept.push((reply, observation));
        }
        let omitted = self.history.len() - kept.len();
        let mut messages = vec![
            ChatMessage::new(jarvis_ai::ChatRole::System, system),
            ChatMessage::user(if omitted > 0 {
                format!("{goal}\n\n({omitted} earlier steps are not shown to save space.)")
            } else {
                goal
            }),
        ];
        for (reply, observation) in kept.into_iter().rev() {
            messages.push(ChatMessage::new(
                jarvis_ai::ChatRole::Assistant,
                reply.clone(),
            ));
            messages.push(ChatMessage::user(observation.clone()));
        }
        messages
    }
}

/// Aborts a helper task when the scope that started it ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// History for a resumed task, from its journal.
fn rebuild_history(steps: &[TaskStep]) -> Vec<(String, String)> {
    let mut history = Vec::new();
    for step in steps {
        match step.kind {
            StepKind::Action => {
                let input: serde_json::Value = step
                    .input
                    .as_deref()
                    .and_then(|i| serde_json::from_str(i).ok())
                    .unwrap_or_default();
                let reply =
                    serde_json::json!({"thought": step.thought, "tool": step.tool, "input": input})
                        .to_string();
                let status = match step.outcome {
                    StepOutcome::Succeeded => "ok",
                    StepOutcome::Denied => "denied",
                    _ => "error",
                };
                history.push((
                    reply,
                    protocol::observation(
                        step.tool.as_deref().unwrap_or_default(),
                        status,
                        step.output.as_deref().unwrap_or_default(),
                    ),
                ));
            }
            StepKind::Verification if step.outcome == StepOutcome::Failed => {
                if let Some((reply, _)) = history.pop() {
                    history.push((
                        reply,
                        format!(
                            "A check of your answer found a problem: {}\nContinue working on the goal.",
                            step.output.as_deref().unwrap_or_default()
                        ),
                    ));
                }
            }
            StepKind::Answer => {
                let reply =
                    serde_json::json!({"thought": step.thought, "final": step.output}).to_string();
                history.push((reply, "Continue.".to_owned()));
            }
            _ => {}
        }
    }
    history
}
