//! The settings text file (`%APPDATA%\LiveTyper\settings.toml`).
//!
//! A missing key takes its default value, so later versions can add keys without breaking old files.
//! An unknown key is an error, so that a typo does not silently do nothing.

use std::fs;

use serde::Deserialize;

use crate::paths;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub start_with_windows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            start_with_windows: true,
        }
    }
}

/// Written when the file does not exist. It must parse to `Settings::default()` (see the tests).
const TEMPLATE: &str = "\
# LiveTyper settings. Restart LiveTyper after a change.
# A key that is not in this file takes its default value.

# Start LiveTyper when you sign in to Windows.
start_with_windows = true
";

pub fn parse(text: &str) -> Result<Settings, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Reads the settings file, and writes the template if there is none.
/// On an error, returns the defaults and a message for the user.
pub fn load_or_create() -> (Settings, Option<String>) {
    let path = paths::settings_file();
    match fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok(settings) => (settings, None),
            Err(e) => {
                log::error!("settings file {}: {e}", path.display());
                let message = "The settings file has an error. LiveTyper uses the default settings. See the log.";
                (Settings::default(), Some(message.to_owned()))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(e) =
                fs::create_dir_all(paths::roaming_dir()).and_then(|()| fs::write(&path, TEMPLATE))
            {
                log::error!("cannot write {}: {e}", path.display());
            }
            (Settings::default(), None)
        }
        Err(e) => {
            log::error!("cannot read {}: {e}", path.display());
            (
                Settings::default(),
                Some(format!("Cannot read the settings file: {e}")),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_gives_the_defaults() {
        assert_eq!(parse(TEMPLATE), Ok(Settings::default()));
    }

    #[test]
    fn missing_keys_take_defaults() {
        assert_eq!(parse(""), Ok(Settings::default()));
    }

    #[test]
    fn values_are_read() {
        assert!(
            !parse("start_with_windows = false")
                .unwrap()
                .start_with_windows
        );
    }

    #[test]
    fn unknown_key_is_an_error() {
        let error = parse("start_with_window = false").unwrap_err();
        assert!(error.contains("start_with_window"), "{error}");
    }

    #[test]
    fn wrong_type_is_an_error() {
        assert!(parse("start_with_windows = \"yes\"").is_err());
    }
}
