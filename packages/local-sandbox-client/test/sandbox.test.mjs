import assert from "node:assert/strict";
import { test } from "node:test";
import {
  APIError,
  Command,
  LocalSandboxClient,
  Sandbox,
} from "../dist/index.js";

test("Sandbox.create maps production-shaped options to daemon create", async () => {
  const fetch = createMockFetch([
    jsonResponse(
      {
        id: "sandbox-1",
        status: "running",
        architecture: "arm64",
        routes: [
          {
            name: "port-3000",
            protocol: "http",
            target_port: 3000,
            url: "http://127.0.0.1:3000",
          },
        ],
        created_at_unix_ms: 1,
        expires_at_unix_ms: 1001,
      },
      201,
    ),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });

  const sandbox = await client.create({
    ports: [3000],
    timeout: 1000,
    runtime: "node24",
    resources: { vcpus: 4, memoryMib: 4096, diskMib: 32768 },
    __userlandSpawning: false,
  });

  assert.equal(sandbox.sandboxId, "sandbox-1");
  assert.equal(sandbox.status, "running");
  assert.deepEqual(sandbox.routes, [
    { port: 3000, url: "http://127.0.0.1:3000", subdomain: "127.0.0.1:3000" },
  ]);
  assert.equal(fetch.calls[0].url, "http://daemon.test/sandboxes");
  assert.equal(fetch.calls[0].init.method, "POST");
  assert.deepEqual(JSON.parse(fetch.calls[0].init.body), {
    runtime: "node24",
    routes: [{ name: "port-3000", protocol: "http", target_port: 3000 }],
    resources: { vcpus: 4, memory_mib: 4096, disk_mib: 32768 },
    userland_spawning: false,
    timeout_ms: 1000,
  });
});

test("LocalSandboxClient requires an explicit daemon URL", () => {
  const previous = process.env.LOCAL_SANDBOX_DAEMON_URL;
  delete process.env.LOCAL_SANDBOX_DAEMON_URL;
  try {
    assert.throws(
      () => new LocalSandboxClient(),
      /local-sandbox daemon URL is required/,
    );
  } finally {
    if (previous === undefined) {
      delete process.env.LOCAL_SANDBOX_DAEMON_URL;
    } else {
      process.env.LOCAL_SANDBOX_DAEMON_URL = previous;
    }
  }
});

test("Sandbox.create maps snapshot source to snapshot_id", async () => {
  const fetch = createMockFetch([
    jsonResponse(
      {
        id: "sandbox-2",
        status: "running",
        architecture: "arm64",
        routes: [],
        created_at_unix_ms: 1,
      },
      201,
    ),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });

  await client.create({
    source: { type: "snapshot", snapshotId: "snapshot-1" },
    runtime: "node24",
  });

  assert.deepEqual(JSON.parse(fetch.calls[0].init.body), {
    snapshot_id: "snapshot-1",
    routes: [],
  });
});

test("LocalSandboxClient.getProfile reads the daemon profile route", async () => {
  const fetch = createMockFetch([
    jsonResponse({
      daemon: {
        health: { status: "ready", message: null, checked_at_unix_ms: null },
        endpoints: [
          {
            name: "daemon_api",
            visibility: "host",
            purpose: "daemon_api",
            protocol: "http",
            url: null,
            health: {
              status: "ready",
              message: null,
              checked_at_unix_ms: null,
            },
          },
        ],
      },
      sandbox: {
        provider: "in_memory",
        health: { status: "unknown", message: null, checked_at_unix_ms: null },
        endpoints: [],
      },
      routes: [],
      services: {
        object_storage: {
          health: {
            status: "unknown",
            message: "not wired yet",
            checked_at_unix_ms: null,
          },
          endpoints: [
            {
              name: "host_api",
              visibility: "host",
              purpose: "object_storage_api",
              protocol: "http",
              url: "http://127.0.0.1:9000",
              health: {
                status: "unknown",
                message: null,
                checked_at_unix_ms: null,
              },
            },
            {
              name: "guest_api",
              visibility: "guest",
              purpose: "object_storage_api",
              protocol: "http",
              url: "http://host.internal:9000",
              health: {
                status: "unknown",
                message: null,
                checked_at_unix_ms: null,
              },
            },
          ],
          credentials: {
            access_key_id: "local-sandbox",
            secret_access_key: "local-sandbox-secret",
            username: null,
            password: null,
            token: null,
          },
          logs: [
            {
              name: "service",
              path: "/tmp/local-sandbox/minio.log",
              stream: null,
            },
          ],
          object_storage: {
            region: "us-east-1",
            force_path_style: true,
            capabilities: {
              create_bucket: true,
              put_object: true,
              get_object: true,
              head_object: true,
              delete_object: true,
              delete_objects: true,
              list_objects_v2: true,
              copy_object: true,
              presigned_get: true,
              presigned_put: true,
              cors: false,
              browser_upload: false,
              versioning: false,
              list_object_versions: false,
            },
          },
          key_value: null,
        },
        key_value: {
          health: {
            status: "unknown",
            message: "not started",
            checked_at_unix_ms: null,
          },
          endpoints: [
            {
              name: "host_redis",
              visibility: "host",
              purpose: "key_value",
              protocol: "redis",
              url: "redis://127.0.0.1:16379",
              health: {
                status: "unknown",
                message: null,
                checked_at_unix_ms: null,
              },
            },
            {
              name: "guest_redis",
              visibility: "guest",
              purpose: "key_value",
              protocol: "redis",
              url: "redis://host.internal:16379",
              health: {
                status: "unknown",
                message: null,
                checked_at_unix_ms: null,
              },
            },
          ],
          credentials: {
            access_key_id: null,
            secret_access_key: null,
            username: null,
            password: null,
            token: null,
          },
          logs: [],
          object_storage: null,
          key_value: {
            database: 0,
            implementation: "tinyredis",
            capabilities: {
              ping: true,
              strings: true,
              expiry: true,
              hashes: true,
              lists: true,
              sets: true,
              sorted_sets: true,
              scan: true,
              pipeline: true,
              transactions: true,
              targeted_lock_scripts: true,
              lua_scripts: false,
            },
          },
        },
        database: {
          health: {
            status: "unknown",
            message: "not wired yet",
            checked_at_unix_ms: null,
          },
          endpoints: [
            {
              name: "host_postgres",
              visibility: "host",
              purpose: "database",
              protocol: "postgres",
              url: null,
              health: {
                status: "unknown",
                message: null,
                checked_at_unix_ms: null,
              },
            },
          ],
          credentials: null,
          logs: [],
          object_storage: null,
          key_value: null,
        },
      },
    }),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test/",
    fetch,
  });
  const signal = new AbortController().signal;

  const profile = await client.getProfile({ signal });

  assert.equal(fetch.calls[0].url, "http://daemon.test/profile");
  assert.equal(fetch.calls[0].init.method, "GET");
  assert.equal(fetch.calls[0].init.signal, signal);
  assert.equal(profile.daemon.health.status, "ready");
  assert.equal(profile.sandbox.provider, "in_memory");
  assert.equal(profile.services.object_storage.endpoints[0].visibility, "host");
  assert.equal(
    profile.services.object_storage.endpoints[0].url,
    "http://127.0.0.1:9000",
  );
  assert.equal(
    profile.services.object_storage.endpoints[1].visibility,
    "guest",
  );
  assert.equal(
    profile.services.object_storage.endpoints[1].url,
    "http://host.internal:9000",
  );
  assert.equal(profile.services.object_storage.logs[0].name, "service");
  assert.equal(
    profile.services.object_storage.object_storage.region,
    "us-east-1",
  );
  assert.equal(
    profile.services.object_storage.object_storage.force_path_style,
    true,
  );
  assert.equal(
    profile.services.object_storage.object_storage.capabilities.create_bucket,
    true,
  );
  assert.equal(
    profile.services.object_storage.object_storage.capabilities.presigned_put,
    true,
  );
  assert.equal(
    profile.services.object_storage.object_storage.capabilities.cors,
    false,
  );
  assert.equal(
    profile.services.object_storage.object_storage.capabilities.versioning,
    false,
  );
  assert.equal(profile.services.key_value.endpoints[0].protocol, "redis");
  assert.equal(
    profile.services.key_value.endpoints[0].url,
    "redis://127.0.0.1:16379",
  );
  assert.equal(
    profile.services.key_value.key_value.capabilities.targeted_lock_scripts,
    true,
  );
  assert.equal(
    profile.services.key_value.key_value.capabilities.lua_scripts,
    false,
  );
  assert.equal(profile.services.database.endpoints[0].protocol, "postgres");
});

test("Sandbox command/file/timeout/snapshot methods map to daemon routes", async () => {
  const fetch = createMockFetch([
    jsonResponse(
      commandInfo({
        status: "running",
        stdout: "",
        stderr: "",
        exitCode: null,
      }),
      201,
    ),
    jsonResponse(
      commandInfo({
        status: "exited",
        stdout: "hello\n",
        stderr: "",
        exitCode: 0,
      }),
    ),
    jsonResponse({ paths: ["/workspace/index.ts"] }),
    jsonResponse({ expires_at_unix_ms: 5000 }),
    jsonResponse(
      {
        id: "snapshot-1",
        sandbox_id: "sandbox-1",
        status: "ready",
        created_at_unix_ms: 1234,
      },
      201,
    ),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });
  const sandbox = new Sandbox(client, {
    id: "sandbox-1",
    status: "running",
    architecture: "arm64",
    routes: [],
    created_at_unix_ms: 1,
  });

  const command = await sandbox.runCommand({
    cmd: "echo",
    args: ["hello"],
    cwd: "/workspace",
    env: { A: "B" },
    detached: true,
  });
  assert.ok(command instanceof Command);
  const waitResult = await command.wait();
  assert.deepEqual(waitResult, { exitCode: 0 });
  assert.equal(await command.output("stdout"), "hello\n");

  await sandbox.writeFiles([
    { path: "/workspace/index.ts", content: "console.log(1)", mode: 0o644 },
  ]);
  await sandbox.extendTimeout(4000);
  const snapshot = await sandbox.snapshot();

  assert.equal(snapshot.snapshotId, "snapshot-1");
  assert.equal(snapshot.sourceSandboxId, "sandbox-1");
  assert.equal(snapshot.status, "created");
  assert.equal(
    fetch.calls[0].url,
    "http://daemon.test/sandboxes/sandbox-1/commands",
  );
  assert.deepEqual(JSON.parse(fetch.calls[0].init.body), {
    argv: ["echo", "hello"],
    detached: true,
    cwd: "/workspace",
    env: { A: "B" },
  });
  assert.equal(
    fetch.calls[1].url,
    "http://daemon.test/sandboxes/sandbox-1/commands/cmd-1?wait=true",
  );
  assert.deepEqual(JSON.parse(fetch.calls[2].init.body), {
    files: [
      {
        path: "/workspace/index.ts",
        contents_base64: "Y29uc29sZS5sb2coMSk=",
        mode: 420,
      },
    ],
  });
  assert.equal(
    fetch.calls[3].url,
    "http://daemon.test/sandboxes/sandbox-1/timeout",
  );
  assert.deepEqual(JSON.parse(fetch.calls[3].init.body), {
    additional_ms: 4000,
  });
  assert.equal(
    fetch.calls[4].url,
    "http://daemon.test/sandboxes/sandbox-1/snapshots",
  );
});

test("request signals are forwarded to fetch", async () => {
  const fetch = createMockFetch([
    jsonResponse({
      id: "sandbox-1",
      status: "running",
      architecture: "arm64",
      routes: [],
      created_at_unix_ms: 1,
    }),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });
  const signal = new AbortController().signal;

  await client.create({ signal });

  assert.equal(fetch.calls[0].init.signal, signal);
});

test("writeFiles encodes string content as UTF-8 before base64", async () => {
  const fetch = createMockFetch([
    jsonResponse({ paths: ["/workspace/unicode.txt"] }),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });
  const sandbox = new Sandbox(client, {
    id: "sandbox-1",
    status: "running",
    architecture: "arm64",
    routes: [],
    created_at_unix_ms: 1,
  });

  await sandbox.writeFiles([
    { path: "/workspace/unicode.txt", content: "hello 👋" },
  ]);

  assert.deepEqual(JSON.parse(fetch.calls[0].init.body), {
    files: [
      {
        path: "/workspace/unicode.txt",
        contents_base64: Buffer.from("hello 👋", "utf8").toString("base64"),
      },
    ],
  });
});

test("Command.logs yields stdout and stderr deltas while polling", async () => {
  const fetch = createMockFetch([
    jsonResponse(
      commandInfo({
        status: "running",
        stdout: "a",
        stderr: "",
        exitCode: null,
      }),
    ),
    jsonResponse(
      commandInfo({ status: "exited", stdout: "ab", stderr: "e", exitCode: 0 }),
    ),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });
  const command = new Command(
    client,
    commandInfo({ status: "running", stdout: "a", stderr: "", exitCode: null }),
  );

  const logs = [];
  for await (const chunk of command.logs()) {
    logs.push(chunk);
  }

  assert.deepEqual(logs, [
    { stream: "stdout", data: "a" },
    { stream: "stdout", data: "b" },
    { stream: "stderr", data: "e" },
  ]);
  assert.equal(
    fetch.calls[0].url,
    "http://daemon.test/sandboxes/sandbox-1/commands/cmd-1?wait=false",
  );
});

test("APIError preserves daemon error body", async () => {
  const fetch = createMockFetch([
    jsonResponse(
      { code: "unsupported", message: "unsupported operation: network_policy" },
      501,
    ),
  ]);
  const client = new LocalSandboxClient({
    daemonUrl: "http://daemon.test",
    fetch,
  });
  const sandbox = new Sandbox(client, {
    id: "sandbox-1",
    status: "running",
    architecture: "arm64",
    routes: [],
    created_at_unix_ms: 1,
  });

  await assert.rejects(
    sandbox.updateNetworkPolicy({ allow: { "*": [] } }),
    (error) => {
      assert.ok(error instanceof APIError);
      assert.equal(error.status, 501);
      assert.deepEqual(error.json, {
        code: "unsupported",
        message: "unsupported operation: network_policy",
      });
      return true;
    },
  );
});

function commandInfo({ status, stdout, stderr, exitCode }) {
  return {
    id: "cmd-1",
    sandbox_id: "sandbox-1",
    status,
    argv: ["echo", "hello"],
    cwd: "/workspace",
    started_at_unix_ms: 1,
    finished_at_unix_ms: status === "running" ? null : 2,
    exit_code: exitCode,
    output: {
      stdout_base64: base64(stdout),
      stderr_base64: base64(stderr),
    },
  };
}

function createMockFetch(responses) {
  const calls = [];
  const fetch = async (url, init = {}) => {
    calls.push({ url, init });
    const response = responses.shift();
    if (!response) throw new Error(`unexpected fetch: ${url}`);
    return response;
  };
  fetch.calls = calls;
  return fetch;
}

function jsonResponse(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function base64(value) {
  return Buffer.from(value, "utf8").toString("base64");
}
