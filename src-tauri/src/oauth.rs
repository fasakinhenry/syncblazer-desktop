//! Google sign-in for the desktop app, via the system browser + PKCE
//! (RFC 7636) — the standard OAuth pattern for installed apps, and the one
//! Google's own docs recommend for "Desktop app" type clients. No client
//! secret is used or stored anywhere: PKCE replaces it with a one-time
//! cryptographic proof generated fresh for each sign-in attempt, which is
//! exactly the point — a secret embedded in a downloadable app isn't
//! actually secret, so the correct fix is a flow that never needs one.
//!
//! Flow: generate a random `code_verifier` + its SHA-256 `code_challenge` ->
//! open the real system browser to Google's consent screen -> Google
//! redirects to a loopback address this app is already listening on (via
//! the same relay server used for LAN pairing) -> exchange the resulting
//! code + original verifier for an id_token, directly with Google, no
//! secret involved -> hand the id_token to the frontend, which sends it to
//! our own backend exactly like the website's Google sign-in already does.

use axum::extract::{Query, State};
use axum::response::Html;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

pub type PendingSignIn = oneshot::Sender<Result<String, String>>;

// Keyed by a random `state` value unique to each sign-in attempt (the OAuth
// spec's own mechanism for this exact problem) rather than a single "one
// attempt at a time" slot. A single shared slot meant ANY mismatch between
// when a callback arrives and which attempt is "current" — a duplicate
// request, a second click, a leftover attempt from earlier — silently
// discarded a perfectly valid authorization code with no way to recover.
#[derive(Clone, Default)]
pub struct OAuthState {
    pending: Arc<Mutex<HashMap<String, PendingSignIn>>>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    error: Option<String>,
    state: Option<String>,
}

const SUCCESS_PAGE: &str = "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;text-align:center;padding-top:4rem;color:#0f172a\"><h2>You're signed in.</h2><p>You can close this tab and return to SyncBlaze.</p></body></html>";
const ERROR_PAGE: &str = "<!doctype html><html><body style=\"font-family:system-ui,sans-serif;text-align:center;padding-top:4rem;color:#0f172a\"><h2>Sign-in didn't complete.</h2><p>You can close this tab and try again in SyncBlaze.</p></body></html>";

pub async fn oauth_callback_handler(Query(q): Query<CallbackQuery>, State(state): State<OAuthState>) -> Html<&'static str> {
    let Some(request_state) = q.state else {
        return Html(ERROR_PAGE);
    };

    let tx = {
        let mut pending = state.pending.lock().await;
        pending.remove(&request_state)
    };
    let Some(tx) = tx else {
        // No attempt matching this exact state is waiting — stale/duplicate
        // hit. Whatever attempt IS actually in flight (if any) is untouched.
        return Html(ERROR_PAGE);
    };

    match (q.code, q.error) {
        (Some(code), _) => {
            let _ = tx.send(Ok(code));
            Html(SUCCESS_PAGE)
        }
        (None, Some(error)) => {
            let _ = tx.send(Err(error));
            Html(ERROR_PAGE)
        }
        (None, None) => {
            let _ = tx.send(Err("No authorization code returned".to_string()));
            Html(ERROR_PAGE)
        }
    }
}

fn generate_code_verifier() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..64).map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char).collect()
}

fn code_challenge_from_verifier(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

async fn exchange_code_for_id_token(
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    let res = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Google: {e}"))?;

    if !res.status().is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Google rejected the sign-in: {body}"));
    }

    let parsed: TokenResponse = res.json().await.map_err(|e| format!("Unexpected response from Google: {e}"))?;
    Ok(parsed.id_token)
}

#[tauri::command]
pub async fn start_google_signin(
    client_id: String,
    oauth_state: tauri::State<'_, OAuthState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;

    let verifier = generate_code_verifier();
    let challenge = code_challenge_from_verifier(&verifier);
    let redirect_uri = format!("http://127.0.0.1:{}/oauth/callback", crate::LAN_RELAY_PORT);
    let state_token = generate_code_verifier(); // reused as a random nonce — same shape requirements

    let (tx, rx) = oneshot::channel();
    oauth_state.pending.lock().await.insert(state_token.clone(), tx);

    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&state={}&prompt=select_account",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode("openid email profile"),
        challenge,
        urlencoding::encode(&state_token)
    );

    app.opener()
        .open_url(auth_url, None::<&str>)
        .map_err(|e| format!("Couldn't open your browser: {e}"))?;

    let code = match tokio::time::timeout(std::time::Duration::from_secs(180), rx).await {
        Ok(Ok(Ok(code))) => code,
        Ok(Ok(Err(err))) => return Err(format!("Sign-in was cancelled or failed: {err}")),
        Ok(Err(_)) => return Err("Sign-in was cancelled".to_string()),
        Err(_) => {
            // Nobody's coming back for this attempt — clear it so a stale
            // sender doesn't linger forever, without touching any other
            // attempt that might be in flight under its own state token.
            oauth_state.pending.lock().await.remove(&state_token);
            return Err("Sign-in timed out. Please try again.".to_string());
        }
    };

    exchange_code_for_id_token(&client_id, &code, &verifier, &redirect_uri).await
}
