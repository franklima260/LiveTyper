//! The settings text file (`%APPDATA%\LiveTyper\settings.toml`).
//!
//! A missing key takes its default value, so later versions can add keys without breaking old files.
//! An unknown key is an error, so that a typo does not silently do nothing.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::paths;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub talk_key: String,
    pub scratch_pad_x: Option<i32>,
    pub scratch_pad_y: Option<i32>,
    pub scratch_pad_width: u32,
    pub microphone: String,
    pub models_dir: String,
    pub unload_after_min: u32,
    pub lookahead_ms: u32,
    pub recast_interval_ms: u32,
    pub recast_window_s: u32,
    pub release_deadline_ms: u32,
    pub min_hold_ms: u32,
    pub live_threads: u32,
    pub recast_threads: u32,
    pub start_with_windows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            talk_key: "Ctrl+Space".into(),
            scratch_pad_x: None,
            scratch_pad_y: None,
            scratch_pad_width: 640,
            microphone: String::new(),
            models_dir: String::new(),
            unload_after_min: 10,
            lookahead_ms: 480,
            recast_interval_ms: 500,
            recast_window_s: 15,
            release_deadline_ms: 2000,
            min_hold_ms: 300,
            live_threads: 0,
            recast_threads: 0,
            start_with_windows: true,
        }
    }
}

impl Settings {
    pub fn models_dir(&self) -> PathBuf {
        if self.models_dir.trim().is_empty() {
            paths::local_dir().join("models")
        } else {
            PathBuf::from(self.models_dir.trim())
        }
    }
}

/// The lookahead values that the streaming model was trained with.
pub const LOOKAHEAD_CHOICES_MS: [u32; 4] = [0, 80, 480, 1040];

/// Written when the file does not exist. It must parse to `Settings::default()` (see the tests).
const TEMPLATE: &str = r#"# LiveTyper settings. Restart LiveTyper after a change.
# A key that is not in this file takes its default value.

# Hold this key combination to dictate. Examples: "Ctrl+Space", "Ctrl+Alt+D", "F9".
# If another app uses the key (for example, PowerToys Peek can use Ctrl+Space), the Talk Key
# does not work and LiveTyper shows a warning. Then set a different key here.
talk_key = "Ctrl+Space"

# Scratch Pad position: the center of its bottom edge, in screen pixels.
# Without these two keys, the Scratch Pad shows at the bottom center of the main screen.
# scratch_pad_x = 960
# scratch_pad_y = 1000
scratch_pad_width = 640

# Microphone name, as Windows shows it. Empty = the Windows default microphone.
microphone = ""

# Folder with the model files. Empty = %LOCALAPPDATA%\LiveTyper\models.
models_dir = ""

# Unload the models after this many minutes without a Dictation.
unload_after_min = 10

# Live words: lookahead of the streaming model in ms (0, 80, 480 or 1040).
# Less lookahead shows words sooner but uses more CPU.
lookahead_ms = 480

# Recast: how often (ms), and how much of the latest audio (s) to decode again.
recast_interval_ms = 500
recast_window_s = 15

# After you release the Talk Key, wait at most this long (ms) for the last Recast.
release_deadline_ms = 2000

# A shorter key press (ms) does nothing.
min_hold_ms = 300

# CPU threads for the live model and the Recast model. 0 = automatic.
live_threads = 0
recast_threads = 0

# Start LiveTyper when you sign in to Windows.
start_with_windows = true
"#;

pub fn parse(text: &str) -> Result<Settings, String> {
    let settings: Settings = toml::from_str(text).map_err(|e| e.to_string())?;
    if settings
        .talk_key
        .parse::<global_hotkey::hotkey::HotKey>()
        .is_err()
    {
        return Err(format!(
            "talk_key: \"{}\" is not a key combination",
            settings.talk_key
        ));
    }
    if !LOOKAHEAD_CHOICES_MS.contains(&settings.lookahead_ms) {
        return Err(format!(
            "lookahead_ms: must be one of {LOOKAHEAD_CHOICES_MS:?}"
        ));
    }
    if settings.recast_window_s < 2 {
        return Err("recast_window_s: must be 2 or more".into());
    }
    Ok(settings)
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
        let s =
            parse("start_with_windows = false\nscratch_pad_x = 100\ntalk_key = \"F9\"").unwrap();
        assert!(!s.start_with_windows);
        assert_eq!(s.scratch_pad_x, Some(100));
        assert_eq!(s.scratch_pad_y, None);
        assert_eq!(s.talk_key, "F9");
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

    #[test]
    fn bad_talk_key_is_an_error() {
        assert!(parse("talk_key = \"Ctrl+Nothing\"").is_err());
    }

    #[test]
    fn lookahead_must_be_a_trained_value() {
        assert!(parse("lookahead_ms = 300").is_err());
        assert!(parse("lookahead_ms = 80").is_ok());
    }

    #[test]
    fn empty_models_dir_means_the_default_folder() {
        let s = Settings::default();
        assert!(s.models_dir().ends_with("LiveTyper\\models"));
        let s = parse("models_dir = 'D:\\models'").unwrap();
        assert_eq!(s.models_dir(), PathBuf::from("D:\\models"));
    }
}
