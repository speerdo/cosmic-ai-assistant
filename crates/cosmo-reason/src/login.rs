//! Signing in with the browser, for providers that let third-party apps do
//! it (`cosmo auth-login`).
//!
//! Only **OpenRouter** does, today: its OAuth PKCE flow is published for
//! apps and returns an ordinary, user-controlled API key, which is then
//! stored exactly as a pasted one would be. The others either have no such
//! flow (OpenAI API keys, Z.ai, OpenCode Go, Ollama Cloud) or forbid third
//! parties from using their logins (Anthropic's Claude.ai sign-in), so for
//! them `auth-login` opens the key page and the key is pasted.
//!
//! The flow: a one-off listener on `localhost` (any port, which OpenRouter
//! allows), the browser sent to the provider's sign-in page with a PKCE
//! challenge and a random `state`, the provider redirecting back with a
//! code, and the code exchanged, with the verifier, for the key.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(300);

/// A provider's browser sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserLogin {
    /// Where the browser is sent.
    pub authorize_url: &'static str,
    /// Where the code is exchanged for a key.
    pub exchange_url: &'static str,
}

/// OpenRouter's OAuth PKCE (openrouter.ai/docs, "OAuth PKCE").
pub const OPENROUTER: BrowserLogin = BrowserLogin {
    authorize_url: "https://openrouter.ai/auth",
    exchange_url: "https://openrouter.ai/api/v1/auth/keys",
};

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("couldn't listen for the browser's return: {0}")]
    Listen(std::io::Error),
    #[error("the browser didn't come back within {} minutes", WAIT.as_secs() / 60)]
    TimedOut,
    #[error("sign-in was cancelled or refused: {0}")]
    Refused(String),
    #[error("the sign-in response didn't match this request (state mismatch); try again")]
    StateMismatch,
    #[error("exchanging the sign-in code failed: {0}")]
    Exchange(String),
}

/// A PKCE verifier and its S256 challenge (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        Self::from_verifier(random_token(32))
    }

    fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// `n` random bytes, base64url: 43 characters for 32 bytes.
fn random_token(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("the OS random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The browser's destination.
fn authorize_url(login: &BrowserLogin, callback: &str, pkce: &Pkce, state: &str) -> Url {
    let mut url = Url::parse(login.authorize_url).expect("a valid authorize URL");
    url.query_pairs_mut()
        .append_pair("callback_url", callback)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("key_label", "cosmo")
        .append_pair("state", state);
    url
}

/// What came back on the callback path.
#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code {
        code: String,
        state: Option<String>,
    },
    Error(String),
    /// Some other request (a favicon): ignored.
    Other,
}

/// Parse the request line of the browser's return.
fn parse_request(head: &str) -> Callback {
    let Some(target) = head
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("GET "))
        .and_then(|l| l.split(' ').next())
    else {
        return Callback::Other;
    };
    let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
        return Callback::Other;
    };
    if url.path() != "/callback" {
        return Callback::Other;
    }
    let get = |k: &str| {
        url.query_pairs()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.into_owned())
    };
    match (get("code"), get("error")) {
        (Some(code), _) => Callback::Code {
            code,
            state: get("state"),
        },
        (None, Some(e)) => Callback::Error(e),
        (None, None) => Callback::Error("no code in the response".into()),
    }
}

/// Sign in through the browser and return the provider's API key.
/// `open` is handed the URL to show (it opens the browser; the caller may
/// also print it).
pub async fn browser_login(
    login: &BrowserLogin,
    open: impl FnOnce(&str),
) -> Result<String, LoginError> {
    let (url, pending) = start(login).await?;
    open(&url);
    pending.finish().await
}

/// A sign-in waiting for the browser to come back.
pub struct PendingLogin {
    login: BrowserLogin,
    v4: TcpListener,
    v6: Option<TcpListener>,
    pkce: Pkce,
    state: String,
}

/// The first half: listen, and return the URL to send the browser to. The
/// daemon hands that URL to whichever client asked (the applet opens it),
/// then waits with [`PendingLogin::finish`].
pub async fn start(login: &BrowserLogin) -> Result<(String, PendingLogin), LoginError> {
    let v4 = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(LoginError::Listen)?;
    let port = v4.local_addr().map_err(LoginError::Listen)?.port();
    // `localhost` may resolve to ::1 first; listen there too when we can.
    let v6 = TcpListener::bind(("::1", port)).await.ok();
    let callback = format!("http://localhost:{port}/callback");
    let pkce = Pkce::new();
    let state = random_token(16);
    let url = authorize_url(login, &callback, &pkce, &state).to_string();
    Ok((
        url,
        PendingLogin {
            login: *login,
            v4,
            v6,
            pkce,
            state,
        },
    ))
}

impl PendingLogin {
    /// The second half: wait for the browser (5 minutes at most), check
    /// it, and exchange the code for the key.
    pub async fn finish(self) -> Result<String, LoginError> {
        let Self {
            login,
            v4,
            v6,
            pkce,
            state,
        } = self;
        let code = tokio::time::timeout(WAIT, async {
            loop {
                let accepted = match &v6 {
                    Some(v6) => tokio::select! {
                        a = v4.accept() => a,
                        a = v6.accept() => a,
                    },
                    None => v4.accept().await,
                };
                let Ok((mut stream, _)) = accepted else {
                    continue;
                };
                match parse_request(&read_head(&mut stream).await) {
                    Callback::Other => respond(&mut stream, "404 Not Found", "").await,
                    Callback::Error(e) => {
                        respond(&mut stream, "200 OK", PAGE_FAILED).await;
                        return Err(LoginError::Refused(e));
                    }
                    Callback::Code { code, state: got } => {
                        // OpenRouter returns `state` unchanged; a missing
                        // one is accepted (PKCE still binds the code to
                        // this verifier), a different one is not.
                        if got.as_deref().is_some_and(|s| s != state) {
                            respond(&mut stream, "200 OK", PAGE_FAILED).await;
                            return Err(LoginError::StateMismatch);
                        }
                        respond(&mut stream, "200 OK", PAGE_DONE).await;
                        return Ok(code);
                    }
                }
            }
        })
        .await
        .map_err(|_| LoginError::TimedOut)??;
        exchange(&login, &code, &pkce).await
    }
}

/// The code and verifier, for the key.
async fn exchange(login: &BrowserLogin, code: &str, pkce: &Pkce) -> Result<String, LoginError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        key: String,
    }
    let resp = reqwest::Client::new()
        .post(login.exchange_url)
        .timeout(Duration::from_secs(30))
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": pkce.verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|e| LoginError::Exchange(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        // An error body carries no key; keep it short for the message.
        let body: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        return Err(LoginError::Exchange(format!("{status}: {body}")));
    }
    let reply: Reply = resp
        .json()
        .await
        .map_err(|e| LoginError::Exchange(format!("unexpected reply: {e}")))?;
    if reply.key.trim().is_empty() {
        return Err(LoginError::Exchange("the reply had an empty key".into()));
    }
    Ok(reply.key)
}

/// The request head, up to 8 KB or its blank line.
async fn read_head(stream: &mut TcpStream) -> String {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read = async {
        while buf.len() < 8192 {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), read).await;
    String::from_utf8_lossy(&buf).into_owned()
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

const PAGE_DONE: &str = "<!doctype html><meta charset=utf-8><title>Cosmo</title>\
<body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">\
<h1>Cosmo is signed in</h1><p>You can close this tab and go back to Cosmo.</p>";

const PAGE_FAILED: &str = "<!doctype html><meta charset=utf-8><title>Cosmo</title>\
<body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">\
<h1>Sign-in didn't complete</h1><p>Go back to the terminal to see why, and try again.</p>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_an_independent_computation() {
        // Challenge computed with Python's hashlib + base64.urlsafe_b64encode.
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mJ92K9qq7O74Tmm8EK06z-UDEP_RNo".into());
        assert_eq!(p.challenge, "Jv1xIq93pEk9GrNWRgeC3FzxRjeqFwK3fqqy3Rp-kRw");
        let fresh = Pkce::new();
        assert_eq!(fresh.verifier.len(), 43, "RFC 7636: 43–128 characters");
        assert_ne!(fresh.verifier, Pkce::new().verifier);
    }

    #[test]
    fn the_authorize_url_carries_the_challenge_and_an_encoded_callback() {
        let p = Pkce::from_verifier("v".into());
        let url = authorize_url(&OPENROUTER, "http://localhost:4321/callback", &p, "st");
        let q: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("callback_url"), Some("http://localhost:4321/callback"));
        assert_eq!(get("code_challenge"), Some(p.challenge.as_str()));
        assert_eq!(get("code_challenge_method"), Some("S256"));
        assert_eq!(get("state"), Some("st"));
        assert!(url.as_str().contains("callback_url=http%3A%2F%2Flocalhost"));
    }

    #[test]
    fn callbacks_are_parsed_and_strays_ignored() {
        assert_eq!(
            parse_request("GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: x\r\n\r\n"),
            Callback::Code {
                code: "abc".into(),
                state: Some("xyz".into())
            }
        );
        assert_eq!(
            parse_request("GET /callback?error=access_denied HTTP/1.1\r\n\r\n"),
            Callback::Error("access_denied".into())
        );
        assert_eq!(
            parse_request("GET /favicon.ico HTTP/1.1\r\n\r\n"),
            Callback::Other
        );
        assert_eq!(parse_request("garbage"), Callback::Other);
    }

    /// The whole flow against a fake provider: the "browser" follows the
    /// URL back to the callback, and the exchange gets the right verifier.
    #[tokio::test]
    async fn browser_login_round_trip() {
        let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let exchange_url = format!("http://{}/api/v1/auth/keys", provider.local_addr().unwrap());
        let seen_body = tokio::spawn(async move {
            let (mut s, _) = provider.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if let Some((_, body)) = text.split_once("\r\n\r\n")
                    && body.ends_with('}')
                {
                    break;
                }
            }
            let reply = r#"{"key":"sk-or-test"}"#;
            s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            String::from_utf8_lossy(&got).into_owned()
        });
        let login = BrowserLogin {
            authorize_url: "https://openrouter.example/auth",
            exchange_url: Box::leak(exchange_url.into_boxed_str()),
        };
        let challenge = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = challenge.clone();
        let key = browser_login(&login, move |url| {
            let url = Url::parse(url).unwrap();
            let get = |k: &str| {
                url.query_pairs()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.into_owned())
                    .unwrap()
            };
            *seen.lock().unwrap() = get("code_challenge");
            let back = format!(
                "{}?code=the-code&state={}",
                get("callback_url"),
                get("state")
            )
            .replace("localhost", "127.0.0.1");
            tokio::spawn(async move { reqwest::get(back).await.unwrap().text().await.unwrap() });
        })
        .await
        .unwrap();
        assert_eq!(key, "sk-or-test");
        let request = seen_body.await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["code"], "the-code");
        let verifier = body["code_verifier"].as_str().unwrap();
        assert_eq!(
            Pkce::from_verifier(verifier.into()).challenge,
            *challenge.lock().unwrap(),
            "the verifier sent matches the challenge the browser was given"
        );
    }

    #[tokio::test]
    async fn a_wrong_state_is_refused() {
        let key = browser_login(&OPENROUTER, |url| {
            let url = Url::parse(url).unwrap();
            let cb = url
                .query_pairs()
                .find(|(n, _)| n == "callback_url")
                .map(|(_, v)| v.into_owned())
                .unwrap()
                .replace("localhost", "127.0.0.1");
            tokio::spawn(async move {
                let _ = reqwest::get(format!("{cb}?code=c&state=forged")).await;
            });
        })
        .await;
        assert!(matches!(key, Err(LoginError::StateMismatch)), "{key:?}");
    }
}
