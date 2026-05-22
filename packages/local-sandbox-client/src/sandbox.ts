const POLL_INTERVAL_MS = 200;

export type SandboxStatus =
  | "running"
  | "stopped"
  | "aborted"
  | "failed"
  | "snapshotting"
  | "stopping"
  | "pending";

export interface SandboxRoute {
  port: number;
  subdomain?: string;
  url: string;
}

export interface SandboxSource {
  type: "snapshot";
  snapshotId: string;
}

export interface SandboxCreateOptions {
  ports?: number[];
  timeout?: number;
  runtime?: string;
  source?: SandboxSource;
  resources?: {
    vcpus?: number;
    memoryMib?: number;
    diskMib?: number;
  };
  __userlandSpawning?: boolean;
  signal?: AbortSignal;
}

export interface RunCommandOptions {
  cmd: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  signal?: AbortSignal;
  detached?: boolean;
}

export interface VMFile {
  path: string;
  content: BufferSource | string;
  mode?: number;
}

export interface CommandLog {
  stream: "stdout" | "stderr";
  data: string;
}

export type CommandOutputStream = "stdout" | "stderr" | "both";

export interface CommandWaitResult {
  exitCode: number | null;
}

export interface SnapshotResult {
  snapshotId: string;
  sourceSandboxId?: string;
  status?: "created" | "deleted" | "failed";
  sizeBytes?: number;
  createdAt?: Date;
  expiresAt?: Date | null;
}

export interface NetworkPolicy {
  allow?: Record<string, unknown[]>;
}

export interface LocalSandboxClientOptions {
  daemonUrl?: string;
  fetch?: typeof fetch;
}

export interface LocalProfile {
  daemon: LocalDaemonProfile;
  sandbox: LocalSandboxProfile;
  routes: LocalRouteProfile[];
  services: Record<string, LocalServiceProfile>;
}

export interface LocalDaemonProfile {
  health: LocalHealth;
  endpoints: LocalEndpoint[];
}

export interface LocalSandboxProfile {
  provider: string;
  health: LocalHealth;
  endpoints: LocalEndpoint[];
}

export interface LocalRouteProfile {
  name: string;
  target_port?: number | null;
  health: LocalHealth;
  endpoints: LocalEndpoint[];
}

export interface LocalServiceProfile {
  health: LocalHealth;
  endpoints: LocalEndpoint[];
  credentials?: LocalCredentials | null;
}

export type LocalEndpointVisibility = "host" | "guest" | "browser" | "public";

export type LocalEndpointPurpose =
  | "daemon_api"
  | "sandbox_route"
  | "websocket"
  | "callback"
  | "object_storage_api"
  | "public_object_storage"
  | "database";

export type LocalEndpointProtocol =
  | "http"
  | "https"
  | "ws"
  | "wss"
  | "tcp"
  | "postgres";

export interface LocalEndpoint {
  name: string;
  visibility: LocalEndpointVisibility;
  purpose: LocalEndpointPurpose;
  protocol: LocalEndpointProtocol;
  url?: string | null;
  health: LocalHealth;
}

export interface LocalHealth {
  status: LocalHealthStatus;
  message?: string | null;
  checked_at_unix_ms?: number | null;
}

export type LocalHealthStatus =
  | "unknown"
  | "starting"
  | "ready"
  | "degraded"
  | "stopped"
  | "failed";

export interface LocalCredentials {
  access_key_id?: string | null;
  secret_access_key?: string | null;
  username?: string | null;
  password?: string | null;
  token?: string | null;
}

interface DaemonSandboxInfo {
  id: string;
  status:
    | "created"
    | "starting"
    | "running"
    | "stopping"
    | "stopped"
    | "failed";
  architecture: "arm64" | "x86_64";
  routes: Array<{
    name: string;
    protocol: "http" | "https" | "tcp";
    target_port: number;
    url: string;
  }>;
  created_at_unix_ms: number;
  expires_at_unix_ms?: number | null;
}

interface DaemonCommandInfo {
  id: string;
  sandbox_id: string;
  status: "queued" | "running" | "exited" | "failed" | "cancelled";
  argv: string[];
  cwd?: string | null;
  started_at_unix_ms?: number | null;
  finished_at_unix_ms?: number | null;
  exit_code?: number | null;
  output: {
    stdout_base64: string;
    stderr_base64: string;
  };
}

interface DaemonSnapshotInfo {
  id: string;
  sandbox_id: string;
  status: "created" | "creating" | "ready" | "failed";
  created_at_unix_ms: number;
  label?: string | null;
}

interface DaemonErrorBody {
  code?: string;
  message?: string;
}

interface DaemonRequestInit {
  method?: string;
  body?: unknown;
  signal?: AbortSignal | undefined;
}

let defaultClient: LocalSandboxClient | undefined;

export function configureLocalSandbox(
  options: LocalSandboxClientOptions,
): void {
  defaultClient = new LocalSandboxClient(options);
}

export class APIError extends Error {
  readonly status: number;
  readonly json?: DaemonErrorBody;
  readonly text?: string;

  static async fromResponse(response: Response): Promise<APIError> {
    const text = await response.text();
    let json: DaemonErrorBody | undefined;
    try {
      json = text ? (JSON.parse(text) as DaemonErrorBody) : undefined;
    } catch {
      // Keep raw text below.
    }
    const message =
      json?.message ?? (text || `Status code ${response.status} is not ok`);
    return new APIError(
      message,
      dropUndefined({ status: response.status, json, text }) as {
        status: number;
        json?: DaemonErrorBody;
        text?: string;
      },
    );
  }

  constructor(
    message: string,
    options: { status: number; json?: DaemonErrorBody; text?: string },
  ) {
    super(message);
    this.name = "APIError";
    this.status = options.status;
    if (options.json !== undefined) this.json = options.json;
    if (options.text !== undefined) this.text = options.text;
  }
}

export class LocalSandboxClient {
  readonly daemonUrl: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: LocalSandboxClientOptions = {}) {
    const daemonUrl = options.daemonUrl ?? process.env.LOCAL_SANDBOX_DAEMON_URL;
    if (!daemonUrl) {
      throw new Error(
        "local-sandbox daemon URL is required; pass daemonUrl or set LOCAL_SANDBOX_DAEMON_URL",
      );
    }
    this.daemonUrl = trimTrailingSlash(daemonUrl);
    this.fetchImpl = options.fetch ?? fetch;
  }

  async create(options: SandboxCreateOptions = {}): Promise<Sandbox> {
    throwIfAborted(options.signal);
    const body = {
      runtime: options.source ? undefined : options.runtime,
      snapshot_id: options.source?.snapshotId,
      routes: (options.ports ?? []).map((port) => {
        const targetPort = validatePort(port);
        return {
          name: `port-${targetPort}`,
          protocol: "http",
          target_port: targetPort,
        };
      }),
      resources: options.resources
        ? {
            vcpus: options.resources.vcpus,
            memory_mib: options.resources.memoryMib,
            disk_mib: options.resources.diskMib,
          }
        : undefined,
      userland_spawning: options.__userlandSpawning,
      timeout_ms: options.timeout,
    };

    const sandbox = await this.request<DaemonSandboxInfo>(
      "/sandboxes",
      requestOptions({
        method: "POST",
        body,
        signal: options.signal,
      }),
    );
    return new Sandbox(this, sandbox);
  }

  async get(params: {
    sandboxId: string;
    signal?: AbortSignal;
  }): Promise<Sandbox> {
    const sandbox = await this.request<DaemonSandboxInfo>(
      `/sandboxes/${params.sandboxId}`,
      requestOptions({ signal: params.signal }),
    );
    return new Sandbox(this, sandbox);
  }

  async getProfile(params: { signal?: AbortSignal } = {}): Promise<LocalProfile> {
    throwIfAborted(params.signal);
    return this.request<LocalProfile>(
      "/profile",
      requestOptions({ signal: params.signal }),
    );
  }

  async request<T>(path: string, init: DaemonRequestInit = {}): Promise<T> {
    const requestInit: RequestInit = dropUndefined({
      method: init.method ?? "GET",
      headers:
        init.body === undefined
          ? undefined
          : { "content-type": "application/json" },
      body:
        init.body === undefined
          ? undefined
          : JSON.stringify(dropUndefined(init.body)),
      signal: init.signal,
    }) as RequestInit;
    const response = await this.fetchImpl(
      `${this.daemonUrl}${path}`,
      requestInit,
    );

    if (!response.ok) {
      throw await APIError.fromResponse(response);
    }

    if (response.status === 204) {
      return undefined as T;
    }

    return (await response.json()) as T;
  }
}

export class Sandbox {
  static create(options?: SandboxCreateOptions): Promise<Sandbox> {
    return getDefaultClient().create(options);
  }

  static get(params: {
    sandboxId: string;
    signal?: AbortSignal;
  }): Promise<Sandbox> {
    return getDefaultClient().get(params);
  }

  private readonly client: LocalSandboxClient;
  private info: DaemonSandboxInfo;

  constructor(client: LocalSandboxClient, info: DaemonSandboxInfo) {
    this.client = client;
    this.info = info;
  }

  get sandboxId(): string {
    return this.info.id;
  }

  get status(): SandboxStatus {
    switch (this.info.status) {
      case "created":
      case "starting":
        return "pending";
      case "running":
        return "running";
      case "stopping":
        return "stopping";
      case "stopped":
        return "stopped";
      case "failed":
        return "failed";
    }
  }

  get routes(): SandboxRoute[] {
    return this.info.routes.map(
      (route) =>
        dropUndefined({
          port: route.target_port,
          url: route.url,
          subdomain: localSubdomain(route.url),
        }) as SandboxRoute,
    );
  }

  async refresh(): Promise<this> {
    this.info = await this.client.request<DaemonSandboxInfo>(
      `/sandboxes/${this.sandboxId}`,
    );
    return this;
  }

  async runCommand(options: RunCommandOptions): Promise<Command> {
    throwIfAborted(options.signal);
    const command = await this.client.request<DaemonCommandInfo>(
      `/sandboxes/${this.sandboxId}/commands`,
      requestOptions({
        method: "POST",
        body: {
          argv: [options.cmd, ...(options.args ?? [])],
          detached: options.detached,
          cwd: options.cwd,
          env: options.env,
        },
        signal: options.signal,
      }),
    );
    return new Command(this.client, command);
  }

  async getCommand(
    cmdId: string,
    params?: { signal?: AbortSignal },
  ): Promise<Command> {
    throwIfAborted(params?.signal);
    const command = await this.client.request<DaemonCommandInfo>(
      `/sandboxes/${this.sandboxId}/commands/${cmdId}`,
      requestOptions({ signal: params?.signal }),
    );
    return new Command(this.client, command);
  }

  async writeFiles(
    files: VMFile[],
    params?: { signal?: AbortSignal },
  ): Promise<void> {
    throwIfAborted(params?.signal);
    await this.client.request(
      `/sandboxes/${this.sandboxId}/files`,
      requestOptions({
        method: "POST",
        body: {
          files: files.map((file) => ({
            path: file.path,
            contents_base64: encodeContentBase64(file.content),
            mode: file.mode,
          })),
        },
        signal: params?.signal,
      }),
    );
  }

  async extendTimeout(
    timeout: number,
    params?: { signal?: AbortSignal },
  ): Promise<void> {
    throwIfAborted(params?.signal);
    await this.client.request(
      `/sandboxes/${this.sandboxId}/timeout`,
      requestOptions({
        method: "POST",
        body: { additional_ms: timeout },
        signal: params?.signal,
      }),
    );
  }

  async stop(params?: { signal?: AbortSignal }): Promise<void> {
    throwIfAborted(params?.signal);
    this.info = await this.client.request<DaemonSandboxInfo>(
      `/sandboxes/${this.sandboxId}`,
      requestOptions({
        method: "DELETE",
        signal: params?.signal,
      }),
    );
  }

  async updateNetworkPolicy(
    policy: NetworkPolicy,
    params?: { signal?: AbortSignal },
  ): Promise<void> {
    throwIfAborted(params?.signal);
    await this.client.request(
      `/sandboxes/${this.sandboxId}/network-policy`,
      requestOptions({
        method: "PUT",
        body: translateNetworkPolicy(policy),
        signal: params?.signal,
      }),
    );
  }

  async snapshot(params?: { signal?: AbortSignal }): Promise<SnapshotResult> {
    throwIfAborted(params?.signal);
    const snapshot = await this.client.request<DaemonSnapshotInfo>(
      `/sandboxes/${this.sandboxId}/snapshots`,
      requestOptions({ method: "POST", body: {}, signal: params?.signal }),
    );
    return dropUndefined({
      snapshotId: snapshot.id,
      sourceSandboxId: snapshot.sandbox_id,
      status: translateSnapshotStatus(snapshot.status),
      createdAt: new Date(snapshot.created_at_unix_ms),
      expiresAt: null,
    }) as SnapshotResult;
  }
}

export class Command {
  private readonly client: LocalSandboxClient;
  private info: DaemonCommandInfo;
  private decodedOutput:
    | {
        stdoutBase64: string;
        stderrBase64: string;
        stdoutText: string;
        stderrText: string;
      }
    | undefined;

  constructor(client: LocalSandboxClient, info: DaemonCommandInfo) {
    this.client = client;
    this.info = info;
  }

  get cmdId(): string {
    return this.info.id;
  }

  get exitCode(): number | null {
    return this.info.exit_code ?? null;
  }

  async *logs(params?: { signal?: AbortSignal }): AsyncIterable<CommandLog> {
    let stdoutOffset = 0;
    let stderrOffset = 0;

    for (;;) {
      throwIfAborted(params?.signal);
      const { stdoutText: stdout, stderrText: stderr } = this.outputText();
      const nextStdout = stdout.slice(stdoutOffset);
      const nextStderr = stderr.slice(stderrOffset);
      if (nextStdout) yield { stream: "stdout", data: nextStdout };
      if (nextStderr) yield { stream: "stderr", data: nextStderr };
      stdoutOffset = stdout.length;
      stderrOffset = stderr.length;
      if (isTerminalCommandStatus(this.info.status)) return;
      await delay(POLL_INTERVAL_MS);
      await this.refresh(
        requestOptions({ wait: false, signal: params?.signal }),
      );
    }
  }

  async output(stream: CommandOutputStream = "both"): Promise<string> {
    await this.wait();
    const { stdoutText, stderrText } = this.outputText();
    if (stream === "stdout") return stdoutText;
    if (stream === "stderr") return stderrText;
    return stdoutText + stderrText;
  }

  async wait(params?: { signal?: AbortSignal }): Promise<CommandWaitResult> {
    while (!isTerminalCommandStatus(this.info.status)) {
      throwIfAborted(params?.signal);
      await this.refresh(
        requestOptions({ wait: true, signal: params?.signal }),
      );
      if (!isTerminalCommandStatus(this.info.status)) {
        await delay(POLL_INTERVAL_MS);
      }
    }
    return { exitCode: this.exitCode };
  }

  async kill(params?: {
    signal?: AbortSignal;
    abortSignal?: AbortSignal;
  }): Promise<void> {
    const signal = params?.signal ?? params?.abortSignal;
    throwIfAborted(signal);
    this.info = await this.client.request<DaemonCommandInfo>(
      `/sandboxes/${this.info.sandbox_id}/commands/${this.cmdId}`,
      requestOptions({ method: "DELETE", signal }),
    );
  }

  private async refresh(options: {
    wait: boolean;
    signal?: AbortSignal | undefined;
  }): Promise<void> {
    this.info = await this.client.request<DaemonCommandInfo>(
      `/sandboxes/${this.info.sandbox_id}/commands/${this.cmdId}?wait=${String(options.wait)}`,
      requestOptions({ signal: options.signal }),
    );
  }

  private outputText(): { stdoutText: string; stderrText: string } {
    if (
      this.decodedOutput?.stdoutBase64 !== this.info.output.stdout_base64 ||
      this.decodedOutput?.stderrBase64 !== this.info.output.stderr_base64
    ) {
      this.decodedOutput = {
        stdoutBase64: this.info.output.stdout_base64,
        stderrBase64: this.info.output.stderr_base64,
        stdoutText: decodeBase64Text(this.info.output.stdout_base64),
        stderrText: decodeBase64Text(this.info.output.stderr_base64),
      };
    }
    return this.decodedOutput;
  }
}

function translateNetworkPolicy(policy: NetworkPolicy) {
  if (!policy.allow || Object.keys(policy.allow).length === 0) {
    return { mode: "allow_all", allowed_hosts: [] };
  }
  return { mode: "allow_list", allowed_hosts: Object.keys(policy.allow) };
}

function translateSnapshotStatus(
  status: DaemonSnapshotInfo["status"],
): SnapshotResult["status"] {
  switch (status) {
    case "created":
    case "creating":
    case "ready":
      return "created";
    case "failed":
      return "failed";
  }
}

function isTerminalCommandStatus(status: DaemonCommandInfo["status"]): boolean {
  return status === "exited" || status === "failed" || status === "cancelled";
}

function encodeContentBase64(content: BufferSource | string): string {
  if (typeof content === "string")
    return Buffer.from(content, "utf8").toString("base64");
  const bytes =
    content instanceof ArrayBuffer
      ? new Uint8Array(content)
      : new Uint8Array(content.buffer, content.byteOffset, content.byteLength);
  return Buffer.from(bytes).toString("base64");
}

function decodeBase64Text(value: string): string {
  if (!value) return "";
  return Buffer.from(value, "base64").toString("utf8");
}

function localSubdomain(url: string): string | undefined {
  try {
    const parsed = new URL(url);
    return parsed.port ? `${parsed.hostname}:${parsed.port}` : parsed.hostname;
  } catch {
    return undefined;
  }
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function validatePort(port: number): number {
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new Error(`invalid sandbox port: ${port}`);
  }
  return port;
}

function getDefaultClient(): LocalSandboxClient {
  defaultClient ??= new LocalSandboxClient();
  return defaultClient;
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw signal.reason instanceof Error ? signal.reason : new Error("aborted");
  }
}

function dropUndefined(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(dropUndefined);
  if (
    value &&
    typeof value === "object" &&
    Object.getPrototypeOf(value) === Object.prototype
  ) {
    return Object.fromEntries(
      Object.entries(value)
        .filter(([, entry]) => entry !== undefined)
        .map(([key, entry]) => [key, dropUndefined(entry)]),
    );
  }
  return value;
}

function requestOptions<T extends object>(value: T): T {
  return dropUndefined(value) as T;
}
