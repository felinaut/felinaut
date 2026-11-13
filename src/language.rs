//! The language Felinaut asks websites and search engines for, chosen in Settings (⌘,).
//!
//! Many sites pick a language from the visitor's location rather than the browser's preferred
//! languages, so searches also carry an explicit hint (see `engine`). Websites in general see
//! the choice through WebKit's `Accept-Language` header and `navigator.language`, which WebKit
//! derives from the app's preferred languages when it starts.

/// A website language choice. `System` leaves the macOS language list untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebLanguage {
    #[default]
    System,
    English,
    French,
}

impl WebLanguage {
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::French];

    pub fn name(self) -> &'static str {
        match self {
            Self::System => "Same as macOS",
            Self::English => "English",
            Self::French => "Français",
        }
    }

    /// The persisted identifier, independent of the display name.
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::French => "fr",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.key() == key)
    }

    /// The BCP 47 language code to ask for, or `None` to follow macOS.
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::English => Some("en"),
            Self::French => Some("fr"),
        }
    }
}

/// Make `language` the app's preferred language before WebKit starts, so pages receive it.
///
/// Call this first in `main`, before anything reads the locale: macOS caches the preferred
/// languages for the life of the process, so a change applies at the next launch. The
/// Felinaut settings file stays the source of truth; this mirrors it into the app's own
/// `AppleLanguages` default (never the system-wide one) on every launch, and removes it for
/// `System` so the macOS list applies again.
#[cfg(target_os = "macos")]
pub fn apply(language: WebLanguage) {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSArray, NSString, NSUserDefaults};

    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("AppleLanguages");
    match language.code() {
        Some(code) => {
            let languages = NSArray::from_retained_slice(&[NSString::from_str(code)]);
            // SAFETY: an NSArray of NSStrings is a valid property-list value for this key.
            unsafe {
                defaults.setObject_forKey(Some(AsRef::<AnyObject>::as_ref(&*languages)), &key)
            };
        }
        None => defaults.removeObjectForKey(&key),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply(_: WebLanguage) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_as_macos_is_the_default_and_asks_for_nothing() {
        assert_eq!(WebLanguage::default(), WebLanguage::System);
        assert_eq!(WebLanguage::System.code(), None);
        assert_eq!(WebLanguage::English.code(), Some("en"));
        assert_eq!(WebLanguage::French.code(), Some("fr"));
    }

    #[test]
    fn language_keys_and_names_round_trip_and_reject_unknown_values() {
        let mut keys = Vec::new();
        let mut names = Vec::new();
        for language in WebLanguage::ALL {
            assert_eq!(WebLanguage::from_key(language.key()), Some(language));
            keys.push(language.key());
            names.push(language.name());
        }
        keys.sort_unstable();
        keys.dedup();
        names.sort_unstable();
        names.dedup();
        assert_eq!(keys.len(), WebLanguage::ALL.len());
        assert_eq!(names.len(), WebLanguage::ALL.len());
        for key in ["", "de", "EN", "French", " fr", "fr-CH", "en,de"] {
            assert_eq!(WebLanguage::from_key(key), None, "{key:?}");
        }
    }
}
