# M5 item 2: agent-only MCP run

Spec 12 says: “A nemesis8 agent with only Lume MCP answers 20 scripted fleet
questions; integrator grades against oracle results.” The lead's runtime ruling
uses this stdlib Python tool-call loop when no nemesis8 launcher is available.
It starts no server, model, subprocess or external agent. Completion means an
answer was submitted, not that it was correct. The integrator grades the run.

## Host preparation

Start an existing Lume HTTP MCP server on loopback in a separate terminal:

```sh
lume serve --bind 127.0.0.1 --port 5863 --ti-store .lanes/data/count-paths-enabled-host
```

Use the existing boat correctness corpus with imported notes, logbook and alerts.
The store must enable `[ingest] count_paths = ["electrical.bilge.pumpCycles"]`
during backfill: questions 3 and 4 need sample counts, not a mean alias. Do not
reinterpret an already-built empty-list store. All question dates are UTC and
MMSI 367000000 denotes `vessels.urn:mrn:imo:mmsi:367000000`.

The NEW `tests/golden/agent_questions.json` contains 20 natural-language
questions, with coverage Q1=3, Q2=2, Q3=3, Q4=2, Q5=2, Q6=3, Q7=3, Q8=2.
Notes matches and alerts are included. It does not reuse fleet_questions.json.
Hidden DuckDB SQL is derived from the boat corpus oracle twins; there are no TI
SQL templates. Extraction and exact/tolerance/set grading rules are hidden too.

Generate the expected answers **on the host** with its existing DuckDB:

```sh
python bench/agent_mcp_expected.py --data-dir .lanes/data/correctness \
  --output .lanes/data/agent-mcp-run/expected.json \
  --scratch .lanes/data/agent-mcp-run/oracle-scratch
```

The generator uses raw Parquet and docs via raw_view.sql, UTC, two threads,
512 MiB memory and at most 4 GiB spill. It records the question fixture SHA-256.
It installs nothing. Keep this output outside git; never pass it to the harness.
Grading remains manual; expected generation is not an agent evaluation.

## Agent run

With Ollama already serving an OpenAI-compatible endpoint:

```sh
python3 bench/agent_mcp_run.py \
  --mcp-url http://127.0.0.1:5863/mcp \
  --llm-url http://localhost:11434/v1 --model qwen2.5:7b
```

A second run can select a cloud chat endpoint with `--llm-url` and `--model`.
Authentication comes from `LLM_API_KEY` (or `OPENAI_API_KEY`), only in the HTTP
authorization header. No key or headers are written to transcripts. Endpoint
credentials and redirects are refused. MCP must be on loopback; no bind is
changed by this program.

The harness fetches tools/list (including pagination) and offers exactly those
tools plus the local final `answer` tool. Calls outside that allowlist are
rejected and never forwarded. Tool results, including MCP errors and truncation
fields, are retained verbatim. The model must discover schema, resolve columns,
construct its own SQL and submit its final answer. Only question text is copied
into the prompt; hidden fields never enter the message history. Each question
starts with fresh messages and has at most 12 chat turns and 20 attempted tool
calls, including rejected calls and the final answer. Smaller caps are optional;
larger caps are rejected.

Transcripts are saved after each turn/call to
`.lanes/data/agent-mcp-run/<UTC timestamp>/<qid>.json`:
question, model, messages, tool calls/arguments/results, submitted SQL, final
answer, completion/limit/error status, per-call and model timings. tools.json
records the offered MCP definitions. summary.md and summary.json contain the run
summary, with `graded: false`. Use `--output` to override the run directory;
it must not already exist. In a lane checkout the default uses the shared
repository .lanes/data, not a nested lane data directory.

A plain assistant reply does not finish a question: it must use `answer`.
Failures and cap exits keep partial transcripts and cause a nonzero run exit.
A completed answer with missing evidence is still available for the integrator
to grade; the runner does not label accuracy PASS/FAIL.

The chat adapter uses the [Chat Completions function-tool message format](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).

## Offline checks

```sh
python3 -m unittest discover -s bench -p 'test_agent_mcp_run.py' -v
python3 -m unittest discover -s bench -p 'test_*.py'
python3 -m unittest discover -s bench/grafana -p 'test_*.py'
```

Tests use actual fake chat and MCP HTTP servers bound to 127.0.0.1, exercise a
20-question CLI run, serialize every model request to check hidden-field
exclusion, and check caps, rejected names, malformed arguments, SQL capture,
credential exclusion and transcript shape. No live LLM or DuckDB is required by
these tests. Real Ollama/cloud runs and oracle grading are the integrator's gate.
