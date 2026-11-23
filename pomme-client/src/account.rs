//! The Microsoft account the client plays as when no launcher passed one:
//! restored from `pomme-auth`'s store at startup, or signed in from the menu
//! with a device code.

use std::path::Path;

use pomme_auth::{AuthAccount, SecretStore};

/// The OS credential store on desktop; Android has none, so a file in the
/// app's private storage.
// TODO: wrap the Android file in the Android Keystore.
pub fn store(game_dir: &Path) -> Box<dyn SecretStore> {
    #[cfg(target_os = "android")]
    return Box::new(pomme_auth::FileStore {
        path: game_dir.join("accounts.json"),
    });
    #[cfg(not(target_os = "android"))]
    {
        let _ = game_dir;
        Box::new(pomme_auth::KeyringStore)
    }
}

/// Removes `uuid`'s stored account, whichever way its uuid was written.
pub fn forget(store: &dyn SecretStore, uuid: &uuid::Uuid) {
    if let Some(account) = pomme_auth::get_all_accounts(store)
        .into_iter()
        .find(|a| uuid::Uuid::parse_str(&a.uuid).is_ok_and(|stored| stored == *uuid))
    {
        pomme_auth::remove_account(store, &account.uuid);
    }
}

/// How long before its end a token is taken as spent: a join started with
/// less left could outlast it.
const SPENT_MARGIN_SECS: u64 = 300;

/// `uuid`'s stored account, whichever way its uuid was written.
fn stored(store: &dyn SecretStore, uuid: &uuid::Uuid) -> Option<AuthAccount> {
    pomme_auth::get_all_accounts(store)
        .into_iter()
        .find(|a| uuid::Uuid::parse_str(&a.uuid).is_ok_and(|stored| stored == *uuid))
}

/// `uuid`'s stored access token has run out, or is about to: a session
/// server would refuse a join made with it. False where nothing is stored
/// for it (an account passed by flags has no stored token to renew).
pub fn token_spent(store: &dyn SecretStore, uuid: &uuid::Uuid) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    stored(store, uuid).is_some_and(|account| account.expires_at <= now + SPENT_MARGIN_SECS)
}

/// Signs `uuid` in again from its stored refresh token, with nothing for
/// the player to do: a Minecraft token lasts a day, and a client left open
/// longer would otherwise have to be restarted to join anything. `None`
/// where nothing is stored for it or Microsoft will not renew it.
pub async fn renew(store: &dyn SecretStore, uuid: &uuid::Uuid) -> Option<AuthAccount> {
    let account = stored(store, uuid)?;
    pomme_auth::refresh(crate::net::http::client(), store, &account.uuid)
        .await
        .inspect_err(|error| tracing::warn!("Couldn't renew the sign-in: {error}"))
        .ok()
}

/// The most recently signed-in account, refreshed if its token expired.
pub async fn restore(store: &dyn SecretStore) -> Option<AuthAccount> {
    let last = pomme_auth::get_all_accounts(store).pop()?;
    pomme_auth::try_restore_or_refresh(crate::net::http::client(), store, &last.uuid).await
}

#[cfg(test)]
mod tests {
    use pomme_auth::SecretStore;

    use super::*;

    /// A token is spent once it has run out or has under five minutes left,
    /// whichever way the store wrote the account's uuid; an account the
    /// store does not hold has nothing to renew.
    #[test]
    fn a_stored_token_is_spent_near_its_end() {
        let dir = crate::test_util::test_temp_dir("account");
        let store = pomme_auth::FileStore {
            path: dir.join("accounts.json"),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let uuid = uuid::Uuid::parse_str("6183b673-3b24-43e4-ac73-d4300d3bab00").unwrap();
        let other = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let write = |expires_at: u64| {
            let accounts = serde_json::json!([{
                "username": "Tester",
                // As Mojang's profile gives it: no dashes.
                "uuid": uuid.simple().to_string(),
                "access_token": "token",
                "expires_at": expires_at,
            }]);
            store.write("minecraft-accounts", &accounts.to_string());
        };
        assert!(!token_spent(&store, &uuid));
        write(now + 3600);
        assert!(!token_spent(&store, &uuid));
        write(now + 60);
        assert!(token_spent(&store, &uuid));
        write(now.saturating_sub(10));
        assert!(token_spent(&store, &uuid));
        assert!(!token_spent(&store, &other));
        assert_eq!(stored(&store, &uuid).unwrap().username, "Tester");
        let _ = std::fs::remove_dir_all(dir);
    }
}
