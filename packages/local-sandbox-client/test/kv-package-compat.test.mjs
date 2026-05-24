import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import net from "node:net";
import os from "node:os";
import { mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { Lock } from "@upstash/lock";
import { Ratelimit } from "@upstash/ratelimit";
import { Redis } from "@upstash/redis";
import { createClient } from "@vercel/kv";
import IORedis from "ioredis";
import { LocalSandboxClient } from "../src/index.ts";

const BRIDGE_TOKEN = "local-sandbox-test-token";
const DAEMON_READY_PATTERN = /local-sandbox daemon listening .*addr=([^\s]+)/;
const ANSI_PATTERN = /\u001b\[[0-9;]*m/g;
const BRIDGE_ERROR_MESSAGES = new Set([
  "method not allowed",
  "unauthorized",
  "invalid request",
  "command failed",
  "unsupported script: generic Lua is unavailable because local-sandbox key_value.capabilities.lua_scripts=false",
]);

test(
  "profile-derived key-value endpoint supports v0-observed Node KV packages",
  { timeout: 90_000, skip: await kvPackageCompatSkipReason() },
  async () => {
    const tempRoot = await mkdtemp(
      path.join(os.tmpdir(), "local-sandbox-kv-packages-"),
    );
    const { daemon, keyValuePort } = await startDaemonWithFreePorts({
      tempRoot,
    });
    const cleanup = async () => {
      await daemon.stop();
      await rm(tempRoot, { recursive: true, force: true });
    };

    try {
      const client = new LocalSandboxClient({ daemonUrl: daemon.url });
      const profile = await client.getProfile();
      const service = profile.services.key_value;
      assert.equal(service?.health.status, "ready");
      assert.equal(service?.key_value?.implementation, "tinyredis");
      assert.equal(service?.key_value?.capabilities.lua_scripts, false);
      assert.equal(
        service?.key_value?.capabilities.targeted_lock_scripts,
        true,
      );

      const redisEndpoint = hostRedisEndpoint(service);
      assert.equal(redisEndpoint, `redis://127.0.0.1:${keyValuePort}`);
      assert.equal(
        service.endpoints.some(
          (endpoint) =>
            endpoint.protocol === "http" || endpoint.protocol === "https",
        ),
        false,
        "profile must not pretend to advertise an Upstash REST endpoint",
      );

      const bridge = await startUpstashRestBridge(redisEndpoint);
      try {
        const upstashRedis = new Redis({
          url: bridge.url,
          token: BRIDGE_TOKEN,
          enableTelemetry: false,
          retry: false,
          responseEncoding: false,
        });
        const vercelKv = createClient({
          url: bridge.url,
          token: BRIDGE_TOKEN,
          enableTelemetry: false,
          retry: false,
          responseEncoding: false,
        });

        const prefix = `kv-package-compat:${Date.now()}:${Math.random()
          .toString(16)
          .slice(2)}`;

        await proveUpstashRedis(upstashRedis, prefix);
        await proveVercelKv(vercelKv, prefix);
        await proveUpstashLock(upstashRedis, prefix);
        await classifyUpstashRatelimit(upstashRedis, prefix, service);
      } finally {
        await bridge.close();
      }
    } finally {
      await cleanup();
    }
  },
);

async function proveUpstashRedis(redis, prefix) {
  assert.equal(await redis.ping(), "PONG");

  await redis.set(key(prefix, "string"), "value");
  assert.equal(await redis.get(key(prefix, "string")), "value");
  assert.deepEqual(
    await redis.mget(key(prefix, "string"), key(prefix, "missing")),
    ["value", null],
  );

  assert.equal(await redis.setex(key(prefix, "setex"), 30, "value"), "OK");
  assert.ok((await redis.ttl(key(prefix, "setex"))) > 0);
  assert.equal(
    await redis.set(key(prefix, "conditional"), "first", { nx: true, ex: 30 }),
    "OK",
  );
  assert.equal(
    await redis.set(key(prefix, "conditional"), "second", { nx: true }),
    null,
  );

  assert.equal(await redis.hset(key(prefix, "hash"), { a: "1", b: "2" }), 2);
  assert.equal(await redis.hget(key(prefix, "hash"), "a"), 1);
  assert.deepEqual(await redis.hgetall(key(prefix, "hash")), { a: 1, b: 2 });

  assert.equal(await redis.rpush(key(prefix, "list"), "a", "b", "c"), 3);
  assert.equal(await redis.ltrim(key(prefix, "list"), 1, -1), "OK");
  assert.deepEqual(await redis.lrange(key(prefix, "list"), 0, -1), ["b", "c"]);

  assert.equal(await redis.sadd(key(prefix, "set"), "a", "b"), 2);
  assert.equal(await redis.sismember(key(prefix, "set"), "a"), 1);
  assert.equal(await redis.srem(key(prefix, "set"), "b"), 1);

  assert.equal(
    await redis.zadd(
      key(prefix, "zset"),
      { score: 1, member: "a" },
      { score: 2, member: "b" },
      { score: 3, member: "c" },
    ),
    3,
  );
  assert.equal(await redis.zremrangebyrank(key(prefix, "zset"), 0, 0), 1);
  assert.deepEqual(await redis.zrange(key(prefix, "zset"), 0, -1), ["b", "c"]);

  const [cursor, keys] = await redis.scan(0, {
    match: `${prefix}:*`,
    count: 100,
  });
  assert.equal(cursor, "0");
  assert.ok(keys.includes(key(prefix, "string")));
  assert.equal(await redis.type(key(prefix, "string")), "string");

  const pipeline = redis.pipeline();
  pipeline.set(key(prefix, "pipeline"), "value");
  pipeline.get(key(prefix, "pipeline"));
  pipeline.rpush(key(prefix, "pipeline-list"), "a", "b");
  assert.deepEqual(await pipeline.exec(), ["OK", "value", 2]);
}

async function proveVercelKv(kv, prefix) {
  assert.equal(await kv.set(key(prefix, "vercel-string"), "value"), "OK");
  assert.equal(await kv.get(key(prefix, "vercel-string")), "value");

  const pipeline = kv.pipeline();
  pipeline.set(key(prefix, "vercel-pipeline"), "value");
  pipeline.get(key(prefix, "vercel-pipeline"));
  pipeline.zadd(
    key(prefix, "vercel-zset"),
    { score: 1, member: "a" },
    { score: 2, member: "b" },
  );
  pipeline.zremrangebyrank(key(prefix, "vercel-zset"), 0, 0);
  assert.deepEqual(await pipeline.exec(), ["OK", "value", 2, 1]);

  const iterated = [];
  for await (const scannedKey of kv.scanIterator({
    match: `${prefix}:vercel-*`,
    count: 100,
  })) {
    iterated.push(scannedKey);
  }
  assert.ok(iterated.includes(key(prefix, "vercel-string")));
}

async function proveUpstashLock(redis, prefix) {
  const lockId = key(prefix, "lock");
  const lock = new Lock({
    redis,
    id: lockId,
    lease: 30_000,
    retry: { attempts: 1, delay: 0 },
  });
  const competing = new Lock({
    redis,
    id: lockId,
    lease: 30_000,
    retry: { attempts: 1, delay: 0 },
  });

  assert.equal(await lock.acquire({ uuid: "owner-1" }), true);
  assert.equal(await competing.acquire({ uuid: "owner-2" }), false);
  assert.equal(await lock.extend(10_000), true);
  assert.equal(await lock.release(), true);
  assert.equal(await competing.acquire({ uuid: "owner-2" }), true);
  assert.equal(await competing.release(), true);
}

async function classifyUpstashRatelimit(redis, prefix, service) {
  assert.equal(service.key_value.capabilities.lua_scripts, false);
  const ratelimit = new Ratelimit({
    redis,
    prefix: key(prefix, "ratelimit"),
    limiter: Ratelimit.fixedWindow(1, "60 s"),
    analytics: false,
    ephemeralCache: false,
    timeout: 0,
  });

  await assert.rejects(
    () => ratelimit.limit("user-1"),
    (error) =>
      error instanceof Error &&
      error.message.includes("evalsha") &&
      error.message.includes("command failed"),
    "@upstash/ratelimit fixedWindow should be explicitly unsupported while generic Lua is not advertised",
  );
}

async function startDaemonWithFreePorts({ tempRoot }) {
  let lastError;
  for (let attempt = 0; attempt < 10; attempt += 1) {
    const keyValuePort = await freePort();
    const objectStoragePort = await freePort();
    try {
      const daemon = await startDaemon({
        tempRoot,
        keyValuePort,
        objectStoragePort,
      });
      return { daemon, keyValuePort };
    } catch (error) {
      lastError = error;
      if (!isAddressInUseError(error)) throw error;
    }
  }
  throw new Error(
    `daemon did not get free ports after retries: ${lastError?.message ?? "unknown error"}`,
  );
}

function isAddressInUseError(error) {
  return (
    error instanceof Error &&
    /address already in use|addrinuse|os error 48|os error 98/i.test(
      error.message,
    )
  );
}

async function startDaemon({ tempRoot, keyValuePort, objectStoragePort }) {
  const daemonBinary = await daemonBinaryPath();
  const child = spawn(daemonBinary, [], {
    cwd: path.resolve(import.meta.dirname, "../../.."),
    env: {
      ...process.env,
      RUST_LOG: "local_sandbox_daemon=info",
      LOCAL_SANDBOX_OBJECT_STORAGE_DATA_DIR: path.join(
        tempRoot,
        "object-storage",
      ),
      LOCAL_SANDBOX_OBJECT_STORAGE_API_PORT: String(objectStoragePort),
      LOCAL_SANDBOX_KEY_VALUE_DATA_DIR: path.join(tempRoot, "key-value"),
      LOCAL_SANDBOX_KEY_VALUE_PORT: String(keyValuePort),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stopping = false;
  let output = "";
  let settled = false;

  const ready = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      if (!settled) {
        settled = true;
        reject(new Error(`daemon did not report readiness:\n${output}`));
      }
    }, 60_000);

    const onData = (chunk) => {
      output += stripAnsi(String(chunk));
      const match = output.match(DAEMON_READY_PATTERN);
      if (match && !settled) {
        settled = true;
        clearTimeout(timeout);
        resolve(match[1]);
      }
    };

    child.stdout.on("data", onData);
    child.stderr.on("data", onData);
    child.on("error", (error) => {
      if (!settled) {
        settled = true;
        clearTimeout(timeout);
        reject(error);
      }
    });
    child.on("exit", (code, signal) => {
      if (!settled) {
        settled = true;
        clearTimeout(timeout);
        reject(
          new Error(
            `daemon exited before readiness: code=${code} signal=${signal}\n${output}`,
          ),
        );
      }
    });
  });

  let url;
  try {
    const addr = await ready;
    url = `http://${addr}`;
    await waitForProfile(url);
  } catch (error) {
    await stopChild(
      child,
      () => output,
      () => stopping,
      (value) => {
        stopping = value;
      },
    );
    throw error;
  }

  return {
    url,
    async stop() {
      await stopChild(
        child,
        () => output,
        () => stopping,
        (value) => {
          stopping = value;
        },
      );
    },
  };
}

async function stopChild(child, output, isStopping, setStopping) {
  if (child.exitCode !== null || isStopping()) return;
  setStopping(true);
  child.kill("SIGINT");
  let code;
  let signal;
  try {
    [code, signal] = await onceWithTimeout(child, "exit", 10_000);
  } catch {
    child.kill("SIGKILL");
    [code, signal] = await onceWithTimeout(child, "exit", 10_000);
  }
  assert.equal(
    signal,
    null,
    `daemon should exit cleanly, code=${code} output=${output()}`,
  );
  assert.equal(code, 0, `daemon should exit cleanly, output=${output()}`);
}

async function onceWithTimeout(emitter, event, timeoutMs) {
  let timeout;
  try {
    return await Promise.race([
      once(emitter, event),
      new Promise((_, reject) => {
        timeout = setTimeout(
          () => reject(new Error(`${event} timed out`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

let cachedDaemonBinary;
async function daemonBinaryPath() {
  if (process.env.LOCAL_SANDBOX_DAEMON_BINARY) {
    return process.env.LOCAL_SANDBOX_DAEMON_BINARY;
  }
  if (cachedDaemonBinary) return cachedDaemonBinary;
  await buildDaemonBinary();
  cachedDaemonBinary = path.resolve(
    import.meta.dirname,
    "../../../target/debug/local-sandbox-daemon",
  );
  return cachedDaemonBinary;
}

let daemonBuilt = false;
async function buildDaemonBinary() {
  if (daemonBuilt) return;
  await new Promise((resolve, reject) => {
    const child = spawn("cargo", ["build", "-p", "local-sandbox-daemon"], {
      cwd: path.resolve(import.meta.dirname, "../../.."),
      stdio: "inherit",
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        daemonBuilt = true;
        resolve();
      } else {
        reject(
          new Error(`cargo build -p local-sandbox-daemon exited with ${code}`),
        );
      }
    });
  });
}

async function kvPackageCompatSkipReason() {
  if (process.env.LOCAL_SANDBOX_RUN_KV_PACKAGE_COMPAT !== "1") {
    return "set LOCAL_SANDBOX_RUN_KV_PACKAGE_COMPAT=1 to run";
  }
  if (process.env.LOCAL_SANDBOX_DAEMON_BINARY) return false;
  if (await commandExists("cargo")) return false;
  return "requires cargo or LOCAL_SANDBOX_DAEMON_BINARY";
}

async function commandExists(command) {
  return new Promise((resolve) => {
    const child = spawn(command, ["--version"], { stdio: "ignore" });
    child.on("error", () => resolve(false));
    child.on("exit", (code) => resolve(code === 0));
  });
}

async function waitForProfile(daemonUrl) {
  const client = new LocalSandboxClient({ daemonUrl });
  const started = Date.now();
  while (Date.now() - started < 30_000) {
    try {
      const profile = await client.getProfile();
      if (profile.services.key_value?.health.status === "ready") return;
    } catch {
      // Keep polling until the daemon begins serving `/profile`.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("daemon profile did not become ready");
}

async function startUpstashRestBridge(redisUrl) {
  const redis = new IORedis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 0,
  });
  await redis.connect();

  const server = createServer(async (request, response) => {
    try {
      if (request.method !== "POST") {
        writeBridgeError(response, 405, "method not allowed");
        return;
      }
      if (request.headers.authorization !== `Bearer ${BRIDGE_TOKEN}`) {
        writeBridgeError(response, 401, "unauthorized");
        return;
      }

      const body = await readJson(request);
      const commands = normalizeBridgeCommands(request.url, body);
      const results = [];
      for (const command of commands) {
        results.push(await runBridgeCommand(redis, command));
      }
      const payload = request.url === "/pipeline" ? results : results[0];
      writeJson(response, 200, payload);
    } catch {
      writeBridgeError(response, 400, "invalid request");
    }
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });

  const address = server.address();
  assert.ok(address && typeof address === "object");
  return {
    url: `http://127.0.0.1:${address.port}`,
    async close() {
      await redis.quit();
      await new Promise((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    },
  };
}

function normalizeBridgeCommands(url, body) {
  if (url === "/pipeline") {
    assert.ok(Array.isArray(body), "pipeline body must be an array");
    return body;
  }
  assert.ok(Array.isArray(body), "command body must be an array");
  return [body];
}

async function runBridgeCommand(redis, command) {
  assert.ok(
    Array.isArray(command) && command.length > 0,
    "command must be a non-empty array",
  );
  const name = String(command[0]).toLowerCase();
  if (name === "eval" || name === "evalsha" || name === "script") {
    return runScriptCommand(redis, name, command);
  }

  const args = command
    .slice(1)
    .map((value) => (value == null ? "" : String(value)));
  try {
    const result = await redis.call(name, ...args);
    return { result: encodeBridgeResult(normalizeRedisResult(name, result)) };
  } catch {
    return bridgeCommandError("command failed");
  }
}

async function runScriptCommand(redis, name, command) {
  if (name === "script" && String(command[1]).toLowerCase() === "load") {
    const script = String(command[2]);
    if (isRatelimitScript(script)) {
      return {
        error:
          "unsupported script: generic Lua is unavailable because local-sandbox key_value.capabilities.lua_scripts=false",
      };
    }
    const result = await redis.call("script", "load", script);
    return { result: encodeBridgeResult(result) };
  }

  const script = String(command[1]);
  if (isRatelimitScript(script)) {
    return {
      error:
        "unsupported script: generic Lua is unavailable because local-sandbox key_value.capabilities.lua_scripts=false",
    };
  }
  try {
    const args = command
      .slice(1)
      .map((value) => (value == null ? "" : String(value)));
    const result = await redis.call(name, ...args);
    return { result: encodeBridgeResult(normalizeRedisResult(name, result)) };
  } catch {
    return bridgeCommandError("command failed");
  }
}

function bridgeCommandError(error) {
  assert.ok(BRIDGE_ERROR_MESSAGES.has(error));
  return { error };
}

function normalizeRedisResult(name, result) {
  if (
    name === "hgetall" &&
    result &&
    typeof result === "object" &&
    !Array.isArray(result)
  ) {
    return Object.entries(result).flat();
  }
  return result;
}

function isRatelimitScript(script) {
  return (
    script.includes('redis.call("INCRBY"') &&
    script.includes('redis.call("PEXPIRE"')
  );
}

function encodeBridgeResult(value) {
  if (value === null || value === undefined) return null;
  if (Buffer.isBuffer(value)) return value.toString("utf8");
  if (Array.isArray(value)) return value.map(encodeBridgeResult);
  return value;
}

function writeJson(response, status, body) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}

function writeBridgeError(response, status, error) {
  assert.ok(BRIDGE_ERROR_MESSAGES.has(error));
  writeJson(response, status, { error });
}

async function readJson(request) {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const raw = Buffer.concat(chunks).toString("utf8");
  return raw ? JSON.parse(raw) : null;
}

function hostRedisEndpoint(service) {
  const endpoint = service.endpoints.find(
    (entry) =>
      entry.name === "host_redis" &&
      entry.visibility === "host" &&
      entry.protocol === "redis" &&
      entry.purpose === "key_value",
  );
  assert.ok(endpoint?.url, "profile must expose a host Redis endpoint");
  return endpoint.url;
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address === "object");
  const { port } = address;
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  return port;
}

function key(prefix, suffix) {
  return `${prefix}:${suffix}`;
}

function stripAnsi(value) {
  return value.replace(ANSI_PATTERN, "");
}
