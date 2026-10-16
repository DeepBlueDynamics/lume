# Learning from judged SQL (Addita)

`lume agent` and `lume chat` can judge each completed SQL Q&A run against an Addita or Ollaya `/api/decide` endpoint, record the run in `<store>/agent_runs/YYYY-MM-DD.jsonl`, and inject the top 3 BM25-ranked GOOD SQL examples from the same store into subsequent prompts.

## CLI flags & environment

| Flag / Env | Default | Description |
|---|---|---|
| `--decide-url <URL>` / `LUME_DECIDE_URL` | `~/.addita/serve.json` if present, else disabled | Base URL of the Addita or Ollaya decide server (e.g. `http://127.0.0.1:8090`). When unset, Lume checks `~/.addita/serve.json` (`{url, token_file?}`, overridable via `LUME_ADDITA_DISCOVERY_FILE`). |
| `--decide-model <MODEL>` | `laya` | Model name sent in `POST {decide_url}/api/decide` |
| `--decide-token-file <PATH>` / `LUME_DECIDE_TOKEN_FILE` | `token_file` from `~/.addita/serve.json` if present | File containing the Bearer token for `/api/decide` (never passed on argv or written to logs) |
| `--decide-threshold <VAL>` | `0.8` | Minimum `noul` score (`0.0..=1.0`) required on both criteria when `addita.final` is absent |
| `--decide-preset` | *(inline)* | Send `"preset": "lume.sql_judge"` instead of inline `questions` |
| `--decide-timeout <SECS>` / `LUME_DECIDE_TIMEOUT` | `30` | Timeout in seconds for `POST {decide_url}/api/decide` |
| `--no-decide` | `false` | Disable calling `/api/decide` even when `LUME_DECIDE_URL`, `--decide-url`, or `~/.addita/serve.json` is present |
| `--no-agent-log` | `false` | Disable appending run records to `<store>/agent_runs/YYYY-MM-DD.jsonl` |
| `--no-examples` | `false` | Disable loading and injecting past GOOD SQL runs into the system prompt |

## How it works

1. **Post-answer judging (`POST {decide_url}/api/decide`):**
   - After the final answer has been printed, Lume probes `GET {decide_url}/health` or `GET {decide_url}/api/version` (including `"addita": {"ref": "lume-run-<uuid>"}` when the server identifies as `{"service": "addita"}`, and omitting it for plain Ollaya).
   - Lume sends a single non-streaming `POST {decide_url}/api/decide` (30 s default timeout via `--decide-timeout` / `LUME_DECIDE_TIMEOUT`, no retries) with `state`: `question`, `sql` (last successful executed SQL), `sql_all` (all executed SQL queries), `rows` (up to 20 sample rows, total serialized `state` capped at 16 KB), `rows_truncated` (`bool`), `error` (`null` or error string), and `answer`.
   - Cloud vs local teacher selection is an Addita server setting (`ADDITA_NO_CLOUD`). Lume records `routing_model`, student `noul` scores, `student_ms`, `teacher_ms`, `teacher_model`, and `teacher_host` in the run log, and prints a one-line stderr note (`judged by cloud teacher <model>`) when `teacher_host` is not loopback/LAN.
   - A run is marked `good: true` when `addita.final` has both `rows_answer_question: true` and `answer_supported: true`, or (when no `addita.final` block is present) when both `noul` scores are `>= --decide-threshold` (`0.8`). If `addita.teacher` is `"pending"`, Lume records `judged: "pending"` and `good: false`. On any non-2xx HTTP status, transport error, JSON error, or timeout, Lume records the error `code` string as `error_code` and `judged: false`.

2. **Run log (`<store>/agent_runs/YYYY-MM-DD.jsonl`):**
   - Each run appends one JSON line to `<ti-store>/agent_runs/YYYY-MM-DD.jsonl` (or `<docs-index>/agent_runs/YYYY-MM-DD.jsonl` when `--ti-store` is not set), with directory mode `0700`, file mode `0600`, and automatic 30-day retention pruning on write.

3. **BM25 few-shot SQL examples:**
   - At the start of a run, Lume loads recent `good: true` runs from the same store (`<= 90` days, `<= 2000` lines)—including `judged: "pending"` runs whose `ref` was later resolved as `true/true` via an `addita.decision.final` row in the TI store's `docs` table—ranks them against the new question using Lume's in-memory BM25 index (no Shivvr dependency), deduplicates by SQL, and injects up to 3 examples into the system prompt:
     ```text
     Past questions on this store answered with working SQL (examples, adapt; don't copy blindly):
     Q: … SQL: …
     ```
