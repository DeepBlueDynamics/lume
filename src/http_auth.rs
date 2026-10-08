//! HTTP bearer authentication with static tokens and nuts.services verification.
use std::{path::Path, sync::Arc};

/// Startup credential, deliberately without Debug or a public token accessor.
#[derive(Clone)]
pub struct HttpBearer {
    token: Option<Arc<str>>,
    nuts: Option<Arc<crate::nuts_auth::NutsAuth>>,
}

impl HttpBearer {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read HTTP token file: {e}"))?;
        let token = text.trim();
        if token.is_empty() {
            return Err("HTTP token file is empty".into());
        }
        Ok(Self {
            token: Some(Arc::from(token)),
            nuts: None,
        })
    }

    pub fn from_args(args: &[String]) -> Result<Option<Self>, String> {
        let mut positions = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--http-token-file");
        let Some((i, _)) = positions.next() else {
            return Ok(None);
        };
        if positions.next().is_some() {
            return Err("--http-token-file must be specified only once".into());
        }
        let path = args
            .get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .ok_or("--http-token-file requires a path")?;
        Self::from_file(Path::new(path)).map(Some)
    }

    /// Nuts and a static token may coexist; either grants access, while route
    /// credentials override both. The static token retains its full-access role.
    pub fn from_server_args(args: &[String], root: &Path) -> Result<Option<Self>, String> {
        let mut auth = Self::from_args(args)?;
        if let Some(config) = crate::nuts_auth::NutsConfig::from_args(args)? {
            let nuts = crate::nuts_auth::NutsAuth::open(config, root)?;
            let value = auth.get_or_insert(Self {
                token: None,
                nuts: None,
            });
            value.nuts = Some(nuts);
        }
        Ok(auth)
    }

    pub(crate) fn public_health(&self) -> bool {
        self.nuts.is_some()
    }

    #[cfg(test)]
    pub(crate) fn accepts(&self, headers: &str, override_token: Option<&str>) -> bool {
        self.accepts_scope(headers, override_token, "read")
    }

    pub(crate) fn accepts_scope(
        &self,
        headers: &str,
        override_token: Option<&str>,
        scope: &str,
    ) -> bool {
        let mut authorization = headers.lines().filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("authorization")
                .then_some(value.trim())
        });
        let Some(value) = authorization.next() else {
            return false;
        };
        // Ambiguous credentials are rejected rather than choosing one header.
        if authorization.next().is_some() {
            return false;
        }
        let Some((scheme, token)) = value.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("bearer") {
            return false;
        }
        let token = token.trim();
        if let Some(expected) = override_token {
            return constant_time_eq(token.as_bytes(), expected.as_bytes());
        }
        self.token
            .as_ref()
            .is_some_and(|expected| constant_time_eq(token.as_bytes(), expected.as_bytes()))
            || self
                .nuts
                .as_ref()
                .is_some_and(|nuts| nuts.accepts(token, scope))
    }
}

/// Fail before binding or starting ingest; plain serve's existing bind is retained.
pub fn validate_bind(bind: &str, authenticated: bool) -> Result<(), String> {
    let ip = bind
        .parse::<std::net::IpAddr>()
        .map_err(|_| "Invalid HTTP bind IP address")?;
    if !ip.is_loopback() && !authenticated {
        return Err(
            "Non-loopback HTTP requires --nuts-auth with --nuts-allow or --http-token-file".into(),
        );
    }
    Ok(())
}

// Work depends on lengths, never the first mismatching byte. black_box keeps the
// optimizer from replacing the XOR fold with an early-exit equality comparison.
fn constant_time_eq(client: &[u8], expected: &[u8]) -> bool {
    let mut difference = client.len() ^ expected.len();
    for i in 0..client.len().max(expected.len()) {
        let a = client.get(i).copied().unwrap_or(0);
        let b = expected.get(i).copied().unwrap_or(0);
        difference |= usize::from(std::hint::black_box(a) ^ std::hint::black_box(b));
    }
    std::hint::black_box(difference) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_checks_every_byte_and_length() {
        for value in ["", "s", "secreu", "tecret", "secret-long", "secret\0"] {
            assert!(!constant_time_eq(value.as_bytes(), b"secret"));
        }
        assert!(constant_time_eq(b"secret", b"secret"));
        let auth = HttpBearer {
            token: Some(Arc::from("secret")),
            nuts: None,
        };
        assert!(auth.accepts("Authorization: Bearer secret", None));
        assert!(auth.accepts("authorization: bEaReR secret", None));
        assert!(!auth.accepts("Authorization: Bearer wrong", None));
        assert!(!auth.accepts("Authorization: Basic secret", None));
        assert!(!auth.accepts(
            "Authorization: Bearer secret\nAuthorization: Bearer secret",
            None
        ));
        assert!(auth.accepts("Authorization: Bearer exporter", Some("exporter")));
        assert!(!auth.accepts("Authorization: Bearer secret", Some("exporter")));
    }

    #[test]
    fn token_files_are_trimmed_nonempty_and_errors_do_not_echo_content() {
        let root = std::env::temp_dir().join(format!("http-token-{}", crate::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        std::fs::write(&path, " \n secret \r\n").unwrap();
        let auth = HttpBearer::from_file(&path).unwrap();
        assert!(auth.accepts("Authorization: Bearer secret", None));
        std::fs::write(&path, " \n\t").unwrap();
        assert_eq!(
            HttpBearer::from_file(&path).err().unwrap(),
            "HTTP token file is empty"
        );
        // The running listener keeps its startup snapshot despite file changes.
        assert!(auth.accepts("Authorization: Bearer secret", None));
        std::fs::write(&path, b"private-content\xff").unwrap();
        let error = HttpBearer::from_file(&path).err().unwrap();
        assert!(!error.contains("private-content"));
        assert!(HttpBearer::from_args(&["--http-token-file".into()]).is_err());
        assert!(HttpBearer::from_args(&["--http-token-file".into(), "--bind".into()]).is_err());
        assert!(HttpBearer::from_args(&[
            "--http-token-file".into(),
            "x".into(),
            "--http-token-file".into(),
            "y".into()
        ])
        .is_err());
        assert!(HttpBearer::from_args(&[]).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
