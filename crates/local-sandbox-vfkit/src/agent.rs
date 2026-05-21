//! Guest-agent seam for command, file, and snapshot operations.

use base64::{engine::general_purpose::STANDARD, Engine};
use local_sandbox_core::{
    CommandId, CommandInfo, CommandOutput, CommandStatus, CommandWait, CreateCommandRequest,
    CreateSnapshotRequest, FileWrite, SandboxError, SandboxId, SandboxResult, SnapshotId,
    SnapshotInfo, SnapshotStatus, WriteFilesRequest, WriteFilesResponse,
};
use local_sandbox_protocol::{
    read_frame, write_frame, AgentErrorCode, AgentFileEntry, AgentRequest, AgentResponse,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{mpsc, Mutex, OnceLock},
    time::Duration,
};
use tokio::net::UnixStream;
use uuid::Uuid;

const MAX_FILES_PER_WRITE: usize = 4096;
const AGENT_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Fakeable guest-agent boundary.
pub(crate) trait SandboxAgent: Send + Sync {
    fn register(&self, sandbox_id: SandboxId) -> SandboxResult<()>;

    fn register_protocol_client(
        &self,
        sandbox_id: SandboxId,
        socket_path: PathBuf,
    ) -> SandboxResult<()> {
        let _ = (sandbox_id, socket_path);
        self.register(sandbox_id)
    }

    fn unregister(&self, sandbox_id: SandboxId) -> SandboxResult<()>;

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo>;

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        wait: CommandWait,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo>;

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo>;

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> SandboxResult<WriteFilesResponse>;

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<SnapshotInfo>;
}

/// Guest-agent client that speaks the local sandbox protocol over Unix sockets.
#[derive(Default)]
pub(crate) struct ProtocolAgent {
    clients: Mutex<BTreeMap<SandboxId, ProtocolClient>>,
    commands: Mutex<BTreeMap<(SandboxId, CommandId), ProtocolCommandMetadata>>,
}

#[derive(Clone)]
struct ProtocolClient {
    socket_path: PathBuf,
}

#[derive(Clone)]
struct ProtocolCommandMetadata {
    argv: Vec<String>,
    cwd: Option<String>,
    started_at_unix_ms: Option<u64>,
    finished_at_unix_ms: Option<u64>,
}

/// Default guest-agent implementation used by the vfkit provider.
#[derive(Default)]
pub(crate) struct DefaultAgent {
    in_memory: InMemoryAgent,
    protocol: ProtocolAgent,
}

impl SandboxAgent for DefaultAgent {
    fn register(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        self.in_memory.register(sandbox_id)
    }

    fn register_protocol_client(
        &self,
        sandbox_id: SandboxId,
        socket_path: PathBuf,
    ) -> SandboxResult<()> {
        self.protocol
            .register_protocol_client(sandbox_id, socket_path)
    }

    fn unregister(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        self.in_memory.unregister(sandbox_id)?;
        self.protocol.unregister(sandbox_id)
    }

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        if self.protocol.has_client(sandbox_id)? {
            self.protocol
                .create_command(sandbox_id, request, now_unix_ms)
        } else {
            self.in_memory
                .create_command(sandbox_id, request, now_unix_ms)
        }
    }

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        wait: CommandWait,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        if self.protocol.has_client(sandbox_id)? {
            self.protocol
                .get_command(sandbox_id, command_id, wait, now_unix_ms)
        } else {
            self.in_memory
                .get_command(sandbox_id, command_id, wait, now_unix_ms)
        }
    }

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        if self.protocol.has_client(sandbox_id)? {
            self.protocol
                .cancel_command(sandbox_id, command_id, now_unix_ms)
        } else {
            self.in_memory
                .cancel_command(sandbox_id, command_id, now_unix_ms)
        }
    }

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> SandboxResult<WriteFilesResponse> {
        if self.protocol.has_client(sandbox_id)? {
            self.protocol.write_files(sandbox_id, request)
        } else {
            self.in_memory.write_files(sandbox_id, request)
        }
    }

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<SnapshotInfo> {
        if self.protocol.has_client(sandbox_id)? {
            self.protocol
                .create_snapshot(sandbox_id, request, now_unix_ms)
        } else {
            self.in_memory
                .create_snapshot(sandbox_id, request, now_unix_ms)
        }
    }
}

impl SandboxAgent for ProtocolAgent {
    fn register(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        Err(SandboxError::provider(format!(
            "protocol agent socket path missing for sandbox {sandbox_id}"
        )))
    }

    fn register_protocol_client(
        &self,
        sandbox_id: SandboxId,
        socket_path: PathBuf,
    ) -> SandboxResult<()> {
        wait_until_ready(socket_path.clone())?;
        let mut clients = self.lock_clients()?;
        clients.insert(sandbox_id, ProtocolClient { socket_path });
        Ok(())
    }

    fn unregister(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        let mut clients = self.lock_clients()?;
        clients.remove(&sandbox_id);
        drop(clients);
        self.lock_commands()?
            .retain(|(command_sandbox_id, _), _| *command_sandbox_id != sandbox_id);
        Ok(())
    }

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        if request.argv.is_empty() {
            return Err(SandboxError::invalid_request(
                "command argv must not be empty",
            ));
        }

        let mut argv = request.argv;
        let cmd = argv.remove(0);
        let full_argv = command_argv(cmd.clone(), argv.clone());
        let command_cwd = request.cwd.clone();
        let response = self.send(
            sandbox_id,
            AgentRequest::RunCommand {
                cmd,
                args: argv.clone(),
                cwd: command_cwd.clone(),
                env: non_empty_env(request.env),
                detached: true_option(request.detached),
            },
        )?;

        match response {
            AgentResponse::CommandStarted { cmd_id } => {
                let command_id = command_id_from_agent(&cmd_id)?;
                let command = CommandInfo {
                    id: command_id,
                    sandbox_id,
                    status: CommandStatus::Running,
                    argv: full_argv,
                    cwd: command_cwd,
                    started_at_unix_ms: Some(now_unix_ms),
                    finished_at_unix_ms: None,
                    exit_code: None,
                    output: CommandOutput::default(),
                };
                self.remember_command(&command);
                Ok(command)
            }
            AgentResponse::CommandOutput {
                cmd_id,
                stdout,
                stderr,
                exit_code,
                done,
            } => {
                let command = command_output_info(
                    sandbox_id,
                    command_id_from_agent(&cmd_id)?,
                    full_argv,
                    command_cwd,
                    Some(now_unix_ms),
                    Some(now_unix_ms),
                    stdout,
                    stderr,
                    exit_code,
                    done,
                );
                self.remember_command(&command);
                Ok(command)
            }
            AgentResponse::Error { message, .. } => Err(SandboxError::provider(message)),
            response => Err(unexpected_agent_response(&response)),
        }
    }

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        wait: CommandWait,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        let response = self.send(
            sandbox_id,
            AgentRequest::GetCommand {
                cmd_id: command_id.to_string(),
                wait: true_option(wait.wait),
            },
        )?;

        match response {
            AgentResponse::CommandOutput {
                cmd_id,
                stdout,
                stderr,
                exit_code,
                done,
            } => {
                let command_id = command_id_from_agent(&cmd_id)?;
                let metadata = self.command_metadata(sandbox_id, command_id)?;
                let command = command_output_info(
                    sandbox_id,
                    command_id,
                    metadata.argv,
                    metadata.cwd,
                    metadata.started_at_unix_ms,
                    metadata
                        .finished_at_unix_ms
                        .or_else(|| done.then_some(now_unix_ms)),
                    stdout,
                    stderr,
                    exit_code,
                    done,
                );
                self.remember_command(&command);
                Ok(command)
            }
            AgentResponse::Error {
                code: Some(AgentErrorCode::CommandNotFound),
                ..
            } => Err(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            }),
            AgentResponse::Error {
                code: None,
                message,
            } if message.contains("unknown command") => Err(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            }),
            AgentResponse::Error { message, .. } => Err(SandboxError::provider(message)),
            response => Err(unexpected_agent_response(&response)),
        }
    }

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        let response = self.send(
            sandbox_id,
            AgentRequest::KillCommand {
                cmd_id: command_id.to_string(),
                signal: None,
            },
        )?;

        match response {
            AgentResponse::KillOk => {
                let metadata = self.command_metadata(sandbox_id, command_id)?;
                let command = CommandInfo {
                    id: command_id,
                    sandbox_id,
                    status: CommandStatus::Cancelled,
                    argv: metadata.argv,
                    cwd: metadata.cwd,
                    started_at_unix_ms: metadata.started_at_unix_ms,
                    finished_at_unix_ms: Some(now_unix_ms),
                    exit_code: None,
                    output: CommandOutput::default(),
                };
                self.remember_command(&command);
                Ok(command)
            }
            AgentResponse::Error {
                code: Some(AgentErrorCode::CommandNotFound),
                ..
            } => Err(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            }),
            AgentResponse::Error {
                code: None,
                message,
            } if message.contains("unknown command") => Err(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            }),
            AgentResponse::Error { message, .. } => Err(SandboxError::provider(message)),
            response => Err(unexpected_agent_response(&response)),
        }
    }

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> SandboxResult<WriteFilesResponse> {
        if request.files.len() > MAX_FILES_PER_WRITE {
            return Err(SandboxError::invalid_request(format!(
                "file write request exceeds {MAX_FILES_PER_WRITE} files"
            )));
        }

        let paths = request
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let files = request
            .files
            .into_iter()
            .map(agent_file_entry_from_contract)
            .collect::<SandboxResult<Vec<_>>>()?;
        let response = self.send(sandbox_id, AgentRequest::WriteFiles { files })?;

        match response {
            AgentResponse::WriteFilesOk => Ok(WriteFilesResponse { paths }),
            AgentResponse::Error { message, .. } => Err(SandboxError::provider(message)),
            response => Err(unexpected_agent_response(&response)),
        }
    }

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<SnapshotInfo> {
        let response = self.send(sandbox_id, AgentRequest::Sync)?;

        match response {
            AgentResponse::SyncOk => Ok(SnapshotInfo {
                id: SnapshotId::new(),
                sandbox_id,
                status: SnapshotStatus::Ready,
                created_at_unix_ms: now_unix_ms,
                label: request.label,
            }),
            AgentResponse::Error { message, .. } => Err(SandboxError::provider(message)),
            response => Err(unexpected_agent_response(&response)),
        }
    }
}

impl ProtocolAgent {
    fn send(&self, sandbox_id: SandboxId, request: AgentRequest) -> SandboxResult<AgentResponse> {
        let client = self.client(sandbox_id)?;
        block_on_agent_io(send_agent_request(client.socket_path, request))
    }

    fn client(&self, sandbox_id: SandboxId) -> SandboxResult<ProtocolClient> {
        let clients = self.lock_clients()?;
        clients
            .get(&sandbox_id)
            .cloned()
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })
    }

    fn has_client(&self, sandbox_id: SandboxId) -> SandboxResult<bool> {
        let clients = self.lock_clients()?;
        Ok(clients.contains_key(&sandbox_id))
    }

    fn remember_command(&self, command: &CommandInfo) {
        if let Ok(mut commands) = self.lock_commands() {
            commands.insert(
                (command.sandbox_id, command.id),
                ProtocolCommandMetadata {
                    argv: command.argv.clone(),
                    cwd: command.cwd.clone(),
                    started_at_unix_ms: command.started_at_unix_ms,
                    finished_at_unix_ms: command.finished_at_unix_ms,
                },
            );
        }
    }

    fn command_metadata(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
    ) -> SandboxResult<ProtocolCommandMetadata> {
        let commands = self.lock_commands()?;
        commands
            .get(&(sandbox_id, command_id))
            .cloned()
            .ok_or(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            })
    }

    fn lock_clients(
        &self,
    ) -> SandboxResult<std::sync::MutexGuard<'_, BTreeMap<SandboxId, ProtocolClient>>> {
        self.clients
            .lock()
            .map_err(|_| SandboxError::provider("protocol agent lock poisoned"))
    }

    fn lock_commands(
        &self,
    ) -> SandboxResult<
        std::sync::MutexGuard<'_, BTreeMap<(SandboxId, CommandId), ProtocolCommandMetadata>>,
    > {
        self.commands
            .lock()
            .map_err(|_| SandboxError::provider("protocol command cache lock poisoned"))
    }
}

/// In-memory guest-agent implementation for deterministic provider tests.
#[derive(Default)]
pub(crate) struct InMemoryAgent {
    state: Mutex<AgentState>,
}

#[derive(Default)]
struct AgentState {
    sandboxes: BTreeMap<SandboxId, AgentSandbox>,
}

#[derive(Default)]
struct AgentSandbox {
    commands: BTreeMap<CommandId, CommandInfo>,
    files: BTreeMap<String, FileWrite>,
    snapshots: BTreeMap<SnapshotId, SnapshotInfo>,
}

impl SandboxAgent for InMemoryAgent {
    fn register(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        let mut state = self.lock_state()?;
        state
            .sandboxes
            .entry(sandbox_id)
            .or_insert_with(AgentSandbox::default);
        Ok(())
    }

    fn unregister(&self, sandbox_id: SandboxId) -> SandboxResult<()> {
        let mut state = self.lock_state()?;
        state.sandboxes.remove(&sandbox_id);
        Ok(())
    }

    fn create_command(
        &self,
        sandbox_id: SandboxId,
        request: CreateCommandRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        if request.argv.is_empty() {
            return Err(SandboxError::invalid_request(
                "command argv must not be empty",
            ));
        }

        let mut state = self.lock_state()?;
        let sandbox = state
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        let command_id = CommandId::new();
        let (status, finished_at_unix_ms, exit_code) = if request.detached {
            (CommandStatus::Running, None, None)
        } else {
            (CommandStatus::Exited, Some(now_unix_ms), Some(0))
        };
        let command = CommandInfo {
            id: command_id,
            sandbox_id,
            status,
            argv: request.argv,
            cwd: request.cwd,
            started_at_unix_ms: Some(now_unix_ms),
            finished_at_unix_ms,
            exit_code,
            output: CommandOutput::default(),
        };
        sandbox.commands.insert(command_id, command.clone());
        Ok(command)
    }

    fn get_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        _wait: CommandWait,
        _now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        let state = self.lock_state()?;
        let sandbox = state
            .sandboxes
            .get(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        sandbox
            .commands
            .get(&command_id)
            .cloned()
            .ok_or(SandboxError::CommandNotFound {
                sandbox_id,
                command_id,
            })
    }

    fn cancel_command(
        &self,
        sandbox_id: SandboxId,
        command_id: CommandId,
        now_unix_ms: u64,
    ) -> SandboxResult<CommandInfo> {
        let mut state = self.lock_state()?;
        let sandbox = state
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        let command =
            sandbox
                .commands
                .get_mut(&command_id)
                .ok_or(SandboxError::CommandNotFound {
                    sandbox_id,
                    command_id,
                })?;
        command.status = CommandStatus::Cancelled;
        command.finished_at_unix_ms = Some(now_unix_ms);
        command.exit_code = None;
        Ok(command.clone())
    }

    fn write_files(
        &self,
        sandbox_id: SandboxId,
        request: WriteFilesRequest,
    ) -> SandboxResult<WriteFilesResponse> {
        if request.files.len() > MAX_FILES_PER_WRITE {
            return Err(SandboxError::invalid_request(format!(
                "file write request exceeds {MAX_FILES_PER_WRITE} files"
            )));
        }

        let mut state = self.lock_state()?;
        let sandbox = state
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        let mut paths = Vec::new();
        for file in request.files {
            paths.push(file.path.clone());
            sandbox.files.insert(file.path.clone(), file);
        }
        Ok(WriteFilesResponse { paths })
    }

    fn create_snapshot(
        &self,
        sandbox_id: SandboxId,
        request: CreateSnapshotRequest,
        now_unix_ms: u64,
    ) -> SandboxResult<SnapshotInfo> {
        let mut state = self.lock_state()?;
        let sandbox = state
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or(SandboxError::SandboxNotFound { sandbox_id })?;
        let snapshot_id = SnapshotId::new();
        let snapshot = SnapshotInfo {
            id: snapshot_id,
            sandbox_id,
            status: SnapshotStatus::Ready,
            created_at_unix_ms: now_unix_ms,
            label: request.label,
        };
        sandbox.snapshots.insert(snapshot_id, snapshot.clone());
        Ok(snapshot)
    }
}

impl InMemoryAgent {
    fn lock_state(&self) -> SandboxResult<std::sync::MutexGuard<'_, AgentState>> {
        self.state
            .lock()
            .map_err(|_| SandboxError::provider("in-memory agent lock poisoned"))
    }
}

async fn send_agent_request(
    socket_path: PathBuf,
    request: AgentRequest,
) -> SandboxResult<AgentResponse> {
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|error| {
        SandboxError::provider(format!(
            "failed to connect to guest agent at {}: {error}",
            socket_path.display()
        ))
    })?;
    write_frame(&mut stream, &request).await.map_err(|error| {
        SandboxError::provider(format!("failed to write guest-agent frame: {error}"))
    })?;
    read_frame(&mut stream).await.map_err(|error| {
        SandboxError::provider(format!("failed to read guest-agent frame: {error}"))
    })
}

fn block_on_agent_io<T: Send + 'static>(
    future: impl std::future::Future<Output = SandboxResult<T>> + Send + 'static,
) -> SandboxResult<T> {
    agent_io_worker()?.run(future)
}

struct AgentIoWorker {
    jobs: mpsc::SyncSender<Box<dyn AgentIoJob>>,
}

trait AgentIoJob: Send {
    fn run(self: Box<Self>, runtime: &tokio::runtime::Runtime);
}

struct AgentIoJobImpl<F, T>
where
    F: std::future::Future<Output = SandboxResult<T>> + Send + 'static,
    T: Send + 'static,
{
    future: F,
    result: mpsc::SyncSender<SandboxResult<T>>,
}

impl<F, T> AgentIoJob for AgentIoJobImpl<F, T>
where
    F: std::future::Future<Output = SandboxResult<T>> + Send + 'static,
    T: Send + 'static,
{
    fn run(self: Box<Self>, runtime: &tokio::runtime::Runtime) {
        let result = runtime.block_on(self.future);
        let _ = self.result.send(result);
    }
}

impl AgentIoWorker {
    fn start() -> SandboxResult<Self> {
        let (jobs, receiver) = mpsc::sync_channel::<Box<dyn AgentIoJob>>(64);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|error| {
                SandboxError::provider(format!("failed to build agent runtime: {error}"))
            })?;
        std::thread::Builder::new()
            .name("local-sandbox-agent-io".to_string())
            .spawn(move || {
                for job in receiver {
                    job.run(&runtime);
                }
            })
            .map_err(|error| {
                SandboxError::provider(format!("failed to start agent runtime thread: {error}"))
            })?;
        Ok(Self { jobs })
    }

    fn run<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = SandboxResult<T>> + Send + 'static,
    ) -> SandboxResult<T> {
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        self.jobs
            .send(Box::new(AgentIoJobImpl {
                future,
                result: result_sender,
            }))
            .map_err(|_| SandboxError::provider("agent runtime worker stopped"))?;
        result_receiver
            .recv()
            .map_err(|_| SandboxError::provider("agent runtime worker stopped"))?
    }
}

fn agent_io_worker() -> SandboxResult<&'static AgentIoWorker> {
    static WORKER: OnceLock<SandboxResult<AgentIoWorker>> = OnceLock::new();
    WORKER
        .get_or_init(AgentIoWorker::start)
        .as_ref()
        .map_err(Clone::clone)
}

fn command_argv(cmd: String, args: Vec<String>) -> Vec<String> {
    std::iter::once(cmd).chain(args).collect()
}

fn non_empty_env(env: BTreeMap<String, String>) -> Option<BTreeMap<String, String>> {
    if env.is_empty() {
        None
    } else {
        Some(env)
    }
}

fn true_option(value: bool) -> Option<bool> {
    value.then_some(true)
}

#[allow(clippy::too_many_arguments)]
fn command_output_info(
    sandbox_id: SandboxId,
    command_id: CommandId,
    argv: Vec<String>,
    cwd: Option<String>,
    started_at_unix_ms: Option<u64>,
    finished_at_unix_ms: Option<u64>,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    done: bool,
) -> CommandInfo {
    CommandInfo {
        id: command_id,
        sandbox_id,
        status: if done {
            CommandStatus::Exited
        } else {
            CommandStatus::Running
        },
        argv,
        cwd,
        started_at_unix_ms,
        finished_at_unix_ms: finished_at_unix_ms.filter(|_| done),
        exit_code,
        output: CommandOutput {
            stdout_base64: STANDARD.encode(stdout),
            stderr_base64: STANDARD.encode(stderr),
        },
    }
}

fn command_id_from_agent(cmd_id: &str) -> SandboxResult<CommandId> {
    Uuid::parse_str(cmd_id)
        .map(CommandId::from)
        .map_err(|error| SandboxError::provider(format!("invalid guest-agent command id: {error}")))
}

fn agent_file_entry_from_contract(file: FileWrite) -> SandboxResult<AgentFileEntry> {
    Ok(AgentFileEntry {
        path: file.path,
        content: STANDARD.decode(file.contents_base64).map_err(|error| {
            SandboxError::invalid_request(format!("invalid file contents_base64: {error}"))
        })?,
        mode: file.mode,
    })
}

fn wait_until_ready(socket_path: PathBuf) -> SandboxResult<()> {
    block_on_agent_io(async move {
        let mut last_error = None;
        for _ in 0..600 {
            match send_agent_request(socket_path.clone(), AgentRequest::Ping).await {
                Ok(AgentResponse::Pong) => return Ok(()),
                Ok(response) => {
                    last_error = Some(format!("unexpected readiness response: {response:?}"));
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }
            tokio::time::sleep(AGENT_CONNECT_RETRY_DELAY).await;
        }

        Err(SandboxError::provider(format!(
            "guest agent did not become ready at {}: {}",
            socket_path.display(),
            last_error.unwrap_or_else(|| "timed out".to_string())
        )))
    })
}

fn unexpected_agent_response(response: &AgentResponse) -> SandboxError {
    SandboxError::provider(format!("unexpected guest-agent response: {response:?}"))
}
