# Lume operator manual

## 1. Network exposure

Every serve path defaults to `127.0.0.1`: plain `lume serve`, serving a TI
store, and `lume ti ingest --serve`. Local clients can connect without
authentication when no authentication flags are configured. PostgreSQL also
defaults to the HTTP bind unless you explicitly set `--pg-bind`; its SCRAM
authentication is configured separately.

To make HTTP accessible on a LAN, choose an explicit bind and authentication.
For nuts.services authentication, allow only your intended users:

```sh
lume serve --bind 0.0.0.0 --nuts-auth --nuts-allow you@example.com
```

Clients send a nuts JWT or an `ahp_` token in the Authorization bearer header.
The allowlist is mandatory. Direct JWT verification uses cached public keys;
AHP exchange requires the auth service. Without cached keys or network access,
nuts-auth startup fails. Read scope permits TI queries, MCP and SSE. Write scope
permits OTLP ingestion, sync mutations and indexing; indexing through MCP needs
both read and write scopes.

Alternatively, use a static bearer token stored in a private, nonempty file:

```sh
lume serve --bind 0.0.0.0 --http-token-file /path/to/private/http-token
```

Send that token in `Authorization: Bearer <token>`. It grants full access;
do not put it in a URL or share it publicly. An explicit non-loopback bind
without either authentication method is refused before the listener opens.
For remote connections, put HTTPS in front of Lume: bearer authentication
alone does not encrypt HTTP traffic.

GET `/health` is public when nuts auth is enabled, and on an unauthenticated
loopback server. With only a static HTTP token configured, it requires that
token too. Other routes require credentials when global authentication is
configured. Explicit OTLP and sync route tokens take precedence over global
credentials on their respective routes.

On the Pi, the Signal K plugin keeps Lume HTTP on loopback and accesses it
through the Signal K proxy. Open the plugin through Signal K rather than
exposing Lume's HTTP port. Grafana's separately configured PostgreSQL listener
may use the host's Docker bridge address; that does not move HTTP off loopback.

Refer to hosts by name, such as `halos.local`, instead of hard-coding a host IP
in client URLs. The bind address selects a local interface; the hostname is
what clients use to reach the host. A wildcard bind exposes every interface,
so restrict access with your firewall and allowlist.
