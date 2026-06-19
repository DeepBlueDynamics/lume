// Bridge between the browser and `lume stream`.
//
// Serves the built React app (dist/) over HTTP and exposes a WebSocket. When a
// client sends {type:"search", query, db, candidates, steps}, the bridge spawns
//   lume stream <query> --db <db> -k <candidates> --steps <steps>
// reads its NDJSON stdout line by line, and forwards each frame to the client.
// The browser can't read a child process or a raw socket; this is the relay.
//
// Config via env: PORT (default 8086), LUME_BIN (default ../target/release/lume[.exe]).

import http from "node:http";
import { spawn } from "node:child_process";
import { createReadStream, existsSync } from "node:fs";
import { extname, join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import { WebSocketServer } from "ws";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 8086);
const DIST = join(__dirname, "dist");

function resolveLumeBin() {
  if (process.env.LUME_BIN) return process.env.LUME_BIN;
  const win = join(__dirname, "..", "target", "release", "lume.exe");
  const nix = join(__dirname, "..", "target", "release", "lume");
  return existsSync(win) ? win : nix;
}
const LUME_BIN = resolveLumeBin();

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".json": "application/json", ".svg": "image/svg+xml", ".ico": "image/x-icon" };

const server = http.createServer((req, res) => {
  let path = decodeURIComponent((req.url || "/").split("?")[0]);
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

const wss = new WebSocketServer({ server });

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
    if (msg.type !== "search") return;
    // Accept a queries array (additive search) or a single query.
    const queries = (Array.isArray(msg.queries) ? msg.queries : [msg.query]).filter(Boolean);
    if (!queries.length) return;

    killChild();
    const args = ["stream", queries[0],
      "--db", msg.db || ".lume-index",
      "-k", String(msg.candidates || 24),
      "--steps", String(msg.steps || 160)];
    for (const q of queries.slice(1)) args.push("--add", q);

    ws.send(JSON.stringify({ type: "status", state: "running", bin: LUME_BIN, args }));
    child = spawn(LUME_BIN, args, { cwd: join(__dirname, "..") });

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

server.listen(PORT, () => {
  console.log(`lume-viz bridge on http://localhost:${PORT}`);
  console.log(`  lume binary: ${LUME_BIN}${existsSync(LUME_BIN) ? "" : "  (NOT FOUND — set LUME_BIN)"}`);
  console.log(existsSync(DIST) ? `  serving dist/` : `  dist/ not built — run \`npm run build\`, or use \`npm run dev\``);
});
