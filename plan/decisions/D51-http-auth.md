# D51 — HTTP, MCP and SSE authentication (PROPOSED)

Status: **PROPOSED; awaiting the user's decision. No runtime changes are authorized by this record.**

A8 found that a non-loopback listener exposes read-only vessel/document queries and ordinary-index MCP operations without authentication. Startup warnings shipped as a compatibility-preserving interim measure.

Propose a separate bearer-token policy for HTTP /ti and MCP, including SSE connection establishment and subsequent message requests. Check authorization before parsing or executing operations. Use independently revocable read and index roles: read allows schema, status, resolve, explain and read-only query/search; index allows the existing index mutation operations. Never put bearer tokens in query strings or log them. Load secrets from a private file or supported environment variable; compare credentials in constant time. The default plain `lume serve` bind should become loopback, with explicit opt-in for remote listeners.

Non-loopback listeners should require credentials unless an explicit, prominently warned compatibility override is selected. Signal K's loopback plugin proxy can continue to use the local trusted path; specify the trust boundary when implementing it. Do not reuse PostgreSQL SCRAM verifiers as bearer credentials: SCRAM authenticates a different protocol and cannot supply a reusable HTTP token.

For shore access, require HTTPS through a configured TLS terminator/reverse proxy or a future native TLS listener. Bearer authentication over cleartext does not protect credentials. Proxy configuration must restrict upstream access and pass the authenticated principal safely.

Before approval, settle token provisioning/rotation, role assignment, plugin interoperability, and compatibility defaults. Implementation acceptance should cover missing/incorrect tokens, read-versus-index authorization, every HTTP/MCP/SSE entry point, secret redaction, and unchanged authenticated read-only behavior. D51 adds no dependencies or authentication code.
