//! Internationalisation via gettext. La langue suit la locale système.
//! Les chaînes source sont en anglais (msgid) ; les traductions vivent dans
//! `po/<lang>.po`, compilées en `<datadir>/locale/<lang>/LC_MESSAGES/aur-guard.mo`.

use gettextrs::{bindtextdomain, setlocale, textdomain, LocaleCategory};
use std::path::PathBuf;

const DOMAIN: &str = "aur-guard";

/// Initialise gettext. À appeler une fois au démarrage de chaque binaire.
pub fn init() {
    setlocale(LocaleCategory::LcAll, "");
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/usr/share"))
        .join("locale");
    let _ = bindtextdomain(DOMAIN, dir);
    let _ = textdomain(DOMAIN);
}

/// Traduit un message simple.
pub fn tr(msgid: &str) -> String {
    gettextrs::gettext(msgid)
}

/// Traduit puis remplace séquentiellement les `{}` par les arguments.
pub fn trf(msgid: &str, args: &[String]) -> String {
    let mut s = gettextrs::gettext(msgid);
    for arg in args {
        if let Some(i) = s.find("{}") {
            s.replace_range(i..i + 2, arg);
        }
    }
    s
}

/// Macro de traduction.
/// - `t!("English text")` → message simple.
/// - `t!("Found {} items", n)` → interpolation positionnelle des `{}`.
#[macro_export]
macro_rules! t {
    ($id:literal) => { $crate::i18n::tr($id) };
    ($id:literal, $($arg:expr),+ $(,)?) => {
        $crate::i18n::trf($id, &[$(($arg).to_string()),+])
    };
}
