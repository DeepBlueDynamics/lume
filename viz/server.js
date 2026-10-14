// Bridge between the browser and `lume stream`.
//
// Serves the built React app (dist/) over HTTP and exposes a WebSocket. When a
// client sends {type:"search", query, db, candidates, steps}, the bridge spawns
//   lume stream <query> --db <db> -k <candidates> --steps <steps>
// reads its NDJSON stdout line by line, and forwards each frame to the client.
// The browser can't read a child process or a raw socket; this is the relay.
//
// Config via env: PORT (default 8086), HOST (default 127.0.0.1), LUME_BIN
// (default ../target/release/lume[.exe], else `lume` on PATH), OLLAMA_URL
// (default http://localhost:11434; also passed to `lume answer`).
//
// GET /api/models lists the Ollama models for the Ask picker, split into cloud
// and local. GET /api/info?db=<path> reports the lume version and whether the
// index exists, its format and when it was last written.

import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { createReadStream, existsSync, statSync } from "node:fs";
import { extname, join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import { WebSocketServer } from "ws";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 8086);
const HOST = process.env.HOST || "127.0.0.1";
const OLLAMA_URL = (process.env.OLLAMA_URL || "http://localhost:11434").replace(/\/+$/, "");
const DIST = join(__dirname, "dist");
const REPO = join(__dirname, "..");

function resolveLumeBin() {
  if (process.env.LUME_BIN) return process.env.LUME_BIN;
  const win = join(REPO, "target", "release", "lume.exe");
  const nix = join(REPO, "target", "release", "lume");
  if (existsSync(win)) return win;
  if (existsSync(nix)) return nix;
  // On Windows, npm's `lume` is a .cmd shim that can't be spawned without a
  // shell, so use the real binary from the global npm install instead.
  if (process.platform === "win32" && process.env.APPDATA) {
    const npmExe = join(process.env.APPDATA, "npm", "node_modules", "@deepbluedynamics", "lume",
      "node_modules", "@deepbluedynamics", "lume-win32-x64", "bin", "lume.exe");
    if (existsSync(npmExe)) return npmExe;
  }
  return "lume"; // installed copy on PATH
}
const LUME_BIN = resolveLumeBin();

function lumeVersion() {
  try {
    return execFileSync(LUME_BIN, ["--version"], { encoding: "utf8", timeout: 10000 }).trim();
  } catch {
    return null;
  }
}
const LUME_VERSION = lumeVersion();

// What the index at `db` (relative to the repo root) looks like on disk.
function indexInfo(db) {
  const dir = resolve(REPO, db);
  if (!existsSync(dir)) return { db, exists: false };
  const v4 = join(dir, "index.json");
  const legacy = join(dir, "state.json");
  const marker = existsSync(v4) ? v4 : existsSync(legacy) ? legacy : null;
  if (!marker) return { db, exists: false };
  return { db, exists: true, format: marker === v4 ? "v4" : "json", modified: statSync(marker).mtime.toISOString() };
}

// Ollama models for the Ask picker. Cloud models run on ollama.com through the
// local Ollama: names ending in ":cloud", or aliases that report a remote host.
async function ollamaModels() {
  const res = await fetch(`${OLLAMA_URL}/api/tags`, { signal: AbortSignal.timeout(5000) });
  if (!res.ok) throw new Error(`Ollama answered ${res.status}`);
  const { models = [] } = await res.json();
  return models
    .map((m) => ({
      name: m.name,
      cloud: m.name.endsWith(":cloud") || Boolean(m.remote_host),
      remote: m.remote_model || null,
      size: m.size || 0,
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

// Only pages served from this machine (the bridge itself, or `npm run dev`) may
// use the API or the WebSocket, so another website open in the browser can't
// drive lume through it.
const LOCAL_ORIGIN = /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/;
const localOrigin = (origin) => !origin || LOCAL_ORIGIN.test(origin);

function sendJson(req, res, status, body) {
  const headers = { "Content-Type": "application/json", "Cache-Control": "no-store" };
  if (req.headers.origin && localOrigin(req.headers.origin)) headers["Access-Control-Allow-Origin"] = req.headers.origin;
  res.writeHead(status, headers);
  res.end(JSON.stringify(body));
}

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".json": "application/json", ".svg": "image/svg+xml", ".ico": "image/x-icon" };

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url || "/", "http://localhost");
  if (url.pathname.startsWith("/api/") && !localOrigin(req.headers.origin)) {
    sendJson(req, res, 403, { error: "forbidden origin" });
    return;
  }
  if (url.pathname === "/api/models") {
    try {
      sendJson(req, res, 200, { ollama: OLLAMA_URL, models: await ollamaModels() });
    } catch (e) {
      sendJson(req, res, 502, { ollama: OLLAMA_URL, error: `Can't reach Ollama: ${e.message}` });
    }
    return;
  }
  if (url.pathname === "/api/info") {
    sendJson(req, res, 200, { lume: LUME_VERSION, bin: LUME_BIN, index: indexInfo(url.searchParams.get("db") || ".lume-index") });
    return;
  }
  let path = decodeURIComponent(url.pathname);
  if (path === "/") path = "/index.html";
  const file = join(DIST, path);
  if (!file.startsWith(DIST) || !existsSync(file)) {
    // SPA fallback to index.html when the build exists.
    const index = join(DIST, "index.html");
    if (existsSync(index)) {
      res.writeHead(200, { "Content-Type": "text/html" });
      createReadStream(index).pipe(res);
      return;
    }
    res.writeHead(404, { "Content-Type": "text/plain" });
    res.end("Run `npm run build` first, or use `npm run dev` for the dev server.");
    return;
  }
  res.writeHead(200, { "Content-Type": MIME[extname(file)] || "application/octet-stream" });
  createReadStream(file).pipe(res);
});

const wss = new WebSocketServer({ server, verifyClient: ({ origin }) => localOrigin(origin) });

wss.on("connection", (ws) => {
  let child = null;

  const killChild = () => {
    if (child) {
      try { child.kill(); } catch {}
      child = null;
    }
  };

  ws.on("message", (raw) => {
    let msg;
    try { msg = JSON.parse(raw.toString()); } catch { return; }

    const db = msg.db || ".lume-index";
    const k = String(msg.candidates || 24);
    const steps = String(msg.steps || 160);
    let args;

    if (msg.type === "ask" && msg.question) {
      // Agentic answer loop: plan → retrieve → evaluate → refine → cited answer.
      args = ["answer", msg.question, "--db", db, "-k", k, "--steps", steps];
      if (typeof msg.model === "string" && /^[\w.:/-]{1,128}$/.test(msg.model)) args.push("--model", msg.model);
    } else if (msg.type === "search") {
      const queries = (Array.isArray(msg.queries) ? msg.queries : [msg.query]).filter(Boolean);
      if (!queries.length) return;
      args = ["stream", queries[0], "--db", db, "-k", k, "--steps", steps];
      for (const q of queries.slice(1)) args.push("--add", q);
    } else {
      return;
    }

    killChild();

    ws.send(JSON.stringify({ type: "status", state: "running", bin: LUME_BIN, args }));
    child = spawn(LUME_BIN, args, { cwd: REPO, env: { ...process.env, OLLAMA_URL } });

    const rl = createInterface({ input: child.stdout });
    rl.on("line", (line) => {
      const t = line.trim();
      if (t.startsWith("{") && ws.readyState === ws.OPEN) ws.send(t); // forward frame verbatim
    });

    let err = "";
    child.stderr.on("data", (d) => { err += d.toString(); });
    child.on("close", (code) => {
      if (ws.readyState === ws.OPEN) {
        ws.send(JSON.stringify({ type: "status", state: "closed", code, stderr: err.slice(-2000) }));
      }
      child = null;
    });
    child.on("error", (e) => {
      if (ws.readyState === ws.OPEN) ws.send(JSON.stringify({ type: "status", state: "error", message: e.message }));
    });
  });

  ws.on("close", killChild);
});

server.listen(PORT, HOST, () => {
  console.log(`lume-viz bridge on http://${HOST}:${PORT}`);
  console.log(`  lume binary: ${LUME_BIN} (${LUME_VERSION || "NOT FOUND — set LUME_BIN"})`);
  console.log(`  ollama: ${OLLAMA_URL}`);
  console.log(existsSync(DIST) ? `  serving dist/` : `  dist/ not built — run \`npm run build\`, or use \`npm run dev\``);
});
