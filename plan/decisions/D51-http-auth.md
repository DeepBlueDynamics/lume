# D51 — HTTP, MCP and SSE authentication (PROPOSED)

Status: **Opt-in flag implemented in A11; default changes and role policy still PROPOSED, awaiting the user's decision.**

A11 adds `--http-token-file <path>` to plain/TI `lume serve` and `lume ti ingest --serve`. The UTF-8 file is trimmed, must be nonempty, and is read once at startup; restart to rotate it. Before dispatch or reading request bodies, every HTTP route checks a bearer in constant time and rejects missing/wrong/ambiguous credentials with 401 and an empty body. A configured sync token takes precedence on `/ti/manifest` and `/ti/shards/*`; an OTLP token takes precedence on `/v1/*`; otherwise the HTTP token applies. No-flag behavior, binds, existing route authentication and roles remain unchanged. Authenticated listeners omit the unauthenticated startup warning. PostgreSQL SCRAM remains independent.

A8 found that a non-loopback listener exposes read-only vessel/document queries and ordinary-index MCP operations without authentication. Startup warnings shipped as a compatibility-preserving interim measure.

Propose a separate bearer-token policy for HTTP /ti and MCP, including SSE connection establishment and subsequent message requests. Check authorization before parsing or executing operations. Use independently revocable read and index roles: read allows schema, status, resolve, explain and read-only query/search; index allows the existing index mutation operations. Never put bearer tokens in query strings or log them. Load secrets from a private file or supported environment variable; compare credentials in constant time. The default plain `lume serve` bind should become loopback, with explicit opt-in for remote listeners.

Non-loopback listeners should require credentials unless an explicit, prominently warned compatibility override is selected. Signal K's loopback plugin proxy can continue to use the local trusted path; specify the trust boundary when implementing it. Do not reuse PostgreSQL SCRAM verifiers as bearer credentials: SCRAM authenticates a different protocol and cannot supply a reusable HTTP token.

For shore access, require HTTPS through a configured TLS terminator/reverse proxy or a future native TLS listener. Bearer authentication over cleartext does not protect credentials. Proxy configuration must restrict upstream access and pass the authenticated principal safely.

Before approval, settle token provisioning/rotation, role assignment, plugin interoperability, and compatibility defaults. Implementation acceptance should cover missing/incorrect tokens, read-versus-index authorization, every HTTP/MCP/SSE entry point, secret redaction, and unchanged authenticated read-only behavior. A11 implements only the explicitly authorized opt-in bearer flag, without new dependencies; the remaining policy is still proposed.
