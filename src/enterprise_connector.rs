//! Connector-side enterprise enrollment and authorization checks.
//!
//! The computer's persistent key is bound to one Okta user (issuer + subject) by
//! an interactive Device Authorization sign-in. The binding and its refresh token
//! live in private connector state, separate from deployment policy and from
//! browser pairings. The connector re-proves the owner to Okta and to the relay
//! on a schedule, and independently checks every relay grant against that owner.
use crate::{
    enterprise_config::Policy,
    enterprise_grant::{self, Audit, Expected, Grant},
    enterprise_oidc::{Failure, Oidc, Principal},
    relay_crypto,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Stored {
    schema_version: u8,
    issuer: String,
    client_id: String,
    subject: String,
    refresh_token: String,
}
/// The owner's most recent proof, or the reason there is none.
#[derive(Clone)]
pub enum Proof {
    Current {
        id_token: String,
        obtained: Instant,
    },
    /// Okta definitively refused: the enrollment is gone until a new sign-in.
    Revoked,
}
pub struct Enrollment {
    pub policy: Policy,
    pub owner: Principal,
    pub audit: Audit,
    oidc: Oidc,
    path: PathBuf,
    refresh_token: tokio::sync::Mutex<String>,
    proof: watch::Sender<Proof>,
    /// Grant identifiers seen recently; a repeated grant is a replay.
    seen: Mutex<HashMap<String, Instant>>,
}

/// Ask the relay what it enforces. Its answer is authoritative for a connector
/// without local policy, and must agree with local policy when there is one.
pub async fn resolve_policy(
    relay_url: &str,
    mode_path: &str,
    local: Option<Policy>,
    allow_http_loopback: bool,
) -> Result<Option<Policy>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = format!("{relay_url}{mode_path}");
    let mut delay = 1;
    let advertised = loop {
        // Without the relay's answer the required mode is unknown: wait, never guess.
        let result = async {
            let response = http.get(&url).send().await?;
            ensure!(response.status().is_success(), "unexpected status");
            let bytes = response.bytes().await?;
            ensure!(bytes.len() <= 65536, "mode response too large");
            Ok::<Value, anyhow::Error>(serde_json::from_slice(&bytes)?)
        }
        .await;
        match result {
            Ok(mode) if mode["mode"] == "relay" => break mode["enterprise"].clone(),
            Ok(_) => bail!("{relay_url} is not a Herdr relay"),
            Err(_) => {
                eprintln!("Relay unreachable; retrying before deciding the access mode.");
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(30);
            }
        }
    };
    match local {
        Some(policy) => {
            ensure!(
                policy.relay_url == relay_url,
                "enterprise policy in {} names relay {}, not {relay_url}",
                policy.source.display(),
                policy.relay_url
            );
            policy.ensure_matches_relay(&advertised)?;
            Ok(Some(policy))
        }
        None if advertised["required"] == true => Ok(Some(Policy::from_relay(
            relay_url,
            &advertised,
            allow_http_loopback,
        )?)),
        None => Ok(None),
    }
}

impl Enrollment {
    /// Loads the existing binding or runs the Okta device sign-in. `announce`
    /// receives the sign-in instructions; nothing else here prints.
    pub async fn open(
        policy: Policy,
        state_dir: &Path,
        announce: impl FnOnce(&crate::enterprise_oidc::DevicePrompt),
    ) -> Result<Arc<Self>> {
        let oidc = Oidc::discover(&policy, None).await?;
        let path = state_dir.join("enterprise.json");
        let audit = Audit::file(&state_dir.join("enterprise-audit.log"));
        let stored = match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let stored: Stored =
                    serde_json::from_slice(&relay_crypto::read_private(&path, 16384)?)
                        .context("enterprise enrollment state is damaged")?;
                // The binding is to one identity provider and application, for good.
                ensure!(
                    stored.schema_version == 1
                        && stored.issuer == policy.issuer
                        && stored.client_id == policy.client_id,
                    "this computer is enrolled with a different Okta issuer or application; \
                     start once with --reset-pairings to enroll again (revokes every paired browser)"
                );
                Some(stored)
            }
            Err(_) => None,
        };
        let mut login = None;
        let current = match &stored {
            Some(stored) => match oidc.refresh(&stored.refresh_token).await {
                Ok(tokens) => Some(tokens),
                Err(Failure::Rejected) => None,
                Err(Failure::Unavailable) => {
                    bail!(
                        "Okta is unreachable; the connector stays offline until it can verify its owner"
                    )
                }
            },
            None => None,
        };
        let tokens = match current {
            Some(tokens) => tokens,
            None => {
                let tokens = oidc.device_login(announce).await?;
                login = Some(());
                tokens
            }
        };
        let owner = tokens.identity.principal.clone();
        if let Some(stored) = &stored {
            ensure!(
                stored.subject == owner.sub,
                "this computer is enrolled to a different corporate user; only that user may sign in. \
                 Start once with --reset-pairings to enroll again (revokes every paired browser)"
            );
        }
        let refresh_token = tokens
            .refresh_token
            .or(stored.map(|s| s.refresh_token).filter(|_| login.is_none()))
            .context(
                "Okta returned no refresh token; enable the Refresh Token grant and offline_access for this application",
            )?;
        let this = Arc::new(Self {
            proof: watch::channel(Proof::Current {
                id_token: tokens.id_token,
                obtained: Instant::now(),
            })
            .0,
            refresh_token: tokio::sync::Mutex::new(refresh_token),
            oidc,
            path,
            owner,
            policy,
            audit,
            seen: Default::default(),
        });
        this.persist().await?;
        if login.is_some() {
            this.audit.log(
                "enrollment",
                json!({"sub":this.owner.sub,"iss":this.owner.iss}),
            );
        }
        Ok(this)
    }
    pub fn reset(state_dir: &Path) -> Result<()> {
        match std::fs::remove_file(state_dir.join("enterprise.json")) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
    async fn persist(&self) -> Result<()> {
        relay_crypto::write_private(
            &self.path,
            &serde_json::to_vec(&Stored {
                schema_version: 1,
                issuer: self.owner.iss.clone(),
                client_id: self.policy.client_id.clone(),
                subject: self.owner.sub.clone(),
                refresh_token: self.refresh_token.lock().await.clone(),
            })?,
        )
    }
    pub fn proofs(&self) -> watch::Receiver<Proof> {
        self.proof.subscribe()
    }
    fn recheck(&self) -> Duration {
        Duration::from_secs(self.policy.recheck_seconds)
    }
    /// A proof older than the recheck bound is no proof.
    pub fn current_token(&self) -> Result<String> {
        match &*self.proof.borrow() {
            Proof::Current { id_token, obtained } if obtained.elapsed() < self.recheck() => {
                Ok(id_token.clone())
            }
            Proof::Current { .. } => bail!("Okta owner verification is out of date"),
            Proof::Revoked => bail!("Okta enrollment was revoked"),
        }
    }
    /// Process-lifetime worker. Returns when Okta revokes the enrollment.
    pub async fn maintain(self: Arc<Self>) {
        loop {
            tokio::time::sleep(self.recheck() / 2).await;
            // Retry an outage briskly inside the bound instead of waiting a full period.
            loop {
                let token = self.refresh_token.lock().await.clone();
                match self.oidc.refresh(&token).await {
                    Ok(tokens) if tokens.identity.principal == self.owner => {
                        if let Some(next) = tokens.refresh_token {
                            *self.refresh_token.lock().await = next;
                            let _ = self.persist().await;
                        }
                        self.proof.send_replace(Proof::Current {
                            id_token: tokens.id_token,
                            obtained: Instant::now(),
                        });
                        self.audit
                            .log("authorization-renewed", json!({"sub":self.owner.sub}));
                        break;
                    }
                    Ok(_) | Err(Failure::Rejected) => {
                        self.audit.log(
                            "authorization-revoked",
                            json!({"sub":self.owner.sub,"reason":"okta-rejected"}),
                        );
                        self.proof.send_replace(Proof::Revoked);
                        return;
                    }
                    Err(Failure::Unavailable) => {
                        self.audit.log(
                            "authorization-renewal-failed",
                            json!({"sub":self.owner.sub,"reason":"okta-unavailable"}),
                        );
                        // Wake the connection so it can notice a proof that went stale.
                        self.proof.send_modify(|_| {});
                        tokio::time::sleep(Duration::from_secs(15)).await;
                    }
                }
            }
        }
    }

    /// Checks a relay grant against what this computer knows on its own: its
    /// enrolled owner, its identity, the relay context and the session. Returns
    /// the grant and how long the session may run before the next renewal.
    pub fn verify_grant(
        &self,
        grant_key: &str,
        value: &Value,
        machine_id: &str,
        context: &str,
        session_id: &str,
        browser_key: Option<&str>,
    ) -> Result<(Grant, Duration)> {
        let (grant, lifetime) = enterprise_grant::verify(
            value,
            &Expected {
                key: grant_key,
                owner: &self.owner,
                machine_id,
                context,
                session_id,
                browser_key,
            },
            enterprise_grant::now(),
        )?;
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, at| at.elapsed() < Duration::from_secs(2 * enterprise_grant::MAX_LIFETIME));
        ensure!(
            seen.len() < 4096 && seen.insert(grant.jti.clone(), Instant::now()).is_none(),
            "grant replayed"
        );
        Ok((grant, lifetime))
    }
}

/// Per-session authorization clock. The session ends when it runs out.
pub struct GrantClock {
    pub browser_key: String,
    pub sub: String,
    deadline: watch::Sender<Instant>,
}
impl GrantClock {
    pub fn new(grant: &Grant, lifetime: Duration) -> Self {
        Self {
            browser_key: grant.browser_key.clone(),
            sub: grant.sub.clone(),
            deadline: watch::channel(Instant::now() + lifetime).0,
        }
    }
    pub fn renew(&self, lifetime: Duration) {
        self.deadline.send_replace(Instant::now() + lifetime);
    }
    pub async fn expired(&self) {
        let mut deadline = self.deadline.subscribe();
        loop {
            let at = *deadline.borrow_and_update();
            tokio::select! {
                _ = tokio::time::sleep_until(at.into()) => {
                    if *deadline.borrow() <= Instant::now() { return; }
                }
                changed = deadline.changed() => { if changed.is_err() { return; } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enterprise_oidc::mock::Idp;
    use std::os::unix::fs::PermissionsExt;
    fn state() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    async fn enroll(idp: &Arc<Idp>, dir: &Path, sub: &'static str) -> Result<Arc<Enrollment>> {
        let approver = idp.clone();
        let task = tokio::spawn(async move {
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                approver.approve_devices(sub);
            }
        });
        let result = Enrollment::open(idp.policy("https://relay.example"), dir, |_| {}).await;
        task.abort();
        result
    }
    #[tokio::test]
    async fn enrollment_binds_one_owner_and_survives_restart_without_a_new_login() {
        let idp = Idp::start().await;
        let dir = state();
        let first = enroll(&idp, dir.path(), "user-a").await.unwrap();
        assert_eq!(first.owner.sub, "user-a");
        assert!(first.current_token().is_ok());
        let stored = std::fs::metadata(dir.path().join("enterprise.json")).unwrap();
        assert_eq!(stored.permissions().mode() & 0o777, 0o600);
        // No device approval is offered now: the stored binding must be enough.
        let again = Enrollment::open(idp.policy("https://relay.example"), dir.path(), |_| {
            panic!("unexpected sign-in prompt")
        })
        .await
        .unwrap();
        assert_eq!(again.owner, first.owner);
        // After Okta revokes it, only the same user may sign in again.
        idp.revoke("user-a");
        assert!(enroll(&idp, dir.path(), "user-b").await.is_err());
        assert_eq!(
            enroll(&idp, dir.path(), "user-a").await.unwrap().owner,
            first.owner
        );
        // A changed issuer/client never silently re-binds the computer.
        let mut other = idp.policy("https://relay.example");
        other.client_id = "another-client".into();
        assert!(Enrollment::open(other, dir.path(), |_| {}).await.is_err());
        // An explicit reset allows a fresh enrollment.
        Enrollment::reset(dir.path()).unwrap();
        assert_eq!(
            enroll(&idp, dir.path(), "user-b").await.unwrap().owner.sub,
            "user-b"
        );
    }
    #[tokio::test]
    async fn outage_at_startup_fails_closed() {
        let idp = Idp::start().await;
        let dir = state();
        enroll(&idp, dir.path(), "user-a").await.unwrap();
        idp.inner.lock().unwrap().outage = true;
        assert!(
            Enrollment::open(idp.policy("https://relay.example"), dir.path(), |_| {})
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn grants_for_other_users_computers_or_replays_are_refused() {
        let idp = Idp::start().await;
        let dir = state();
        let enrollment = enroll(&idp, dir.path(), "user-a").await.unwrap();
        let relay = relay_crypto::Identity::ephemeral();
        let browser = relay_crypto::Identity::ephemeral().public_key();
        let key = relay.public_key();
        let make = |sub: &str, machine: &str, session: &str| {
            Grant::new(
                &Principal {
                    iss: idp.issuer.clone(),
                    sub: sub.into(),
                },
                machine,
                &browser,
                "https://relay.example",
                session,
                300,
            )
            .sign(&relay)
        };
        let check = |grant: &Value, session: &str| {
            enrollment.verify_grant(
                &key,
                grant,
                "machine",
                "https://relay.example",
                session,
                None,
            )
        };
        let good = make("user-a", "machine", "s1");
        let (grant, lifetime) = check(&good, "s1").unwrap();
        assert_eq!(lifetime, Duration::from_secs(300));
        assert_eq!(grant.browser_key, browser);
        // Exactly the same grant again is a replay.
        assert!(check(&good, "s1").is_err());
        assert!(check(&make("user-b", "machine", "s2"), "s2").is_err());
        assert!(check(&make("user-a", "other-machine", "s3"), "s3").is_err());
        assert!(check(&make("user-a", "machine", "s4"), "s5").is_err());
        let forger = relay_crypto::Identity::ephemeral();
        let forged = Grant::new(
            &enrollment.owner,
            "machine",
            &browser,
            "https://relay.example",
            "s6",
            300,
        )
        .sign(&forger);
        assert!(check(&forged, "s6").is_err());
        assert!(check(&json!({}), "s7").is_err());
    }
    #[tokio::test]
    async fn session_clock_expires_unless_renewed() {
        let grant = Grant::new(
            &Principal {
                iss: "i".into(),
                sub: "s".into(),
            },
            "m",
            "b",
            "c",
            "sid",
            60,
        );
        let clock = Arc::new(GrantClock::new(&grant, Duration::from_millis(400)));
        let waiter = clock.clone();
        let expired = tokio::spawn(async move { waiter.expired().await });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!expired.is_finished());
        clock.renew(Duration::from_millis(400));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!expired.is_finished());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(expired.is_finished());
    }
    #[tokio::test]
    async fn relay_mode_decides_and_local_policy_cannot_be_downgraded() {
        use axum::{Json, Router, routing::get};
        let idp = Idp::start().await;
        let serve = |body: Value| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route("/api/mode", get(move || async move { Json(body) }));
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            url
        };
        let plain = serve(json!({"mode":"relay","enterprise":null})).await;
        let local = |relay: &str| idp.policy(relay);
        let enterprise =
            serve(json!({"mode":"relay","enterprise":local("x").advertisement()})).await;
        assert!(
            resolve_policy(&plain, "/api/mode", None, true)
                .await
                .unwrap()
                .is_none()
        );
        let adopted = resolve_policy(&enterprise, "/api/mode", None, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(adopted.issuer, idp.issuer);
        assert!(
            resolve_policy(&enterprise, "/api/mode", Some(local(&enterprise)), true)
                .await
                .unwrap()
                .is_some()
        );
        // Local policy demands Okta: a relay without it, or another relay, is refused.
        assert!(
            resolve_policy(&plain, "/api/mode", Some(local(&plain)), true)
                .await
                .is_err()
        );
        assert!(
            resolve_policy(
                &enterprise,
                "/api/mode",
                Some(local("http://127.0.0.1:1")),
                true
            )
            .await
            .is_err()
        );
        let mut other = local(&enterprise);
        other.issuer = "https://evil.okta.com".into();
        assert!(
            resolve_policy(&enterprise, "/api/mode", Some(other), true)
                .await
                .is_err()
        );
        let not_relay = serve(json!({"mode":"local"})).await;
        assert!(
            resolve_policy(&not_relay, "/api/mode", None, true)
                .await
                .is_err()
        );
    }
}
