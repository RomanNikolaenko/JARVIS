//! File tools: everything happens inside the workspace folders.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, FileScope, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncReadExt;

use crate::util::{glob_match, optional_path, parse, text_of, to_args, truncate};
use crate::{PreparedCall, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

/// Directories that searches skip (build output, dependencies, VCS data).
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".venv",
    "venv",
    "__pycache__",
    ".angular",
    ".next",
];
const MAX_SEARCH_DEPTH: usize = 12;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListDirectory::new()),
        Arc::new(ReadFile::new()),
        Arc::new(SearchFiles::new()),
        Arc::new(WriteFile::new()),
        Arc::new(EditFile::new()),
        Arc::new(CopyPath::new()),
        Arc::new(MovePath::new()),
        Arc::new(DeletePath::new()),
    ]
}

fn spec(
    name: &str,
    title: &str,
    description: &str,
    parameters: serde_json::Value,
    capabilities: &[Capability],
    risk: RiskLevel,
    idempotent: bool,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        parameters,
        capabilities: capabilities.to_vec(),
        risk,
        idempotent,
        plugin: None,
    }
}

fn action(spec: &ToolSpec, risk: RiskLevel, summary: String, paths: Vec<PathBuf>) -> ActionRequest {
    ActionRequest {
        tool: spec.name.clone(),
        capabilities: spec.capabilities.clone(),
        risk,
        summary,
        paths,
        program: None,
        url: None,
    }
}

fn args_of<T: serde::de::DeserializeOwned>(call: &PreparedCall) -> Result<T, ToolError> {
    parse(&call.args)
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

async fn read_text(path: &Path, max_bytes: u64) -> Result<(String, bool), ToolError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|err| ToolError::io(format!("opening {}", shown(path)), err))?;
    let size = file
        .metadata()
        .await
        .map_err(|err| ToolError::io("reading metadata", err))?
        .len();
    let mut bytes = Vec::new();
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .await
        .map_err(|err| ToolError::io(format!("reading {}", shown(path)), err))?;
    Ok((text_of(&bytes)?, size > max_bytes))
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug)]
struct ListDirectory(ToolSpec);

impl ListDirectory {
    fn new() -> Self {
        Self(spec(
            "list_directory",
            "List a folder",
            "Lists files and folders in a directory of the workspace. Without `path`, lists the first workspace folder.",
            json!({"type": "object", "properties": {"path": {"type": "string", "description": "Folder, absolute or relative to the workspace"}}}),
            &[Capability::FsRead],
            RiskLevel::Low,
            true,
        ))
    }
}

#[async_trait]
impl Tool for ListDirectory {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: PathArgs = parse(input)?;
        let path = ctx.scope.resolve(optional_path(args.path.as_deref()))?;
        Ok(PreparedCall {
            action: action(
                &self.0,
                RiskLevel::Low,
                format!("List {}", shown(&path)),
                vec![path.clone()],
            ),
            args: to_args(&PathArgs {
                path: Some(shown(&path)),
            })?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: PathArgs = args_of(call)?;
        let path = ctx
            .scope
            .ensure_inside(Path::new(&args.path.unwrap_or_default()))?;
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .map_err(|err| ToolError::io(format!("listing {}", shown(&path)), err))?;
        let mut entries = Vec::new();
        let mut more = false;
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|err| ToolError::io("listing", err))?
        {
            if entries.len() == ctx.limits.max_listed {
                more = true;
                break;
            }
            let meta = entry.metadata().await.ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let size = meta.as_ref().map_or(0, |m| m.len());
            entries.push((
                is_dir,
                entry.file_name().to_string_lossy().into_owned(),
                size,
            ));
        }
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        let mut lines: Vec<String> = entries
            .iter()
            .map(|(is_dir, name, size)| {
                if *is_dir {
                    format!("{name}/")
                } else {
                    format!("{name} ({size} bytes)")
                }
            })
            .collect();
        if more {
            lines.push(format!("… more than {} entries", ctx.limits.max_listed));
        }
        Ok(ToolOutput {
            summary: format!("{} entries in {}", entries.len(), shown(&path)),
            content: truncate(
                &format!("{}:\n{}", ctx.scope.relative(&path), lines.join("\n")),
                ctx.limits.max_output_chars,
            ),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadArgs {
    path: String,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    max_lines: Option<u32>,
}

#[derive(Debug)]
struct ReadFile(ToolSpec);

impl ReadFile {
    fn new() -> Self {
        Self(spec(
            "read_file",
            "Read a file",
            "Reads a text file of the workspace. Optional `startLine` (1-based) and `maxLines` select a part of a long file.",
            json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"},
                "startLine": {"type": "integer", "minimum": 1},
                "maxLines": {"type": "integer", "minimum": 1}
            }}),
            &[Capability::FsRead],
            RiskLevel::Low,
            true,
        ))
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: ReadArgs = parse(input)?;
        let path = ctx.scope.resolve(&args.path)?;
        args.path = shown(&path);
        Ok(PreparedCall {
            action: action(
                &self.0,
                RiskLevel::Low,
                format!("Read {}", shown(&path)),
                vec![path],
            ),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = args_of(call)?;
        let path = ctx.scope.ensure_inside(Path::new(&args.path))?;
        let (text, cut) = read_text(&path, ctx.limits.max_read_bytes).await?;
        let start = args.start_line.unwrap_or(1).max(1) as usize;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let take = args.max_lines.map_or(usize::MAX, |n| n as usize);
        let selected: Vec<&str> = lines.iter().skip(start - 1).take(take).copied().collect();
        let mut content = selected.join("\n");
        if cut {
            content.push_str(&format!(
                "\n… [only the first {} bytes were read]",
                ctx.limits.max_read_bytes
            ));
        }
        Ok(ToolOutput {
            summary: format!(
                "Read {} ({} of {total} lines)",
                shown(&path),
                selected.len()
            ),
            content: truncate(&content, ctx.limits.max_output_chars),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SearchArgs {
    #[serde(default)]
    path: Option<String>,
    /// File name pattern (`*`, `?`).
    #[serde(default)]
    name: Option<String>,
    /// Text to find in files (case-insensitive).
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    max_results: Option<u32>,
}

#[derive(Debug)]
struct SearchFiles(ToolSpec);

impl SearchFiles {
    fn new() -> Self {
        Self(spec(
            "search_files",
            "Search files",
            "Finds files by name pattern (`name`, e.g. `*.rs`) and/or lines containing `text` (case-insensitive) under a workspace folder. Skips .git, node_modules, target and similar folders.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "name": {"type": "string"},
                "text": {"type": "string"},
                "maxResults": {"type": "integer", "minimum": 1, "maximum": 200}
            }}),
            &[Capability::FsRead],
            RiskLevel::Low,
            true,
        ))
    }
}

#[async_trait]
impl Tool for SearchFiles {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: SearchArgs = parse(input)?;
        let empty = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
        if empty(&args.name) && empty(&args.text) {
            return Err(ToolError::InvalidInput("give `name` or `text`".to_owned()));
        }
        let path = ctx.scope.resolve(optional_path(args.path.as_deref()))?;
        args.path = Some(shown(&path));
        let what = args
            .text
            .clone()
            .or_else(|| args.name.clone())
            .unwrap_or_default();
        Ok(PreparedCall {
            action: action(
                &self.0,
                RiskLevel::Low,
                format!("Search \"{what}\" in {}", shown(&path)),
                vec![path],
            ),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SearchArgs = args_of(call)?;
        let root = ctx
            .scope
            .ensure_inside(Path::new(args.path.as_deref().unwrap_or_default()))?;
        let max_results = args.max_results.unwrap_or(50).clamp(1, 200) as usize;
        let max_files = ctx.limits.max_scanned_files;
        let cancel = ctx.cancel.clone();
        let name = args.name.filter(|n| !n.trim().is_empty());
        let text = args
            .text
            .filter(|t| !t.trim().is_empty())
            .map(|t| t.to_lowercase());
        let scan_root = root.clone();
        let scope = ctx.scope.clone();
        let (results, scanned, complete) = tokio::task::spawn_blocking(move || {
            search(
                &scope,
                &scan_root,
                name.as_deref(),
                text.as_deref(),
                max_results,
                max_files,
                &cancel,
            )
        })
        .await
        .map_err(|err| ToolError::Failed(err.to_string()))?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut content = if results.is_empty() {
            "No matches.".to_owned()
        } else {
            results.join("\n")
        };
        if !complete {
            content.push_str(&format!(
                "\n… stopped after {scanned} files or {max_results} results"
            ));
        }
        Ok(ToolOutput {
            summary: format!("{} matches in {}", results.len(), shown(&root)),
            content: truncate(&content, ctx.limits.max_output_chars),
        })
    }
}

/// Walks `root` breadth-first. Returns results, files scanned and whether
/// the walk finished.
fn search(
    scope: &FileScope,
    root: &Path,
    name: Option<&str>,
    text: Option<&str>,
    max_results: usize,
    max_files: usize,
    cancel: &tokio_util::sync::CancellationToken,
) -> (Vec<String>, usize, bool) {
    let mut results = Vec::new();
    let mut scanned = 0;
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if cancel.is_cancelled() || results.len() >= max_results || scanned >= max_files {
                return (results, scanned, false);
            }
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_SEARCH_DEPTH && !SKIP_DIRS.contains(&file_name.as_str()) {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            scanned += 1;
            if name.is_some_and(|pattern| !glob_match(pattern, &file_name)) {
                continue;
            }
            let Some(needle) = text else {
                results.push(scope.relative(&path));
                continue;
            };
            if entry
                .metadata()
                .is_ok_and(|m| m.len() > MAX_SEARCH_FILE_BYTES)
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(content) = text_of(&bytes) else {
                continue;
            };
            for (number, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(needle) {
                    let line: String = line.trim().chars().take(200).collect();
                    results.push(format!("{}:{}: {line}", scope.relative(&path), number + 1));
                    if results.len() >= max_results {
                        return (results, scanned, false);
                    }
                }
            }
        }
    }
    (results, scanned, true)
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug)]
struct WriteFile(ToolSpec);

impl WriteFile {
    fn new() -> Self {
        Self(spec(
            "write_file",
            "Write a file",
            "Creates a text file in the workspace (missing folders are created). An existing file is replaced only with `overwrite: true`.",
            json!({"type": "object", "required": ["path", "content"], "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
                "overwrite": {"type": "boolean"}
            }}),
            &[Capability::FsWrite],
            RiskLevel::Medium,
            false,
        ))
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: WriteArgs = parse(input)?;
        if args.content.len() > ctx.limits.max_write_bytes {
            return Err(ToolError::InvalidInput(format!(
                "content is larger than {} bytes",
                ctx.limits.max_write_bytes
            )));
        }
        let path = ctx.scope.resolve(&args.path)?;
        let exists = path.exists();
        if exists && !args.overwrite {
            return Err(ToolError::InvalidInput(format!(
                "{} exists; set `overwrite: true` to replace it",
                shown(&path)
            )));
        }
        if path.is_dir() {
            return Err(ToolError::InvalidInput(format!(
                "{} is a folder",
                shown(&path)
            )));
        }
        let (risk, verb) = if exists {
            (RiskLevel::High, "Overwrite")
        } else {
            (RiskLevel::Medium, "Create")
        };
        let summary = format!("{verb} {} ({} bytes)", shown(&path), args.content.len());
        args.path = shown(&path);
        Ok(PreparedCall {
            action: action(&self.0, risk, summary, vec![path]),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = args_of(call)?;
        let path = ctx.scope.ensure_inside(Path::new(&args.path))?;
        if let Some(parent) = path.parent() {
            ctx.scope.ensure_inside(parent)?;
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| ToolError::io("creating folders", err))?;
        }
        if args.overwrite {
            tokio::fs::write(&path, args.content.as_bytes())
                .await
                .map_err(|err| ToolError::io(format!("writing {}", shown(&path)), err))?;
        } else {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .map_err(|err| ToolError::io(format!("creating {}", shown(&path)), err))?;
            file.write_all(args.content.as_bytes())
                .await
                .map_err(|err| ToolError::io(format!("writing {}", shown(&path)), err))?;
        }
        Ok(ToolOutput {
            summary: format!("Wrote {} ({} bytes)", shown(&path), args.content.len()),
            content: format!(
                "Wrote {} bytes to {}",
                args.content.len(),
                ctx.scope.relative(&path)
            ),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    find: String,
    replace: String,
    #[serde(default)]
    all: bool,
}

#[derive(Debug)]
struct EditFile(ToolSpec);

impl EditFile {
    fn new() -> Self {
        Self(spec(
            "edit_file",
            "Edit a file",
            "Replaces the exact text `find` with `replace` in a workspace file. `find` must occur exactly once unless `all: true`.",
            json!({"type": "object", "required": ["path", "find", "replace"], "properties": {
                "path": {"type": "string"},
                "find": {"type": "string"},
                "replace": {"type": "string"},
                "all": {"type": "boolean"}
            }}),
            &[Capability::FsRead, Capability::FsWrite],
            RiskLevel::Medium,
            false,
        ))
    }
}

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: EditArgs = parse(input)?;
        if args.find.is_empty() {
            return Err(ToolError::InvalidInput("`find` is empty".to_owned()));
        }
        let path = ctx.scope.resolve(&args.path)?;
        if !path.is_file() {
            return Err(ToolError::InvalidInput(format!(
                "{} is not a file",
                shown(&path)
            )));
        }
        args.path = shown(&path);
        Ok(PreparedCall {
            action: action(
                &self.0,
                RiskLevel::Medium,
                format!("Edit {}", shown(&path)),
                vec![path],
            ),
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = args_of(call)?;
        let path = ctx.scope.ensure_inside(Path::new(&args.path))?;
        let (text, cut) = read_text(&path, ctx.limits.max_read_bytes).await?;
        if cut {
            return Err(ToolError::Failed(
                "the file is too large to edit".to_owned(),
            ));
        }
        let count = text.matches(&args.find).count();
        if count == 0 {
            return Err(ToolError::Failed(
                "the text to replace was not found".to_owned(),
            ));
        }
        if count > 1 && !args.all {
            return Err(ToolError::Failed(format!(
                "the text occurs {count} times; make `find` unique or set `all: true`"
            )));
        }
        let updated = text.replace(&args.find, &args.replace);
        tokio::fs::write(&path, updated.as_bytes())
            .await
            .map_err(|err| ToolError::io(format!("writing {}", shown(&path)), err))?;
        Ok(ToolOutput {
            summary: format!("Edited {} ({count} replacement(s))", shown(&path)),
            content: format!(
                "Replaced {count} occurrence(s) in {}",
                ctx.scope.relative(&path)
            ),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferArgs {
    source: String,
    destination: String,
}

fn prepare_transfer(
    spec: &ToolSpec,
    verb: &str,
    input: &serde_json::Value,
    ctx: &ToolContext,
) -> Result<PreparedCall, ToolError> {
    let args: TransferArgs = parse(input)?;
    let source = ctx.scope.resolve(&args.source)?;
    let destination = ctx.scope.resolve(&args.destination)?;
    if !source.exists() {
        return Err(ToolError::InvalidInput(format!(
            "{} does not exist",
            shown(&source)
        )));
    }
    if destination.exists() {
        return Err(ToolError::InvalidInput(format!(
            "{} already exists",
            shown(&destination)
        )));
    }
    if ctx.scope.roots().contains(&source) {
        return Err(ToolError::InvalidInput(
            "a workspace folder itself cannot be moved".to_owned(),
        ));
    }
    Ok(PreparedCall {
        action: action(
            spec,
            spec.risk,
            format!("{verb} {} to {}", shown(&source), shown(&destination)),
            vec![source.clone(), destination.clone()],
        ),
        args: to_args(&TransferArgs {
            source: shown(&source),
            destination: shown(&destination),
        })?,
    })
}

async fn transfer_paths(
    call: &PreparedCall,
    ctx: &ToolContext,
) -> Result<(PathBuf, PathBuf), ToolError> {
    let args: TransferArgs = args_of(call)?;
    let source = ctx.scope.ensure_inside(Path::new(&args.source))?;
    let destination = ctx.scope.ensure_inside(Path::new(&args.destination))?;
    if tokio::fs::try_exists(&destination).await.unwrap_or(true) {
        return Err(ToolError::Failed(format!(
            "{} already exists",
            shown(&destination)
        )));
    }
    if let Some(parent) = destination.parent() {
        ctx.scope.ensure_inside(parent)?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| ToolError::io("creating folders", err))?;
    }
    Ok((source, destination))
}

#[derive(Debug)]
struct CopyPath(ToolSpec);

impl CopyPath {
    fn new() -> Self {
        Self(spec(
            "copy_path",
            "Copy a file",
            "Copies a file to a new path in the workspace. The destination must not exist.",
            json!({"type": "object", "required": ["source", "destination"], "properties": {
                "source": {"type": "string"}, "destination": {"type": "string"}
            }}),
            &[Capability::FsRead, Capability::FsWrite],
            RiskLevel::Medium,
            false,
        ))
    }
}

#[async_trait]
impl Tool for CopyPath {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        prepare_transfer(&self.0, "Copy", input, ctx)
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (source, destination) = transfer_paths(call, ctx).await?;
        if !source.is_file() {
            return Err(ToolError::Failed("only files can be copied".to_owned()));
        }
        let bytes = tokio::fs::copy(&source, &destination)
            .await
            .map_err(|err| ToolError::io("copying", err))?;
        Ok(ToolOutput {
            summary: format!("Copied {} to {}", shown(&source), shown(&destination)),
            content: format!(
                "Copied {bytes} bytes to {}",
                ctx.scope.relative(&destination)
            ),
        })
    }
}

#[derive(Debug)]
struct MovePath(ToolSpec);

impl MovePath {
    fn new() -> Self {
        Self(spec(
            "move_path",
            "Move or rename",
            "Moves or renames a file or folder inside the workspace. The destination must not exist.",
            json!({"type": "object", "required": ["source", "destination"], "properties": {
                "source": {"type": "string"}, "destination": {"type": "string"}
            }}),
            &[Capability::FsWrite],
            RiskLevel::Medium,
            false,
        ))
    }
}

#[async_trait]
impl Tool for MovePath {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        prepare_transfer(&self.0, "Move", input, ctx)
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (source, destination) = transfer_paths(call, ctx).await?;
        tokio::fs::rename(&source, &destination)
            .await
            .map_err(|err| ToolError::io("moving", err))?;
        Ok(ToolOutput {
            summary: format!("Moved {} to {}", shown(&source), shown(&destination)),
            content: format!("Moved to {}", ctx.scope.relative(&destination)),
        })
    }
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    path: String,
}

#[derive(Debug)]
struct DeletePath(ToolSpec);

impl DeletePath {
    fn new() -> Self {
        Self(spec(
            "delete_path",
            "Delete a file",
            "Deletes a file or an empty folder in the workspace. Always asks the user first.",
            json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
            &[Capability::FsDelete],
            RiskLevel::High,
            false,
        ))
    }
}

#[async_trait]
impl Tool for DeletePath {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: DeleteArgs = parse(input)?;
        let path = ctx.scope.resolve(&args.path)?;
        if ctx.scope.roots().contains(&path) {
            return Err(ToolError::InvalidInput(
                "a workspace folder itself cannot be deleted".to_owned(),
            ));
        }
        if !path.exists() {
            return Err(ToolError::InvalidInput(format!(
                "{} does not exist",
                shown(&path)
            )));
        }
        Ok(PreparedCall {
            action: action(
                &self.0,
                RiskLevel::High,
                format!("Delete {}", shown(&path)),
                vec![path.clone()],
            ),
            args: to_args(&DeleteArgs { path: shown(&path) })?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: DeleteArgs = args_of(call)?;
        let path = ctx.scope.ensure_inside(Path::new(&args.path))?;
        let meta = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|err| ToolError::io(format!("reading {}", shown(&path)), err))?;
        if meta.is_dir() {
            tokio::fs::remove_dir(&path).await.map_err(|err| {
                ToolError::Failed(format!(
                    "cannot delete the folder (it must be empty): {err}"
                ))
            })?;
        } else {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|err| ToolError::io(format!("deleting {}", shown(&path)), err))?;
        }
        Ok(ToolOutput {
            summary: format!("Deleted {}", shown(&path)),
            content: format!("Deleted {}", ctx.scope.relative(&path)),
        })
    }
}
