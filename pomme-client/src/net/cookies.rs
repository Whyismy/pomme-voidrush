//! The cookies servers have given the client (vanilla
//! `ClientCommonPacketListenerImpl.serverCookies`): kept while the session
//! lasts and carried through a transfer to another server, which may ask
//! for them back.

use std::sync::Mutex;

/// One session's cookies, by key. A handful at most, so a list.
static COOKIES: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());

/// `handleStoreCookie`: `key` holds `payload` from now on.
pub fn store(key: &str, payload: Vec<u8>) {
    let mut cookies = COOKIES.lock().unwrap();
    match cookies.iter_mut().find(|(name, _)| name == key) {
        Some((_, held)) => *held = payload,
        None => cookies.push((key.to_owned(), payload)),
    }
}

/// `handleRequestCookie`: what `key` holds, if anything.
pub fn get(key: &str) -> Option<Vec<u8>> {
    let cookies = COOKIES.lock().unwrap();
    cookies
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, payload)| payload.clone())
}

/// A connection that is not a transfer starts with none.
pub fn clear() {
    COOKIES.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    /// A cookie is kept until it is replaced or the session's are cleared.
    #[test]
    fn a_cookie_is_kept_replaced_and_cleared() {
        super::clear();
        assert_eq!(super::get("pomme:test"), None);
        super::store("pomme:test", vec![1, 2]);
        super::store("pomme:other", vec![9]);
        assert_eq!(super::get("pomme:test"), Some(vec![1, 2]));
        super::store("pomme:test", vec![3]);
        assert_eq!(super::get("pomme:test"), Some(vec![3]));
        assert_eq!(super::get("pomme:other"), Some(vec![9]));
        super::clear();
        assert_eq!(super::get("pomme:test"), None);
    }
}
