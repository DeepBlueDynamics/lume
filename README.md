# 📖 Lume: Stylistic Synthesis & Retrieval-Augmented Generation Suite

Lume is a high-performance, FST-backed tagger, hybrid lexical/semantic search engine, and agentic document exploration suite. Written in Rust, it supports hybrid retrieval, semantic knowledge graph walks, autonomous agent loops, and document summarization.

---

## 📐 Architecture & Core Components

All capabilities of Lume are exposed to the autonomous agent as JSON RPC tools and map directly to CLI commands that a user can run manually to see the raw results.

### 1. Hybrid Search Architecture
This diagram represents the hybrid search pipeline executed by the `lume_search` tool:

```mermaid
graph TD
    subgraph lume_search [Tool: lume_search | CLI: lume search]
        User([User Prompt / Query]) --> Search[Hybrid Search Engine]
        Search -->|1. BM25 Lexical Search| BM25[(BM25 Index)]
        Search -->|2. Dense Semantic Embeddings| Vector[(Semantic Vector Cache)]
        Search -->|3. Graph Boost| Graph[(Semantic Knowledge Graph)]
        BM25 --> Hits[Merged & Scored Hits]
        Vector --> Hits
        Graph --> Hits
    end
    Hits --> Synthesis[Ollama/Cloud LLM Synthesis]
    Synthesis --> Output([Coherent Response])
```

### 2. Keyterm Extraction & Graph-Guided Summarization Architecture
This diagram shows how keyterms are extracted via `lume_index` and later used to guide query planning during document summarization:

```mermaid
graph TD
    subgraph Indexing [Tool: lume_index | CLI: lume index]
        Doc[Raw Documents] --> Parse[Text Chunking]
        Parse -->|If -o flag enabled| EntExt[LLM Keyterm & Entity Extraction]
        EntExt -->|Build Entity Edges| SKG[(entity_graph.json)]
    end

    subgraph Summarization [CLI: lume summarize]
        SKG -->|Extract Top 12 Keyterms by Freq| Prior[Keyterm Priority Prior]
        Prior -->|Inject as prompt guide| Planner[LLM Search Planner]
        Planner -->|Generate Guided Queries| Queries[Search Queries]
        Queries -->|Execute lume_search Tool| Retrieval[Retrieve Passage Snippets]
        Retrieval -->|Deduplicate & Aggregate| Context[Aggregated Context]
        Context -->|Synthesize Summary| FinalSummary[Executive Summary]
    end
```

### 3. Autonomous Agent Loop Architecture
This diagram represents the stateful tool-calling loop (`lume agent`) where the LLM plans and executes commands iteratively:

```mermaid
graph TD
    User([User Question]) --> Agent[Agent Chat Loop]
    Agent --> LLM{Ollama / Cloud LLM}
    
    LLM -->|Wants to call a tool| Tool[Tool Dispatcher]
    Tool -->|query| SearchTool[Tool: lume_search | CLI: lume search]
    Tool -->|dir, db| IndexTool[Tool: lume_index | CLI: lume index]
    Tool -->|seed, steer| GenTool[Tool: lume_generate | CLI: lume generate]
    
    SearchTool --> Result[Capture CLI Output]
    IndexTool --> Result
    GenTool --> Result
    
    Result -->|Feed output back into history| Agent
    
    LLM -->|Decides it has the answer| Answer[Return Final Response]
    Answer --> Output([Coherent, Fact-Verified Answer])
```

The system is organized into the following core Rust and Python modules:

*   **FST-Backed Phrase Tagger**: Performs longest-dominant-right matching using Lucene-style separator bytes. Built on [Tagger](file:///workspace/lume/src/lib.rs#L111) and [Entry](file:///workspace/lume/src/lib.rs#L43) in [src/lib.rs](file:///workspace/lume/src/lib.rs).
*   **Hybrid Search Engine**: Integrates BM25 lexical retrieval ([Bm25Index](file:///workspace/lume/src/bm25.rs)), spelling correction ([SpellIndex](file:///workspace/lume/src/spelling.rs)), and dense embeddings ([src/hybrid.rs](file:///workspace/lume/src/hybrid.rs)) with graph-steered query expansion ([src/graph_search.rs](file:///workspace/lume/src/graph_search.rs)) to boost matches based on Semantic Knowledge Graph connections.
*   **Steered Markov Chain Synthesizer**: Under the hood, Lume uses a trigram [MarkovChain](file:///workspace/lume/src/semantic_mesh.rs#L129) to generate text. However, it goes beyond random walks by steering/biasing trigram transitions using FST tags, local attention feedback, and GTR-T5 semantic vector inversion ([src/inversion.rs](file:///workspace/lume/src/inversion.rs)).
*   **Agent & Summarization Engine**: Runs autonomous query planning, search exploration, and structured synthesis. Main entry points are [run_agent_loop](file:///workspace/lume/src/agent.rs#L703) and [summarize_document](file:///workspace/lume/src/agent.rs#L926) in [src/agent.rs](file:///workspace/lume/src/agent.rs). Supports failure recovery via [lume_not_found](file:///workspace/lume/src/agent.rs#L791).
*   **Model Context Protocol (MCP)**: Implements an MCP server over HTTP transport in [serve](file:///workspace/lume/src/agent.rs#L651) to expose indexing and search tools directly to AI agents.
*   **Python Document Extractor**: A high-efficiency parser ([lib/lume_extractor.py](file:///workspace/lume/lib/lume_extractor.py)) that handles PDF page text extraction and generates Q&A benchmark datasets using concurrent Ollama threads.

---

## 🚀 Installation & Quick Start

### Prerequisites
*   [Rust & Cargo](https://rustup.rs/) (v1.75+ recommended)
*   [Ollama](https://ollama.com/) running locally or accessible in your environment (defaults to using the cloud-backed model `gemma4:31b-cloud`).
*   [Python 3.10+](https://www.python.org/) with `requests` and `pypdf` installed (for PDF indexing/Q&A generation).

### Building the CLI
Build the release profile binary:
```bash
cargo build --release
```
The compiled binary will be located at `target/release/lume`.

---

## 🛠️ CLI Subcommands & Tool Execution Reference

Each tool exposed to the agent maps to a CLI subcommand. A user can run these directly to see raw search hits, index logs, or Markov generated texts.

### 1. `lume_index` Tool $\rightarrow$ `lume index` CLI Command
Indexes a directory containing text, markdown, or PDF files.
```bash
# Basic lexical indexing
./target/release/lume index docs/my_documents

# Semantic indexing with dense vectors (-s) and Ollama Entity Graph extraction (-o)
./target/release/lume index -s -o docs/my_documents
```
*   **Raw Output**: Prints file indexing progress, chunk counts, semantic cache updates, and entity extraction timings.
*   **Flags**:
    *   `-s, --semantic`: Enables dense vector search (requires a NUTS token).
    *   `-o, --ollama-entities`: Extract central entities and construct `entity_graph.json`.
    *   `-f, --force`: Forces re-indexing of all documents.
*   **Options**:
    *   `--db <PATH>`: Destination directory for the index metadata [default: `.lume-index`].
    *   `--ollama-model <MODEL>`: Ollama model for entity extraction [default: `gemma4:2b`].

---

### 2. `lume_search` Tool $\rightarrow$ `lume search` CLI Command
Queries the persisted index using lexical (BM25) or hybrid search:
```bash
# Basic BM25 search
./target/release/lume search "Edmond Dantes"

# Hybrid search (weighting: 0.5 BM25, 0.5 vector semantic) with spelling correction (-c)
./target/release/lume search -c -a 0.5 "Edmond Dantes"
```
*   **Raw Output**: Prints raw retrieved document passages accompanied by match scores (BM25 + Semantic + SKG Boost).
*   **Options**:
    *   `-a, --alpha <VAL>`: Hybrid weight. `0.0` is lexical-only; `1.0` is semantic-only [default: `0.5`].
    *   `-g, --graph <VAL>`: Entity graph boost weight [default: `0.4`]. Enables **graph-steered expansion**: Lume resolves entities in the query, walks one hop to their strongest neighbors in `entity_graph.json`, and boosts matching passage scores by the related-entity mass.
    *   `-l, --limit <LIMIT>`: Maximum search hits [default: `10`].

---

### 3. `lume_generate` Tool $\rightarrow$ `lume generate` CLI Command
Synthesizes style-faithful text based on the indexed corpus using a trigram Markov Chain:
```bash
# Generate styled text starting with Dantes and guided by concept keywords
./target/release/lume generate "Dantes" --steer "revenge,castle"
```
*   **Raw Output**: Prints a block of synthesized text in the style of the indexed corpus.
*   **Modes**:
    *   **Tag-Steered Mode**: Biases transitions towards the `--steer` tags using co-occurrence weights from the index's posting lists.
    *   **Vector-Steered Inversion Mode**: Automatically embeds the target seed, inverts it into its closest semantic tags, and runs multiple candidate generation rounds to find the closest cosine-similarity match to the target prompt.

---

### 4. Graph-Guided Summarization (`lume summarize` Command)
Summarize an entire document using an agentic planning-and-retrieval loop guided by the highest-ranking nodes in the Semantic Knowledge Graph:
```bash
./target/release/lume summarize docs/my_documents/book.pdf
```
*   **How it works**:
    1. Reads `entity_graph.json` to identify the top 12 central concepts.
    2. Passes these concepts as priors to the Ollama model.
    3. Plans a series of distinct search queries targeting the key concepts.
    4. Executes queries, aggregates unique passages, and synthesizes a high-level executive summary.

---

### 5. Autonomous Agent Chat Loop (`lume agent` Command)
Spawn an autonomous agent to research and resolve a complex question by executing indexing and search tools iteratively:
```bash
./target/release/lume agent "Explain the relationship between Villefort and Mercedes"
```
*   **Structured Failure Recovery**: If the agent's searches do not yield the required information, it calls a dedicated `lume_not_found` tool. The system then provides structured guidance prompting the agent to refine its query keywords or search broad/narrow variations, preventing premature halts or false answers.

---

### 6. Starting the MCP Server (`lume serve` Command)
Start the Model Context Protocol HTTP server to connect Lume to external AI agents:
```bash
./target/release/lume serve --port 8080
```

---

### 7. Crawling Web Pages (`lume crawl` Command)
Crawls a target website to extract its text/markdown representation and saves the file to the local personal search engine directory (`examples/crawled/`):
```bash
# Crawl a webpage
./target/release/lume crawl https://example.com

# Crawl a Hacker News story
./target/release/lume crawl https://news.ycombinator.com/item?id=8863
```
*   **How it works**:
    *   **Local Crawling (Tokenless)**: If `GRUB_BASE_URL` is set to a local endpoint (such as `http://localhost:6792` or when running locally), Lume connects to the local Grub instance and crawls without requiring any authentication or `NUTS_SERVICES_TOKEN`.
    *   **Remote Crawling (Authenticated)**: If `GRUB_BASE_URL` points to a remote endpoint (e.g. `grub.nuts.services`), Lume uses the `NUTS_SERVICES_TOKEN` environment variable to authenticate. If the token is missing, it falls back to direct HTTP GET (no JavaScript execution).
    *   **Hacker News Special Handling**: If a Hacker News story URL is detected, Lume queries the public HN Firebase API to retrieve both the story post and its top-level discussion comments, assembling them into a clean Markdown file.

---

## 🐍 Python Extractor & Q&A Generator

Located at [lib/lume_extractor.py](file:///workspace/lume/lib/lume_extractor.py), this tool can extract text and generate Q&A evaluation datasets from document chunks:

```bash
# Extract text from a PDF
python lib/lume_extractor.py pdf my_doc.pdf

# Generate a Q&A evaluation benchmark using Ollama
python lib/lume_extractor.py qna my_doc.txt output_qna.json --model gemma4:31b-cloud
```

---

## 💻 Codebase Indexing & Search Demo

Lume can index and search programming code repositories (like Lume's own Rust source files).

### 1. Indexing the Codebase
Index the `src/` directory containing Lume's Rust modules into a separate index database folder:
```bash
./target/release/lume index --db .lume-code-index src
```

### 2. Searching the Codebase for a Symbol
Query the code index for the `run_agent_loop` function to find raw code definitions:
```bash
./target/release/lume search --db .lume-code-index "run_agent_loop"
```

### Example Raw Output
```text
[1] Score: 8.4109 | Lines 700-725 (File: src/agent.rs, Line: 703)
pub fn run_agent_loop(
    question: &str,
    ollama_url: &str,
    ollama_model: &str,
    db_dir: &str,
    verbose: bool,
) -> Result<(), String> {
    let url = format!("{}/api/chat", ollama_url.trim_end_matches('/'));
```

---

## 💡 Acknowledgements & Inspiration

Lume was forked from and inspired by the foundational FST-based tagging work in [jsclosures/rust-fstguardrails](https://github.com/jsclosures/rust-fstguardrails).
