//! OTLP CLI entry points; the receiver shares the TI HTTP server.
use std::path::Path;
pub fn token(args: &[String]) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == "--otlp-token-file") else {
        return Ok(None);
    };
    let path = args
        .get(i + 1)
        .filter(|s| !s.starts_with("--"))
        .ok_or("--otlp-token-file requires a path")?;
    let token =
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read OTLP token file: {e}"))?;
    if token.trim().is_empty() {
        return Err("OTLP token file is empty".into());
    }
    Ok(Some(token.trim().to_string()))
}
pub fn validate_bind(bind: &str, token: &Option<String>) -> Result<(), String> {
    let ip: std::net::IpAddr = bind
        .parse()
        .map_err(|_| "OTLP bind must be an IP address")?;
    if !ip.is_loopback() && token.is_none() {
        return Err("Non-loopback OTLP requires --otlp-token-file".into());
    }
    Ok(())
}
pub fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: lume ti otlp --store <root> [--bind 127.0.0.1] [--port 4318] [--otlp-token-file <path>]\nOTLP HTTP/JSON only: POST /v1/metrics and /v1/logs; every other route returns 404. Bearer authentication is required for a non-loopback bind.");
        return Ok(());
    }
    let mut root = None;
    let mut bind = "127.0.0.1";
    let mut port = 4318u16;
    let mut i = 0;
    while i < args.len() {
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{} requires a value", args[i]))?;
        match args[i].as_str() {
            "--store" => root = Some(Path::new(value)),
            "--bind" => bind = value,
            "--port" => port = value.parse().map_err(|_| "Invalid OTLP port")?,
            "--otlp-token-file" => {}
            other => return Err(format!("Unknown OTLP argument: {other}")),
        }
        i += 2;
    }
    let token = token(args)?;
    validate_bind(bind, &token)?;
    let server =
        crate::ti_http::TiServer::open_with_width(root.ok_or("--store is required")?, Some(10))?
            .with_otlp_only(token)?;
    crate::agent::serve_with_ti_server(port, std::sync::Arc::new(server), bind, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_non_loopback_requires_auth_without_binding_any_socket() {
        assert!(validate_bind("127.0.0.1", &None).is_ok());
        assert!(validate_bind("127.0.0.2", &None).is_ok());
        assert!(validate_bind("::1", &None).is_ok());
        assert!(validate_bind("192.0.2.1", &None).is_err());
        assert!(validate_bind("192.0.2.1", &Some("token".into())).is_ok());
        assert!(validate_bind("localhost", &None).is_err());
    }
    #[test]
    fn standalone_rejects_missing_values_and_unknown_arguments() {
        assert!(run(&["--store".into()]).is_err());
        assert!(run(&["--unknown".into(), "value".into()]).is_err());
        assert!(run(&["--port".into(), "not-a-port".into()]).is_err());
        assert!(run(&[]).is_err());
        assert!(token(&["--otlp-token-file".into()]).is_err());
    }
}
