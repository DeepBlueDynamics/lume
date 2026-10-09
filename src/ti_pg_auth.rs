//! Verifier-only SCRAM-SHA-256 (RFC 5802/7677), without TLS channel binding.
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::{Sink, SinkExt};
use hmac::{Hmac, KeyInit, Mac};
use pgwire::{
    api::{
        auth::{
            finish_authentication, protocol_negotiation, save_startup_parameters_to_metadata,
            DefaultServerParameterProvider, StartupHandler,
        },
        ClientInfo, PgWireConnectionState, PidSecretKeyGenerator, RandomPidSecretKeyGenerator,
    },
    error::{PgWireError, PgWireResult},
    messages::{
        startup::{Authentication, PasswordMessageFamily},
        PgWireBackendMessage, PgWireFrontendMessage,
    },
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};
use tokio::sync::Mutex;
pub(crate) fn open_private_file(
    path: &std::path::Path,
    desc: &str,
) -> Result<std::fs::File, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open {desc}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file
            .metadata()
            .map_err(|e| format!("Cannot stat {desc}: {e}"))?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(format!(
                "{desc} must not be group- or world-accessible (use chmod 600)"
            ));
        }
    }
    Ok(file)
}

/// Open first, then check that exact file's mode before reading verifier data.
pub(crate) fn load_users(path: &std::path::Path) -> Result<Vec<ti_contracts::ScramUser>, String> {
    use std::io::Read;
    let mut file = open_private_file(path, "pg auth config")?;
    let mut text = String::new();
    file.by_ref()
        .take(65537)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read pg auth config")?;
    if text.len() > 65536 {
        return Err("pg auth config exceeds 64 KiB".into());
    }
    // Do not reflect parse errors: TOML diagnostics can include verifier contents.
    let auth = ti_contracts::AuthConfig::from_toml(&text).map_err(|_| "Invalid pg auth config")?;
    for user in &auth.scram_users {
        Verifier::parse(&user.verifier).map_err(|_| "Invalid SCRAM verifier in pg auth config")?;
    }
    Ok(auth.scram_users)
}
fn denied() -> PgWireError {
    super::ti_pg::error("28P01", "SCRAM authentication failed")
}
#[derive(Clone)]
struct Verifier {
    iterations: u32,
    salt: String,
    stored: [u8; 32],
    server: [u8; 32],
}
impl Verifier {
    fn parse(s: &str) -> Result<Self, String> {
        if s.len() > 512 {
            return Err("SCRAM verifier exceeds 512 bytes".into());
        }
        let s = s
            .strip_prefix("SCRAM-SHA-256$")
            .ok_or("requires SCRAM-SHA-256 verifier")?;
        let (salt, keys) = s.split_once('$').ok_or("malformed SCRAM verifier")?;
        let (iterations, salt) = salt.split_once(':').ok_or("malformed SCRAM verifier")?;
        let iterations: u32 = iterations
            .parse()
            .map_err(|_| "invalid SCRAM iteration count")?;
        if !(4096..=1_000_000).contains(&iterations) {
            return Err("SCRAM iterations must be 4096..=1000000".into());
        }
        let decoded = STANDARD.decode(salt).map_err(|_| "invalid SCRAM salt")?;
        if !(8..=64).contains(&decoded.len()) {
            return Err("SCRAM salt must be 8..=64 bytes".into());
        }
        let (stored, server) = keys.split_once(':').ok_or("malformed SCRAM keys")?;
        let key = |s: &str| -> Result<[u8; 32], String> {
            STANDARD
                .decode(s)
                .map_err(|_| "invalid SCRAM key".to_string())?
                .try_into()
                .map_err(|_| "SCRAM keys must be 32 bytes".into())
        };
        Ok(Self {
            iterations,
            salt: salt.into(),
            stored: key(stored)?,
            server: key(server)?,
        })
    }
    fn fake() -> Self {
        Self {
            iterations: 4096,
            salt: STANDARD.encode(rand::random::<[u8; 16]>()),
            stored: rand::random(),
            server: rand::random(),
        }
    }
}
pub(crate) struct AuthConfig {
    users: BTreeMap<String, Verifier>,
    pub(crate) require_tls: bool,
}
impl AuthConfig {
    pub(crate) fn new(
        users: Vec<ti_contracts::ScramUser>,
        non_loopback: bool,
        require_tls: bool,
    ) -> Result<Arc<Self>, String> {
        if non_loopback && users.is_empty() {
            return Err("Non-loopback Postgres bind requires [auth] scram_users in ti.toml".into());
        }
        let mut parsed = BTreeMap::new();
        for user in users {
            parsed.insert(
                user.username.clone(),
                Verifier::parse(&user.verifier)
                    .map_err(|e| format!("SCRAM user {}: {e}", user.username))?,
            );
        }
        Ok(Arc::new(Self {
            users: parsed,
            require_tls,
        }))
    }
    pub(crate) fn startup(self: &Arc<Self>) -> Arc<Auth> {
        Arc::new(Auth {
            config: self.clone(),
            state: Mutex::new(State::First),
        })
    }
}
enum State {
    First,
    Final {
        verifier: Verifier,
        known: bool,
        bare: String,
        first: String,
        nonce: String,
        channel: String,
    },
    Finished,
}
pub(crate) struct Auth {
    config: Arc<AuthConfig>,
    state: Mutex<State>,
}
fn attributes(s: &str) -> PgWireResult<BTreeMap<char, &str>> {
    if s.len() > 4096 || !s.is_ascii() {
        return Err(denied());
    }
    let mut out = BTreeMap::new();
    for item in s.split(',') {
        let b = item.as_bytes();
        if b.len() < 2 || b[1] != b'=' || out.insert(b[0] as char, &item[2..]).is_some() {
            return Err(denied());
        }
    }
    if out.contains_key(&'m') {
        return Err(denied());
    }
    Ok(out)
}
fn first_message(msg: PasswordMessageFamily) -> PgWireResult<String> {
    let (method, data) = match msg {
        PasswordMessageFamily::Raw(body) => {
            if body.len() > 4096 {
                return Err(denied());
            }
            let pos = body.iter().position(|b| *b == 0).ok_or_else(denied)?;
            let method = std::str::from_utf8(&body[..pos])
                .map_err(|_| denied())?
                .to_string();
            let len: [u8; 4] = body
                .get(pos + 1..pos + 5)
                .ok_or_else(denied)?
                .try_into()
                .map_err(|_| denied())?;
            let len = i32::from_be_bytes(len);
            if len < 0 || len as usize != body.len() - pos - 5 {
                return Err(denied());
            }
            (method, body[pos + 5..].to_vec())
        }
        PasswordMessageFamily::SASLInitialResponse(msg) => {
            (msg.auth_method, msg.data.ok_or_else(denied)?.to_vec())
        }
        _ => return Err(denied()),
    };
    if method != "SCRAM-SHA-256" || data.len() > 4096 {
        return Err(denied());
    }
    String::from_utf8(data).map_err(|_| denied())
}
fn final_message(msg: PasswordMessageFamily) -> PgWireResult<String> {
    let data = match msg {
        PasswordMessageFamily::Raw(body) => body.to_vec(),
        PasswordMessageFamily::SASLResponse(msg) => msg.data.to_vec(),
        _ => return Err(denied()),
    };
    if data.len() > 4096 {
        return Err(denied());
    }
    String::from_utf8(data).map_err(|_| denied())
}
fn mac(key: &[u8], data: &[u8]) -> [u8; 32] {
    Hmac::<Sha256>::new_from_slice(key)
        .expect("HMAC permits any key length")
        .chain_update(data)
        .finalize()
        .into_bytes()
        .into()
}
#[async_trait]
impl StartupHandler for Auth {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let mut parameters = DefaultServerParameterProvider::default();
        parameters.server_version = "16.6-lume".into();
        parameters.default_transaction_read_only = true;
        parameters.is_superuser = false;
        parameters.time_zone = "UTC".into();
        match message {
            PgWireFrontendMessage::Startup(ref startup) => {
                if self.config.require_tls && !client.is_secure() {
                    return Err(super::ti_pg::error(
                        "28000",
                        "TLS connection is required for non-loopback connections. Connect using sslmode=require, or start the server with --pg-allow-plaintext",
                    ));
                }
                protocol_negotiation(client, startup).await?;
                save_startup_parameters_to_metadata(client, startup);
                let generator = RandomPidSecretKeyGenerator::default();
                let (pid, key) = generator.generate(client);
                client.set_pid_and_secret_key(pid, key);
                if self.config.users.is_empty() {
                    return finish_authentication(client, &parameters).await;
                }
                client.set_state(PgWireConnectionState::AuthenticationInProgress);
                client
                    .send(PgWireBackendMessage::Authentication(Authentication::SASL(
                        vec!["SCRAM-SHA-256".into()],
                    )))
                    .await?;
            }
            PgWireFrontendMessage::PasswordMessageFamily(msg) => {
                let mut state = self.state.lock().await;
                match std::mem::replace(&mut *state, State::Finished) {
                    State::First => {
                        let message = first_message(msg)?;
                        let (channel, bare) = if let Some(b) = message.strip_prefix("n,,") {
                            ("biws", b)
                        } else if let Some(b) = message.strip_prefix("y,,") {
                            ("eSws", b)
                        } else {
                            return Err(denied());
                        };
                        let attrs = attributes(bare)?;
                        if attrs.len() != 2 || !attrs.contains_key(&'n') {
                            return Err(denied());
                        }
                        let nonce = attrs.get(&'r').ok_or_else(denied)?;
                        if nonce.len() < 8
                            || nonce.len() > 512
                            || nonce.bytes().any(|b| !(33..=126).contains(&b) || b == b',')
                        {
                            return Err(denied());
                        }
                        let user = client
                            .metadata()
                            .get("user")
                            .map(String::as_str)
                            .unwrap_or("");
                        let known = self.config.users.contains_key(user);
                        let verifier = self
                            .config
                            .users
                            .get(user)
                            .cloned()
                            .unwrap_or_else(Verifier::fake);
                        let nonce =
                            format!("{nonce}{}", STANDARD.encode(rand::random::<[u8; 24]>()));
                        let first =
                            format!("r={nonce},s={},i={}", verifier.salt, verifier.iterations);
                        client
                            .send(PgWireBackendMessage::Authentication(
                                Authentication::SASLContinue(first.clone().into()),
                            ))
                            .await?;
                        *state = State::Final {
                            verifier,
                            known,
                            bare: bare.into(),
                            first,
                            nonce,
                            channel: channel.into(),
                        };
                    }
                    State::Final {
                        verifier,
                        known,
                        bare,
                        first,
                        nonce,
                        channel,
                    } => {
                        let final_text = final_message(msg)?;
                        let attrs = attributes(&final_text)?;
                        if attrs.len() != 3
                            || attrs.get(&'r') != Some(&nonce.as_str())
                            || attrs.get(&'c') != Some(&channel.as_str())
                        {
                            return Err(denied());
                        }
                        let proof = STANDARD
                            .decode(attrs.get(&'p').ok_or_else(denied)?)
                            .map_err(|_| denied())?;
                        if proof.len() != 32 {
                            return Err(denied());
                        }
                        let (without, _) = final_text.rsplit_once(",p=").ok_or_else(denied)?;
                        let auth = format!("{bare},{first},{without}");
                        let signature = mac(&verifier.stored, auth.as_bytes());
                        let mut key = [0; 32];
                        for i in 0..32 {
                            key[i] = proof[i] ^ signature[i];
                        }
                        // RustCrypto CtOutput Eq is constant time, including on aarch64.
                        let recovered = hmac::digest::CtOutput::<Sha256>::new(Sha256::digest(key));
                        let stored = hmac::digest::CtOutput::<Sha256>::new(verifier.stored.into());
                        let valid = recovered == stored;
                        if !valid || !known {
                            return Err(denied());
                        }
                        let final_data = format!(
                            "v={}",
                            STANDARD.encode(mac(&verifier.server, auth.as_bytes()))
                        );
                        client
                            .send(PgWireBackendMessage::Authentication(
                                Authentication::SASLFinal(final_data.into()),
                            ))
                            .await?;
                        finish_authentication(client, &parameters).await?;
                    }
                    State::Finished => return Err(denied()),
                }
            }
            _ => return Err(denied()),
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifier_validation_and_non_loopback_policy() {
        for value in [
            "SCRAM-SHA-256$1:a:b",
            "SCRAM-SHA-256$4096:c2FsdA==:x:y",
            "plaintext",
        ] {
            assert!(Verifier::parse(value).is_err());
        }
        assert!(AuthConfig::new(vec![], true, false).is_err());
        assert!(AuthConfig::new(vec![], false, false).is_ok());
    }
    #[test]
    fn malformed_scram_attributes_rejected() {
        for value in ["r=a,r=b", "m=required,r=a", "r=a,n", "r=é"] {
            assert!(attributes(value).is_err());
        }
    }
}
