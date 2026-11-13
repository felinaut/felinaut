//! Proxy passwords in the login Keychain, so they never reach Felinaut's own files.
//! The calls talk to `securityd` and may show a macOS permission prompt, so run them off the
//! UI thread.

use std::ffi::c_void;

use objc2::{rc::Retained, runtime::AnyObject};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSString};

/// Keychain items are listed under this name in Keychain Access.
const SERVICE: &str = "Felinaut SOCKS proxy";
const ERR_SEC_SUCCESS: i32 = 0;
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

// CFStringRef and CFBooleanRef constants are toll-free bridged to NSString and NSNumber.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: &'static NSString;
    static kSecClassGenericPassword: &'static NSString;
    static kSecAttrService: &'static NSString;
    static kSecAttrAccount: &'static NSString;
    static kSecValueData: &'static NSString;
    static kSecReturnData: &'static NSString;
    static kSecMatchLimit: &'static NSString;
    static kSecMatchLimitOne: &'static NSString;

    fn SecItemAdd(attributes: *const c_void, result: *mut *const c_void) -> i32;
    fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> i32;
    fn SecItemDelete(query: *const c_void) -> i32;
}

/// A Keychain call failed with this `OSStatus`.
#[derive(Debug, PartialEq, Eq)]
pub struct KeychainError(pub i32);

fn query(
    account: &str,
    extra: &[(&NSString, &AnyObject)],
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let service = NSString::from_str(SERVICE);
    let account = NSString::from_str(account);
    // SAFETY: the Security framework constants are immutable CFStrings, valid for the life of
    // the process once the framework is linked.
    let (mut keys, mut values): (Vec<&NSString>, Vec<&AnyObject>) = unsafe {
        (
            vec![kSecClass, kSecAttrService, kSecAttrAccount],
            vec![
                kSecClassGenericPassword.as_ref(),
                service.as_ref(),
                account.as_ref(),
            ],
        )
    };
    for (key, value) in extra {
        keys.push(key);
        values.push(value);
    }
    NSDictionary::from_slices(&keys, &values)
}

fn as_cf(dictionary: &NSDictionary<NSString, AnyObject>) -> *const c_void {
    (dictionary as *const NSDictionary<NSString, AnyObject>).cast()
}

/// Save `password` for `account`, replacing any earlier one.
pub fn store(account: &str, password: &str) -> Result<(), KeychainError> {
    delete(account)?;
    let data = NSData::with_bytes(password.as_bytes());
    // SAFETY: see `query`; the dictionary is a valid CFDictionaryRef for the call.
    let status = unsafe {
        let attributes = query(account, &[(kSecValueData, data.as_ref())]);
        SecItemAdd(as_cf(&attributes), std::ptr::null_mut())
    };
    match status {
        ERR_SEC_SUCCESS => Ok(()),
        status => Err(KeychainError(status)),
    }
}

/// The password saved for `account`, or `None` if there is none.
pub fn load(account: &str) -> Result<Option<String>, KeychainError> {
    let yes = NSNumber::new_bool(true);
    let mut result: *const c_void = std::ptr::null();
    // SAFETY: see `query`. On success with kSecReturnData and kSecMatchLimitOne the result is
    // a +1 CFDataRef (toll-free bridged to NSData), which `Retained::from_raw` takes over.
    let status = unsafe {
        let query = query(
            account,
            &[
                (kSecReturnData, yes.as_ref()),
                (kSecMatchLimit, kSecMatchLimitOne.as_ref()),
            ],
        );
        SecItemCopyMatching(as_cf(&query), &mut result)
    };
    match status {
        ERR_SEC_SUCCESS => {
            // SAFETY: see above.
            let data = unsafe { Retained::from_raw(result.cast_mut().cast::<NSData>()) }
                .ok_or(KeychainError(status))?;
            String::from_utf8(data.to_vec())
                .map(Some)
                .map_err(|_| KeychainError(status))
        }
        ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        status => Err(KeychainError(status)),
    }
}

/// Forget the password for `account`; a missing one is not an error.
pub fn delete(account: &str) -> Result<(), KeychainError> {
    // SAFETY: see `query`.
    let status = unsafe { SecItemDelete(as_cf(&query(account, &[]))) };
    match status {
        ERR_SEC_SUCCESS | ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        status => Err(KeychainError(status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real login Keychain, so it runs only on request:
    /// `FELINAUT_KEYCHAIN_TEST=1 cargo test --bins keychain`.
    #[test]
    fn stores_replaces_loads_and_deletes() {
        if std::env::var_os("FELINAUT_KEYCHAIN_TEST").is_none() {
            return;
        }
        let account = format!("felinaut-test-{}@127.0.0.1:1", std::process::id());
        assert_eq!(load(&account), Ok(None));
        store(&account, "first").unwrap();
        store(&account, "sécret ✓").unwrap();
        assert_eq!(load(&account), Ok(Some("sécret ✓".into())));
        delete(&account).unwrap();
        delete(&account).unwrap();
        assert_eq!(load(&account), Ok(None));
    }
}
