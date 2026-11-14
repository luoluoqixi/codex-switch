//! Application-owned translations, shared by foreground and background work.
use std::{
    collections::BTreeMap,
    sync::{
        LazyLock,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Language {
    #[default]
    English,
    SimplifiedChinese,
}

impl Language {
    pub(crate) fn detect() -> Self {
        Self::for_system_locale(sys_locale::get_locale().as_deref())
    }

    fn for_system_locale(locale: Option<&str>) -> Self {
        match locale.and_then(Self::from_code) {
            Some(Self::SimplifiedChinese) => Self::SimplifiedChinese,
            _ => Self::English,
        }
    }

    pub(crate) fn from_code(code: &str) -> Option<Self> {
        let language = code.trim().split(['-', '_', '.']).next()?;
        if language.eq_ignore_ascii_case("zh") {
            Some(Self::SimplifiedChinese)
        } else if language.eq_ignore_ascii_case("en") {
            Some(Self::English)
        } else {
            None
        }
    }

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(Language::English as u8);
static ENGLISH: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../locales/en.json")).expect("English catalog")
});
static CHINESE: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../locales/zh-CN.json")).expect("Simplified Chinese catalog")
});

pub(crate) fn language() -> Language {
    if LANGUAGE.load(Ordering::Relaxed) == Language::SimplifiedChinese as u8 {
        Language::SimplifiedChinese
    } else {
        Language::English
    }
}

pub(crate) fn set_language(language: Language) {
    LANGUAGE.store(language as u8, Ordering::Relaxed);
    // Keep built-in editor menus, dialogs, and other GPUI components in sync.
    gpui_kit::component::set_locale(language.code());
}

pub(crate) fn text(key: &str) -> &'static str {
    text_for(language(), key)
}

fn text_for(language: Language, key: &str) -> &'static str {
    let message = match language {
        Language::English => ENGLISH.get(key),
        Language::SimplifiedChinese => CHINESE.get(key).or_else(|| ENGLISH.get(key)),
    };
    message.map(String::as_str).unwrap_or("Missing translation")
}

pub(crate) fn format(key: &str, values: &[(&str, String)]) -> String {
    interpolate(text(key), values)
}

fn interpolate(template: &str, values: &[(&str, String)]) -> String {
    // Parse only the template. A profile name or error containing braces is data,
    // and must never be interpreted as another translation or placeholder.
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}').map(|end| start + end) else {
            result.push_str(&rest[start..]);
            return result;
        };
        let name = &rest[start + 1..end];
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push_str(&rest[start..=end]);
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    result
}

#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::text($key)
    };
    ($key:literal; $($name:ident = $value:expr),* $(,)?) => {
        $crate::i18n::format($key, &[$((stringify!($name), ($value).to_string())),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn chinese_system_locales_use_simplified_chinese() {
        for locale in ["zh", "zh-CN", "zh-TW", "zh-Hant-HK", "ZH_cn.UTF-8"] {
            assert_eq!(
                Language::for_system_locale(Some(locale)),
                Language::SimplifiedChinese
            );
        }
        for locale in [None, Some("en-US"), Some("fr-FR"), Some(""), Some("C")] {
            assert_eq!(Language::for_system_locale(locale), Language::English);
        }
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect()
    }

    #[test]
    fn catalogs_have_matching_keys_and_placeholders() {
        assert_eq!(
            ENGLISH.keys().collect::<Vec<_>>(),
            CHINESE.keys().collect::<Vec<_>>()
        );
        for (key, english) in ENGLISH.iter() {
            assert!(!english.trim().is_empty(), "{key}");
            assert!(
                !english
                    .chars()
                    .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
                "{key}"
            );
            assert_eq!(placeholders(english), placeholders(&CHINESE[key]), "{key}");
        }
    }

    #[test]
    fn interpolation_preserves_user_text_and_supports_different_word_order() {
        let values = [("name", "工作 {count}".into()), ("count", "3".into())];
        assert_eq!(
            interpolate("{count} versions of {name}", &values),
            "3 versions of 工作 {count}"
        );
        assert_eq!(
            interpolate("{name} · {count} 个版本", &values),
            "工作 {count} · 3 个版本"
        );
    }

    #[test]
    fn switching_language_updates_messages_and_component_locale() {
        const CHILD: &str = "CODEX_SWITCH_TEST_LANGUAGE_SWITCH";
        if std::env::var_os(CHILD).is_none() {
            // Locale is process-wide. Isolate this test so parallel store and
            // theme tests keep a stable language while checking their errors.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "i18n::tests::switching_language_updates_messages_and_component_locale",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        for (chosen, label, saved) in [
            (Language::English, "Settings", "Settings saved"),
            (Language::SimplifiedChinese, "全局设置", "设置已保存"),
            (Language::English, "Settings", "Settings saved"),
        ] {
            set_language(chosen);
            assert_eq!(language(), chosen);
            assert_eq!(crate::tr!("auxiliary_window.settings"), label);
            assert_eq!(crate::tr!("store.settings_saved"), saved);
            assert_eq!(&*gpui_kit::component::locale(), chosen.code());
        }
    }
}
