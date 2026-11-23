//! Telling Mojang's session server we're joining an online-mode server, as
//! vanilla `ClientHandshakePacketListenerImpl.authenticateServer` does.

use sha1::{Digest, Sha1};

/// Vanilla `new BigInteger(Crypt.digestData(serverId, publicKey, secretKey))
/// .toString(16)`: SHA-1 over the server id, shared secret and public key,
/// printed as a signed hex number.
pub fn server_hash(server_id: &str, shared_secret: &[u8], public_key: &[u8]) -> String {
    let mut digest: [u8; 20] = Sha1::new()
        .chain_update(server_id.as_bytes())
        .chain_update(shared_secret)
        .chain_update(public_key)
        .finalize()
        .into();
    let negative = digest[0] & 0x80 != 0;
    if negative {
        // Two's complement negation, so the magnitude prints after the sign.
        let mut carry = true;
        for byte in digest.iter_mut().rev() {
            *byte = !*byte;
            if carry {
                (*byte, carry) = byte.overflowing_add(1);
            }
        }
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let hex = hex.trim_start_matches('0');
    match (negative, hex.is_empty()) {
        (_, true) => "0".into(),
        (true, false) => format!("-{hex}"),
        (false, false) => hex.into(),
    }
}

/// Why the session server would not take a join.
#[derive(Debug, PartialEq)]
pub enum JoinError {
    /// It refused the access token itself, which has run out or been
    /// replaced: a new sign-in may be taken.
    Session(String),
    /// Anything a new sign-in would not change.
    Other(String),
}

impl JoinError {
    pub fn into_reason(self) -> String {
        match self {
            Self::Session(reason) | Self::Other(reason) => reason,
        }
    }

    /// What a 403's `error` means. A token that has run out comes back as
    /// `ForbiddenOperationException`, so whatever is not about the account
    /// itself is taken to be about the token.
    fn forbidden(error: &str) -> Self {
        match error {
            "InsufficientPrivilegesException" => {
                Self::Other("multiplayer is disabled for this account".into())
            }
            "UserBannedException" => Self::Other("this account is banned from multiplayer".into()),
            "InvalidCredentialsException" => {
                Self::Session("the session is invalid; sign in again".into())
            }
            _ => Self::Session(format!("session server refused the join: {error}")),
        }
    }
}

/// Tells the session server of the join with `token`. Where it refuses the
/// token itself, `renew` signs in again and the join is made once more
/// with the token that gives; the token the server took.
pub async fn join_renewing<J, JoinFuture, N, RenewFuture>(
    token: String,
    join: J,
    renew: N,
) -> Result<String, String>
where
    J: Fn(String) -> JoinFuture,
    JoinFuture: Future<Output = Result<(), JoinError>>,
    N: FnOnce() -> RenewFuture,
    RenewFuture: Future<Output = Option<String>>,
{
    match join(token.clone()).await {
        Ok(()) => Ok(token),
        Err(JoinError::Other(reason)) => Err(reason),
        Err(JoinError::Session(reason)) => {
            tracing::warn!("The session server refused the sign-in ({reason}); signing in again");
            let Some(renewed) = renew().await else {
                return Err(format!(
                    "{reason}. The sign-in could not be renewed: sign in again from the title \
                     screen"
                ));
            };
            join(renewed.clone())
                .await
                .map(|()| renewed)
                .map_err(JoinError::into_reason)
        }
    }
}

/// POSTs the join to the session server.
pub async fn join(
    access_token: &str,
    uuid: &uuid::Uuid,
    server_hash: &str,
) -> Result<(), JoinError> {
    let response = super::http::client()
        .post("https://sessionserver.mojang.com/session/minecraft/join")
        .json(&serde_json::json!({
            "accessToken": access_token,
            "selectedProfile": uuid.simple().to_string(),
            "serverId": server_hash,
        }))
        .send()
        .await
        .map_err(|e| JoinError::Other(format!("session server unreachable: {e}")))?;
    match response.status() {
        reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::OK => Ok(()),
        reqwest::StatusCode::FORBIDDEN => {
            #[derive(serde::Deserialize)]
            struct Forbidden {
                error: String,
            }
            let error = response
                .json::<Forbidden>()
                .await
                .map(|f| f.error)
                .unwrap_or_default();
            Err(JoinError::forbidden(&error))
        }
        reqwest::StatusCode::UNAUTHORIZED => Err(JoinError::Session(
            "the session is invalid; sign in again".into(),
        )),
        reqwest::StatusCode::TOO_MANY_REQUESTS => Err(JoinError::Other(
            "session server rate limit; try again".into(),
        )),
        status => Err(JoinError::Other(format!(
            "session server answered {status}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{JoinError, join_renewing, server_hash};

    /// A token that has run out is the session's fault; a ban or a parent's
    /// switch is not, and a new sign-in would not help.
    #[test]
    fn a_refusal_is_the_token_s_or_the_account_s() {
        let session = |error| matches!(JoinError::forbidden(error), JoinError::Session(_));
        assert!(session("ForbiddenOperationException"));
        assert!(session("InvalidCredentialsException"));
        assert!(session(""));
        assert!(!session("UserBannedException"));
        assert!(!session("InsufficientPrivilegesException"));
    }

    /// A refused token is renewed once and the join made again with the new
    /// one; a refusal a new sign-in would not change is not renewed for.
    #[tokio::test]
    async fn a_refused_token_is_renewed_once() {
        let tried = RefCell::new(Vec::new());
        let join = |token: String| {
            tried.borrow_mut().push(token.clone());
            async move {
                match token.as_str() {
                    "new" => Ok(()),
                    "banned" => Err(JoinError::Other("banned".into())),
                    _ => Err(JoinError::Session("spent".into())),
                }
            }
        };
        let renew_to = |token: Option<&'static str>| move || async move { token.map(String::from) };

        assert_eq!(
            join_renewing("new".into(), join, renew_to(None)).await,
            Ok("new".into())
        );
        assert_eq!(
            join_renewing("old".into(), join, renew_to(Some("new"))).await,
            Ok("new".into())
        );
        assert_eq!(*tried.borrow(), ["new", "old", "new"]);
        // The new token refused too: that is the answer, with no third try.
        tried.borrow_mut().clear();
        assert_eq!(
            join_renewing("old".into(), join, renew_to(Some("older"))).await,
            Err("spent".into())
        );
        assert_eq!(*tried.borrow(), ["old", "older"]);
        // Nothing to renew with: the player is told to sign in.
        let failed = join_renewing("old".into(), join, renew_to(None)).await;
        assert!(failed.unwrap_err().contains("sign in again"));
        // And a ban is a ban.
        let renew_never = || async { panic!("a ban is not renewed for") };
        assert_eq!(
            join_renewing("banned".into(), join, renew_never).await,
            Err("banned".into())
        );
    }

    /// The examples from the protocol documentation, hashing only the name
    /// (the other inputs empty).
    #[test]
    fn hashes_print_as_signed_hex() {
        assert_eq!(
            server_hash("Notch", &[], &[]),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        assert_eq!(
            server_hash("jeb_", &[], &[]),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
        assert_eq!(
            server_hash("simon", &[], &[]),
            "88e16a1019277b15d58faf0541e11910eb756f6"
        );
    }
}
