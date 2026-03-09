import { ChildProcess, execSync, spawn } from "child_process";
import * as fs from "fs";
import * as http from "http";
import * as net from "net";
import * as os from "os";
import * as path from "path";
import { fileURLToPath } from "url";
import { engineScenarios } from "./engine-scenarios.js";
import { scenarios, type BenchmarkCommand, type Scenario } from "./scenarios.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const PAGES_DIR = path.join(__dirname, "pages");

const MIME_TYPES: Record<string, string> = {
  ".css": "text/css",
  ".html": "text/html",
  ".jpg": "image/jpeg",
  ".js": "application/javascript",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

function startFileServer(): Promise<{ port: number; server: http.Server }> {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const url = new URL(req.url || "/", "http://localhost");
      let filePath = path.join(PAGES_DIR, url.pathname === "/" ? "article.html" : url.pathname);

      if (!filePath.startsWith(PAGES_DIR)) {
        res.writeHead(403);
        res.end();
        return;
      }

      if (fs.existsSync(filePath) && fs.statSync(filePath).isDirectory()) {
        filePath = path.join(filePath, "index.html");
      }

      try {
        const content = fs.readFileSync(filePath);
        const ext = path.extname(filePath);
        res.writeHead(200, {
          "Content-Type": MIME_TYPES[ext] || "application/octet-stream",
        });
        res.end(content);
      } catch {
        res.writeHead(404);
        res.end("Not found");
      }
    });

    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (!addr || typeof addr === "string") {
        reject(new Error("Failed to get server address"));
        return;
      }
      resolve({ port: addr.port, server });
    });

    server.on("error", reject);
  });
}

function stopFileServer(server: http.Server): Promise<void> {
  return new Promise((resolve) => {
    server.close(() => resolve());
  });
}

function getProcessMemoryKB(pid: number): number | null {
  if (process.platform === "linux") {
    try {
      const status = fs.readFileSync(`/proc/${pid}/status`, "utf-8");
      const match = status.match(/VmRSS:\s+(\d+)\s+kB/);
      if (match) {
        return parseInt(match[1], 10);
      }
    } catch {
      // ignore
    }
  }

  try {
    const output = execSync(`ps -o rss= -p ${pid}`, {
      encoding: "utf-8",
      timeout: 2000,
    });
    const kb = parseInt(output.trim(), 10);
    if (!Number.isNaN(kb)) {
      return kb;
    }
  } catch {
    // ignore
  }

  return null;
}

function sampleMemory(pids: number[], intervalMs: number): { stop: () => number } {
  let peakKB = 0;
  const timer = setInterval(() => {
    for (const pid of pids) {
      const kb = getProcessMemoryKB(pid);
      if (kb && kb > peakKB) {
        peakKB = kb;
      }
    }
  }, intervalMs);

  return {
    stop() {
      clearInterval(timer);
      for (const pid of pids) {
        const kb = getProcessMemoryKB(pid);
        if (kb && kb > peakKB) {
          peakKB = kb;
        }
      }
      return peakKB;
    },
  };
}

function formatMemory(kb: number): string {
  if (kb >= 1024 * 1024) {
    return `${(kb / 1024 / 1024).toFixed(1)}GB`;
  }
  if (kb >= 1024) {
    return `${(kb / 1024).toFixed(1)}MB`;
  }
  return `${kb}KB`;
}

function getSocketDir(): string {
  if (process.env.AGENT_BROWSER_SOCKET_DIR) {
    return process.env.AGENT_BROWSER_SOCKET_DIR;
  }
  if (process.env.XDG_RUNTIME_DIR) {
    return path.join(process.env.XDG_RUNTIME_DIR, "agent-browser");
  }
  const home = os.homedir();
  if (home) {
    return path.join(home, ".agent-browser");
  }
  return path.join(os.tmpdir(), "agent-browser");
}

function getSocketPath(session: string): string {
  return path.join(getSocketDir(), `${session}.sock`);
}

function getProjectRoot(): string {
  return path.resolve(__dirname, "../..");
}

function getNativeBinaryPath(): string {
  const root = getProjectRoot();
  const platform = os.platform();
  const arch = os.arch();

  const osKey =
    platform === "darwin"
      ? "darwin"
      : platform === "linux"
        ? "linux"
        : platform === "win32"
          ? "win32"
          : null;
  const archKey =
    arch === "x64" || arch === "x86_64"
      ? "x64"
      : arch === "arm64" || arch === "aarch64"
        ? "arm64"
        : null;

  if (!osKey || !archKey) {
    throw new Error(`Unsupported platform: ${platform}-${arch}`);
  }

  const ext = platform === "win32" ? ".exe" : "";
  const binName = `agent-browser-${osKey}-${archKey}${ext}`;
  const candidates = [
    path.join(root, "cli/target/release/agent-browser"),
    path.join(root, "cli/target/debug/agent-browser"),
    path.join(root, "bin", binName),
  ];

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  throw new Error(
    `Native binary not found. Tried:\n${candidates.map((candidate) => `  ${candidate}`).join("\n")}\nRun "pnpm build:native" to build the native binary.`,
  );
}

function sendCommand(session: string, cmd: BenchmarkCommand): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const socketPath = getSocketPath(session);
    const client = net.createConnection({ path: socketPath }, () => {
      client.write(JSON.stringify(cmd) + "\n");
    });

    let data = "";
    client.on("data", (chunk) => {
      data += chunk.toString();
      const newlineIdx = data.indexOf("\n");
      if (newlineIdx !== -1) {
        const line = data.slice(0, newlineIdx);
        client.destroy();
        try {
          resolve(JSON.parse(line));
        } catch {
          reject(new Error(`Invalid JSON response: ${line}`));
        }
      }
    });

    client.on("error", (err) => reject(err));
    client.on("timeout", () => {
      client.destroy();
      reject(new Error("Socket timeout"));
    });
    client.setTimeout(30_000);
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForSocket(session: string, timeoutMs = 15_000): Promise<void> {
  const start = Date.now();
  const socketPath = getSocketPath(session);
  while (Date.now() - start < timeoutMs) {
    if (fs.existsSync(socketPath)) {
      try {
        await new Promise<void>((resolve, reject) => {
          const socket = net.createConnection({ path: socketPath }, () => {
            socket.destroy();
            resolve();
          });
          socket.on("error", reject);
          socket.setTimeout(1000);
          socket.on("timeout", () => {
            socket.destroy();
            reject(new Error("timeout"));
          });
        });
        return;
      } catch {
        // not ready yet
      }
    }
    await sleep(100);
  }
  throw new Error(`Daemon '${session}' did not become ready within ${timeoutMs}ms`);
}

interface DaemonHandle {
  process: ChildProcess;
  session: string;
}

function spawnNodeDaemon(session: string): DaemonHandle {
  const daemonPath = path.join(getProjectRoot(), "dist/daemon.js");
  if (!fs.existsSync(daemonPath)) {
    throw new Error(`Node daemon not found at ${daemonPath}. Run "pnpm build" first.`);
  }

  const child = spawn("node", [daemonPath], {
    detached: true,
    env: {
      ...process.env,
      AGENT_BROWSER_DAEMON: "1",
      AGENT_BROWSER_SESSION: session,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });

  child.stderr?.on("data", (chunk) => {
    const msg = chunk.toString().trim();
    if (msg && process.env.BENCH_DEBUG) {
      process.stderr.write(`[node-daemon] ${msg}\n`);
    }
  });

  return { process: child, session };
}

function spawnNativeDaemon(session: string, engine?: string): DaemonHandle {
  const binaryPath = getNativeBinaryPath();
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    AGENT_BROWSER_DAEMON: "1",
    AGENT_BROWSER_SESSION: session,
  };
  if (engine) {
    env.AGENT_BROWSER_ENGINE = engine;
  }

  const child = spawn(binaryPath, [], {
    detached: true,
    env,
    stdio: ["ignore", "ignore", "pipe"],
  });

  const label = engine ? `native-${engine}` : "native-daemon";
  child.stderr?.on("data", (chunk) => {
    const msg = chunk.toString().trim();
    if (msg && process.env.BENCH_DEBUG) {
      process.stderr.write(`[${label}] ${msg}\n`);
    }
  });

  return { process: child, session };
}

async function closeDaemon(handle: DaemonHandle): Promise<void> {
  try {
    await sendCommand(handle.session, { action: "close", id: "close" });
  } catch {
    // daemon may already be gone
  }
  await sleep(200);
  try {
    handle.process.kill("SIGTERM");
  } catch {
    // already exited
  }
}

function cleanupSockets(): void {
  for (const session of ["bench-node", "bench-native", "bench-chrome", "bench-lightpanda"]) {
    const socketPath = getSocketPath(session);
    const pidPath = socketPath.replace(/\.sock$/, ".pid");
    try {
      fs.unlinkSync(socketPath);
    } catch {
      // ignore
    }
    try {
      fs.unlinkSync(pidPath);
    } catch {
      // ignore
    }
  }
}

interface Stats {
  avgUs: number;
  maxUs: number;
  minUs: number;
  p50Us: number;
  p95Us: number;
}

function computeStats(timingsUs: number[]): Stats {
  const sorted = [...timingsUs].sort((a, b) => a - b);
  const sum = sorted.reduce((a, b) => a + b, 0);
  return {
    avgUs: Math.round(sum / sorted.length),
    maxUs: sorted[sorted.length - 1],
    minUs: sorted[0],
    p50Us: sorted[Math.floor(sorted.length * 0.5)],
    p95Us: sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))],
  };
}

function formatDuration(us: number): string {
  if (us >= 1_000_000) {
    return `${(us / 1_000_000).toFixed(2)}s`;
  }
  if (us >= 1_000) {
    return `${(us / 1_000).toFixed(1)}ms`;
  }
  return `${us}us`;
}

async function runCommands(session: string, commands: BenchmarkCommand[]): Promise<void> {
  for (const cmd of commands) {
    const resp = await sendCommand(session, cmd);
    if (!(resp as { success?: boolean }).success) {
      throw new Error(
        `Command '${cmd.action}' failed on session '${session}': ${JSON.stringify(resp)}`,
      );
    }
  }
}

async function timeCommands(session: string, commands: BenchmarkCommand[]): Promise<number> {
  const start = process.hrtime.bigint();
  await runCommands(session, commands);
  const elapsedNs = process.hrtime.bigint() - start;
  return Number(elapsedNs / 1000n);
}

interface ScenarioResult {
  chromeStats: Stats | null;
  lightpandaStats: Stats | null;
  name: string;
  nativeStats: Stats | null;
  nodeStats: Stats | null;
}

async function runScenario(
  scenario: Scenario,
  sessions: { native?: string; node?: string },
  iterations: number,
  warmup: number,
): Promise<ScenarioResult> {
  const result: ScenarioResult = {
    chromeStats: null,
    lightpandaStats: null,
    name: scenario.name,
    nativeStats: null,
    nodeStats: null,
  };

  for (const [label, session] of Object.entries(sessions)) {
    if (!session) {
      continue;
    }

    if (scenario.setup) {
      await runCommands(session, scenario.setup);
    }

    for (let i = 0; i < warmup; i++) {
      await timeCommands(session, scenario.commands);
    }

    const timings: number[] = [];
    for (let i = 0; i < iterations; i++) {
      timings.push(await timeCommands(session, scenario.commands));
    }

    if (scenario.teardown) {
      await runCommands(session, scenario.teardown);
    }

    const stats = computeStats(timings);
    if (label === "node") {
      result.nodeStats = stats;
    } else if (label === "native") {
      result.nativeStats = stats;
    }
  }

  return result;
}

async function runScenarioWithErrorTolerance(
  scenario: Scenario,
  sessions: Record<string, string>,
  iterations: number,
  warmup: number,
): Promise<ScenarioResult> {
  const result: ScenarioResult = {
    chromeStats: null,
    lightpandaStats: null,
    name: scenario.name,
    nativeStats: null,
    nodeStats: null,
  };

  for (const [label, session] of Object.entries(sessions)) {
    if (!session) {
      continue;
    }

    try {
      if (scenario.setup) {
        await runCommands(session, scenario.setup);
      }

      for (let i = 0; i < warmup; i++) {
        await timeCommands(session, scenario.commands);
      }

      const timings: number[] = [];
      for (let i = 0; i < iterations; i++) {
        timings.push(await timeCommands(session, scenario.commands));
      }

      if (scenario.teardown) {
        await runCommands(session, scenario.teardown);
      }

      const stats = computeStats(timings);
      if (label === "chrome") {
        result.chromeStats = stats;
      } else if (label === "lightpanda") {
        result.lightpandaStats = stats;
      } else if (label === "node") {
        result.nodeStats = stats;
      } else if (label === "native") {
        result.nativeStats = stats;
      }
    } catch (error) {
      if (process.env.BENCH_DEBUG) {
        const message = error instanceof Error ? error.message : String(error);
        process.stderr.write(`  [${label}] scenario '${scenario.name}' failed: ${message}\n`);
      }
    }
  }

  return result;
}

function pad(value: string, len: number): string {
  return value.padEnd(len);
}

function rpad(value: string, len: number): string {
  return value.padStart(len);
}

function formatSpeedup(baselineUs: number, candidateUs: number): string {
  if (candidateUs === 0 && baselineUs === 0) {
    return "  --";
  }
  if (candidateUs === 0) {
    return "  >>>";
  }
  const ratio = baselineUs / candidateUs;
  return `${ratio.toFixed(1)}x`;
}

type BenchmarkMode = "daemon" | "engine";

function printResults(
  results: ScenarioResult[],
  iterations: number,
  warmup: number,
  mode: BenchmarkMode = "daemon",
): void {
  console.log("");

  if (mode === "engine") {
    printEngineResults(results, iterations, warmup);
    return;
  }

  const bothPaths = results[0].nodeStats !== null && results[0].nativeStats !== null;
  const header = bothPaths
    ? `agent-browser benchmark: node vs native (${iterations} iterations, ${warmup} warmup)`
    : `agent-browser benchmark (${iterations} iterations, ${warmup} warmup)`;
  console.log(header);
  console.log("=".repeat(header.length));
  console.log("");

  if (bothPaths) {
    const nameW = 20;
    const colW = 14;

    console.log(
      pad("Scenario", nameW) +
        rpad("Node (avg)", colW) +
        rpad("Native (avg)", colW) +
        rpad("Speedup", 10),
    );
    console.log("-".repeat(nameW + colW * 2 + 10));

    let totalNodeUs = 0;
    let totalNativeUs = 0;
    let count = 0;

    for (const result of results) {
      if (!result.nodeStats || !result.nativeStats) {
        continue;
      }
      totalNodeUs += result.nodeStats.avgUs;
      totalNativeUs += result.nativeStats.avgUs;
      count++;

      console.log(
        pad(result.name, nameW) +
          rpad(formatDuration(result.nodeStats.avgUs), colW) +
          rpad(formatDuration(result.nativeStats.avgUs), colW) +
          rpad(formatSpeedup(result.nodeStats.avgUs, result.nativeStats.avgUs), 10),
      );
    }

    console.log("-".repeat(nameW + colW * 2 + 10));

    if (count > 0 && totalNativeUs > 0) {
      const overallSpeedup = totalNodeUs / totalNativeUs;
      const winner = overallSpeedup >= 1.0 ? "native is faster" : "node is faster";
      console.log(`Overall average speedup: ${overallSpeedup.toFixed(1)}x (${winner})`);
      console.log("");

      const allNativeFaster = results.every(
        (result) =>
          !result.nodeStats ||
          !result.nativeStats ||
          result.nodeStats.avgUs >= result.nativeStats.avgUs,
      );
      if (allNativeFaster) {
        console.log("Result: PASS -- native is faster across all scenarios");
      } else {
        const slower = results
          .filter(
            (result) =>
              result.nodeStats &&
              result.nativeStats &&
              result.nodeStats.avgUs < result.nativeStats.avgUs,
          )
          .map((result) => result.name);
        console.log(`Result: WARN -- native is slower in: ${slower.join(", ")}`);
      }
    }
  } else {
    const nameW = 20;
    const label = results[0].nodeStats ? "Node" : "Native";
    console.log(
      pad("Scenario", nameW) +
        rpad(`${label} avg`, 10) +
        rpad("min", 10) +
        rpad("max", 10) +
        rpad("p50", 10) +
        rpad("p95", 10),
    );
    console.log("-".repeat(nameW + 50));
    for (const result of results) {
      const stats = result.nodeStats ?? result.nativeStats;
      if (!stats) {
        continue;
      }
      console.log(
        pad(result.name, nameW) +
          rpad(formatDuration(stats.avgUs), 10) +
          rpad(formatDuration(stats.minUs), 10) +
          rpad(formatDuration(stats.maxUs), 10) +
          rpad(formatDuration(stats.p50Us), 10) +
          rpad(formatDuration(stats.p95Us), 10),
      );
    }
  }

  console.log("");
}

function printEngineResults(
  results: ScenarioResult[],
  iterations: number,
  warmup: number,
): void {
  const header = `agent-browser benchmark: chrome vs lightpanda (${iterations} iterations, ${warmup} warmup)`;
  console.log(header);
  console.log("=".repeat(header.length));
  console.log("");

  const nameW = 22;
  const colW = 18;

  console.log(
    pad("Scenario", nameW) +
      rpad("Chrome (avg)", colW) +
      rpad("Lightpanda (avg)", colW) +
      rpad("Speedup", 10),
  );
  console.log("-".repeat(nameW + colW * 2 + 10));

  let totalChromeUs = 0;
  let totalLightpandaUs = 0;
  let comparableCount = 0;

  for (const result of results) {
    const chromeAvg = result.chromeStats ? formatDuration(result.chromeStats.avgUs) : "N/A";
    const lightpandaAvg = result.lightpandaStats
      ? formatDuration(result.lightpandaStats.avgUs)
      : "N/A";
    let speedup = "  --";

    if (result.chromeStats && result.lightpandaStats) {
      totalChromeUs += result.chromeStats.avgUs;
      totalLightpandaUs += result.lightpandaStats.avgUs;
      comparableCount++;
      speedup = formatSpeedup(result.chromeStats.avgUs, result.lightpandaStats.avgUs);
    }

    console.log(
      pad(result.name, nameW) +
        rpad(chromeAvg, colW) +
        rpad(lightpandaAvg, colW) +
        rpad(speedup, 10),
    );
  }

  console.log("-".repeat(nameW + colW * 2 + 10));

  if (comparableCount > 0 && totalLightpandaUs > 0) {
    const ratio = totalChromeUs / totalLightpandaUs;
    const winner =
      ratio >= 1.0
        ? `lightpanda ${ratio.toFixed(1)}x faster`
        : `chrome ${(1 / ratio).toFixed(1)}x faster`;
    console.log(`Overall: ${winner}`);
  }

  console.log("");
}

function writeJsonResults(
  results: ScenarioResult[],
  outputPath: string,
  mode: BenchmarkMode = "daemon",
): void {
  const toMs = (us: number) => +(us / 1000).toFixed(2);
  const statsToJson = (stats: Stats) => ({
    avg_ms: toMs(stats.avgUs),
    max_ms: toMs(stats.maxUs),
    min_ms: toMs(stats.minUs),
    p50_ms: toMs(stats.p50Us),
    p95_ms: toMs(stats.p95Us),
  });

  const json = results.map((result) => {
    if (mode === "engine") {
      return {
        chrome: result.chromeStats ? statsToJson(result.chromeStats) : null,
        lightpanda: result.lightpandaStats ? statsToJson(result.lightpandaStats) : null,
        scenario: result.name,
        speedup:
          result.chromeStats &&
          result.lightpandaStats &&
          result.lightpandaStats.avgUs > 0
            ? +(result.chromeStats.avgUs / result.lightpandaStats.avgUs).toFixed(2)
            : null,
      };
    }

    return {
      native: result.nativeStats ? statsToJson(result.nativeStats) : null,
      node: result.nodeStats ? statsToJson(result.nodeStats) : null,
      scenario: result.name,
      speedup:
        result.nodeStats && result.nativeStats && result.nativeStats.avgUs > 0
          ? +(result.nodeStats.avgUs / result.nativeStats.avgUs).toFixed(2)
          : null,
    };
  });

  fs.writeFileSync(outputPath, JSON.stringify(json, null, 2) + "\n");
  console.log(`JSON results written to ${outputPath}`);
}

interface CliArgs {
  engineMode: boolean;
  iterations: number;
  json: boolean;
  nativeOnly: boolean;
  nodeOnly: boolean;
  warmup: number;
}

function parseArgs(): CliArgs {
  const args = process.argv.slice(2);
  const result: CliArgs = {
    engineMode: false,
    iterations: 10,
    json: false,
    nativeOnly: false,
    nodeOnly: false,
    warmup: 3,
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--iterations":
        result.iterations = parseInt(args[++i], 10);
        break;
      case "--warmup":
        result.warmup = parseInt(args[++i], 10);
        break;
      case "--node-only":
        result.nodeOnly = true;
        break;
      case "--native-only":
        result.nativeOnly = true;
        break;
      case "--engine":
        result.engineMode = true;
        break;
      case "--json":
        result.json = true;
        break;
      default:
        console.error(`Unknown flag: ${args[i]}`);
        process.exit(1);
    }
  }

  return result;
}

async function runDaemonBenchmark(args: CliArgs): Promise<void> {
  const runNode = !args.nativeOnly;
  const runNative = !args.nodeOnly;

  console.log("Starting benchmark daemons...");

  let nativeHandle: DaemonHandle | undefined;
  let nodeHandle: DaemonHandle | undefined;

  try {
    if (runNode) {
      nodeHandle = spawnNodeDaemon("bench-node");
      await waitForSocket("bench-node");
      console.log("  Node daemon ready");
    }

    if (runNative) {
      nativeHandle = spawnNativeDaemon("bench-native");
      await waitForSocket("bench-native");
      console.log("  Native daemon ready");
    }

    const sessions: { native?: string; node?: string } = {};
    if (runNode) {
      sessions.node = "bench-node";
    }
    if (runNative) {
      sessions.native = "bench-native";
    }

    for (const session of Object.values(sessions)) {
      const resp = await sendCommand(session, {
        action: "launch",
        headless: true,
        id: "launch",
      });
      if (!(resp as { success?: boolean }).success) {
        throw new Error(`Failed to launch browser on ${session}: ${JSON.stringify(resp)}`);
      }
    }

    console.log("  Browsers launched");
    console.log("");

    const results: ScenarioResult[] = [];
    for (const scenario of scenarios) {
      process.stdout.write(`  Running: ${scenario.name}...`);
      const result = await runScenario(scenario, sessions, args.iterations, args.warmup);
      results.push(result);

      if (result.nodeStats && result.nativeStats) {
        const speedup = formatSpeedup(result.nodeStats.avgUs, result.nativeStats.avgUs);
        process.stdout.write(
          ` node=${formatDuration(result.nodeStats.avgUs)} native=${formatDuration(result.nativeStats.avgUs)} (${speedup})\n`,
        );
      } else {
        const stats = result.nodeStats ?? result.nativeStats;
        process.stdout.write(` avg=${stats ? formatDuration(stats.avgUs) : "??"}\n`);
      }
    }

    printResults(results, args.iterations, args.warmup, "daemon");

    if (args.json) {
      writeJsonResults(results, path.join(getProjectRoot(), "test/benchmarks/results.json"));
    }

    for (const session of Object.values(sessions)) {
      await sendCommand(session, { action: "close", id: "close" }).catch(() => {});
    }

    await sleep(300);

    if (runNode && runNative) {
      let totalNodeUs = 0;
      let totalNativeUs = 0;
      for (const result of results) {
        if (result.nodeStats && result.nativeStats) {
          totalNodeUs += result.nodeStats.avgUs;
          totalNativeUs += result.nativeStats.avgUs;
        }
      }
      if (totalNativeUs > 0 && totalNodeUs / totalNativeUs < 1.0) {
        process.exit(1);
      }
    }
  } finally {
    if (nodeHandle) {
      await closeDaemon(nodeHandle);
    }
    if (nativeHandle) {
      await closeDaemon(nativeHandle);
    }
  }
}

function buildHttpScenarios(baseUrl: string): Scenario[] {
  const pages = ["article.html", "dashboard.html", "ecommerce.html"];
  const httpScenarios: Scenario[] = [];

  for (const page of pages) {
    const label = page.replace(".html", "");
    httpScenarios.push({
      commands: [
        { action: "navigate", id: "nav", url: `${baseUrl}/${page}`, waitUntil: "load" },
      ],
      description: `Navigate to ${label} page over HTTP (full fetch + parse + layout)`,
      name: `http-${label}`,
    });
  }

  httpScenarios.push({
    commands: [
      { action: "navigate", id: "nav", url: `${baseUrl}/article.html`, waitUntil: "load" },
      { action: "snapshot", id: "snap" },
    ],
    description: "Navigate to article over HTTP then snapshot",
    name: "http-nav+snap",
  });

  const multiPageCmds: BenchmarkCommand[] = [];
  for (let round = 0; round < 5; round++) {
    for (const page of pages) {
      multiPageCmds.push({
        action: "navigate",
        id: `nav-${round}-${page}`,
        url: `${baseUrl}/${page}`,
        waitUntil: "load",
      });
    }
  }
  httpScenarios.push({
    commands: multiPageCmds,
    description: "Navigate 15 pages in sequence (5 rounds x 3 pages)",
    name: "http-multi-15pg",
  });

  const bulkCmds: BenchmarkCommand[] = [];
  for (let i = 0; i < 50; i++) {
    bulkCmds.push({
      action: "navigate",
      id: `bulk-${i}`,
      url: `${baseUrl}/${pages[i % pages.length]}`,
      waitUntil: "load",
    });
  }
  httpScenarios.push({
    commands: bulkCmds,
    description: "Navigate 50 pages sequentially (throughput test)",
    name: "http-bulk-50pg",
  });

  return httpScenarios;
}

async function runEngineBenchmark(args: CliArgs): Promise<void> {
  console.log("Starting local file server...");
  const { port, server } = await startFileServer();
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`  Serving pages at ${baseUrl}`);

  console.log("Starting engine benchmark daemons...");

  let chromeHandle: DaemonHandle | undefined;
  let lightpandaHandle: DaemonHandle | undefined;

  try {
    chromeHandle = spawnNativeDaemon("bench-chrome", "chrome");
    await waitForSocket("bench-chrome");
    console.log("  Chrome daemon ready");

    lightpandaHandle = spawnNativeDaemon("bench-lightpanda", "lightpanda");
    await waitForSocket("bench-lightpanda");
    console.log("  Lightpanda daemon ready");

    const sessions: Record<string, string> = {
      chrome: "bench-chrome",
      lightpanda: "bench-lightpanda",
    };

    for (const [label, session] of Object.entries(sessions)) {
      const resp = await sendCommand(session, {
        action: "launch",
        headless: true,
        id: "launch",
      });
      if (!(resp as { success?: boolean }).success) {
        throw new Error(
          `Failed to launch ${label} browser on ${session}: ${JSON.stringify(resp)}`,
        );
      }
    }
    console.log("  Browsers launched");

    const pidsToSample: number[] = [];
    if (chromeHandle.process.pid) {
      pidsToSample.push(chromeHandle.process.pid);
    }
    if (lightpandaHandle.process.pid) {
      pidsToSample.push(lightpandaHandle.process.pid);
    }
    const memorySampler = pidsToSample.length > 0 ? sampleMemory(pidsToSample, 500) : null;

    console.log("");

    const chromeMemPids = chromeHandle.process.pid ? [chromeHandle.process.pid] : [];
    const lightpandaMemPids = lightpandaHandle.process.pid
      ? [lightpandaHandle.process.pid]
      : [];
    const httpScenarios = buildHttpScenarios(baseUrl);
    const allScenarios = [...scenarios, ...engineScenarios, ...httpScenarios];
    const results: ScenarioResult[] = [];

    for (const scenario of allScenarios) {
      process.stdout.write(`  Running: ${scenario.name}...`);
      const result = await runScenarioWithErrorTolerance(
        scenario,
        sessions,
        args.iterations,
        args.warmup,
      );
      results.push(result);

      const chromeAvg = result.chromeStats ? formatDuration(result.chromeStats.avgUs) : "N/A";
      const lightpandaAvg = result.lightpandaStats
        ? formatDuration(result.lightpandaStats.avgUs)
        : "N/A";

      if (result.chromeStats && result.lightpandaStats) {
        const speedup = formatSpeedup(result.chromeStats.avgUs, result.lightpandaStats.avgUs);
        process.stdout.write(` chrome=${chromeAvg} lightpanda=${lightpandaAvg} (${speedup})\n`);
      } else {
        process.stdout.write(` chrome=${chromeAvg} lightpanda=${lightpandaAvg}\n`);
      }
    }

    const chromeMemKB = chromeMemPids.length > 0 ? getProcessMemoryKB(chromeMemPids[0]) : null;
    const lightpandaMemKB =
      lightpandaMemPids.length > 0 ? getProcessMemoryKB(lightpandaMemPids[0]) : null;
    if (memorySampler) {
      memorySampler.stop();
    }

    printResults(results, args.iterations, args.warmup, "engine");

    if (chromeMemKB || lightpandaMemKB) {
      console.log("Memory (daemon RSS after benchmarks):");
      if (chromeMemKB) {
        console.log(`  Chrome daemon:     ${formatMemory(chromeMemKB)}`);
      }
      if (lightpandaMemKB) {
        console.log(`  Lightpanda daemon: ${formatMemory(lightpandaMemKB)}`);
      }
      if (chromeMemKB && lightpandaMemKB && lightpandaMemKB > 0) {
        const ratio = chromeMemKB / lightpandaMemKB;
        console.log(`  Ratio: chrome uses ${ratio.toFixed(1)}x more memory`);
      }
      console.log("");
    }

    if (args.json) {
      writeJsonResults(
        results,
        path.join(getProjectRoot(), "test/benchmarks/results-engine.json"),
        "engine",
      );
    }

    for (const session of Object.values(sessions)) {
      await sendCommand(session, { action: "close", id: "close" }).catch(() => {});
    }

    await sleep(300);
  } finally {
    if (chromeHandle) {
      await closeDaemon(chromeHandle);
    }
    if (lightpandaHandle) {
      await closeDaemon(lightpandaHandle);
    }
    await stopFileServer(server);
  }
}

async function main(): Promise<void> {
  const args = parseArgs();

  cleanupSockets();

  try {
    if (args.engineMode) {
      await runEngineBenchmark(args);
    } else {
      await runDaemonBenchmark(args);
    }
  } finally {
    cleanupSockets();
  }
}

main().catch((error) => {
  console.error("Benchmark failed:", error instanceof Error ? error.message : error);
  process.exit(2);
});
