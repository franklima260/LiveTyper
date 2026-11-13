//! The settings text file, stored in the platform configuration directory.
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
    pub microphone_open: MicrophoneOpen,
    pub microphone_open_after_use_min: u32,
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
    pub startup_method: StartupMethod,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            talk_key: "Ctrl+Space".into(),
            scratch_pad_x: None,
            scratch_pad_y: None,
            scratch_pad_width: 640,
            microphone: String::new(),
            microphone_open: MicrophoneOpen::Always,
            microphone_open_after_use_min: 5,
            models_dir: String::new(),
            unload_after_min: 10,
            lookahead_ms: 480,
            recast_interval_ms: 500,
            recast_window_s: 15,
            release_deadline_ms: if cfg!(target_os = "linux") {
                10_000
            } else {
                2_000
            },
            min_hold_ms: 300,
            live_threads: 0,
            recast_threads: 0,
            start_with_windows: true,
            startup_method: StartupMethod::Registry,
        }
    }
}

/// When the microphone is open. While it is open, LiveTyper keeps the last seconds of audio in
/// memory, so that a Dictation can start a moment before the push of the Talk Key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrophoneOpen {
    /// All the time while LiveTyper runs.
    Always,
    /// From the first Dictation until `microphone_open_after_use_min` after the last one.
    AfterUse,
    /// Only during a Dictation: no audio from before the push.
    Dictation,
}

/// How LiveTyper starts with Windows (`start_with_windows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupMethod {
    /// A value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
    Registry,
    /// A shortcut in the user's Startup folder.
    StartupFolder,
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
# On Windows the Talk Key is intercepted. On Linux choose a chord that does not conflict.
talk_key = "Ctrl+Space"

# Scratch Pad position: the center of its bottom edge, in screen pixels.
# Without these two keys, the Scratch Pad shows at the bottom center of the main screen.
# scratch_pad_x = 960
# scratch_pad_y = 1000
scratch_pad_width = 640

# Microphone name. Empty uses the default input device.
microphone = ""

# When the microphone is open. While it is open, LiveTyper keeps the last 3 seconds of audio in
# memory only (never on disk), so that a Dictation also gets the start of a word that you began a
# moment before you pushed the Talk Key.
#   "always"    = all the time while LiveTyper runs. Windows shows the microphone icon all the time.
#   "after_use" = from a Dictation until microphone_open_after_use_min minutes after the last one.
#                 Use this with a Bluetooth headset: while its microphone is open, its sound is worse.
#   "dictation" = only while you hold the Talk Key. The start of a word before the push is lost.
microphone_open = "always"
microphone_open_after_use_min = 5

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
# Default: 2000 on Windows, 10000 on Linux.
# release_deadline_ms = 2000

# A shorter key press (ms) does nothing.
min_hold_ms = 300

# CPU threads for the live model and the Recast model. 0 = automatic.
live_threads = 0
recast_threads = 0

# Start LiveTyper when you sign in to Windows.
start_with_windows = true
# How: "registry" (a Run value in the registry) or "startup_folder" (a shortcut in the Startup
# folder). If one way does not work on a PC, try the other.
startup_method = "registry"
"#;

pub fn parse(text: &str) -> Result<Settings, String> {
    let settings: Settings = toml::from_str(text).map_err(|e| e.to_string())?;
    if crate::hotkey::Chord::parse(&settings.talk_key).is_err() {
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
    if settings.microphone_open_after_use_min < 1 {
        return Err("microphone_open_after_use_min: must be 1 or more".into());
    }
    if settings.recast_window_s < 2 {
        return Err("recast_window_s: must be 2 or more".into());
    }
    Ok(settings)
}

#[cfg(any(windows, test))]
fn with_values(text: &str, values: &[(&str, &str)]) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, Value};

    let mut document = text
        .parse::<DocumentMut>()
        .map_err(|error| error.to_string())?;
    for (key, value) in values {
        let mut value = value.parse::<Value>().map_err(|error| error.to_string())?;
        if let Some(previous) = document.get_mut(key).and_then(Item::as_value_mut) {
            *value.decor_mut() = previous.decor().clone();
            *previous = value;
        } else {
            document.insert(key, Item::Value(value));
        }
    }
    Ok(document.to_string())
}

/// Sets keys in the settings file (see `with_values`). A missing file starts as the template.
/// The file changes only if the result is valid.
#[cfg(windows)]
pub fn set_values(values: &[(&str, &str)]) -> Result<Settings, String> {
    let path = paths::settings_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_owned(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let text = with_values(&text, values)?;
    let settings = parse(&text)?;
    fs::create_dir_all(paths::roaming_dir())
        .and_then(|()| fs::write(&path, text))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
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
    fn microphone_open_values() {
        let s = parse("microphone_open = \"after_use\"").unwrap();
        assert_eq!(s.microphone_open, MicrophoneOpen::AfterUse);
        assert_eq!(
            parse("microphone_open = \"dictation\"")
                .unwrap()
                .microphone_open,
            MicrophoneOpen::Dictation
        );
        assert!(parse("microphone_open = \"sometimes\"").is_err());
        assert!(parse("microphone_open_after_use_min = 0").is_err());
        assert_eq!(
            parse("microphone_open_after_use_min = 15")
                .unwrap()
                .microphone_open_after_use_min,
            15
        );
    }

    #[test]
    fn startup_method_values() {
        assert_eq!(
            parse("startup_method = \"startup_folder\"")
                .unwrap()
                .startup_method,
            StartupMethod::StartupFolder
        );
        assert!(parse("startup_method = \"task\"").is_err());
    }

    #[test]
    fn set_values_changes_lines_and_keeps_comments() {
        let text = with_values(
            TEMPLATE,
            &[
                ("start_with_windows", "false"),
                ("startup_method", "\"startup_folder\""),
            ],
        )
        .unwrap();
        let s = parse(&text).unwrap();
        assert!(!s.start_with_windows);
        assert_eq!(s.startup_method, StartupMethod::StartupFolder);
        assert_eq!(text.lines().count(), TEMPLATE.lines().count());
        assert!(text.contains("# Start LiveTyper when you sign in to Windows."));
        // A key that is not in the text is added; a commented key is not changed.
        let text = with_values(
            "# talk_key = \"F9\"
microphone = \"\"
",
            &[("talk_key", "\"F9\"")],
        )
        .unwrap();
        assert_eq!(
            text,
            "# talk_key = \"F9\"
microphone = \"\"
talk_key = \"F9\"
"
        );
        // A key that only starts the same is another key.
        let text = with_values(
            "start_with_windows_x = 1
",
            &[("start_with_windows", "true")],
        )
        .unwrap();
        assert!(text.ends_with(
            "start_with_windows = true
"
        ));
    }

    #[test]
    fn installer_updates_a_quoted_startup_key_and_preserves_comments() {
        let text =
            "# Keep this comment\n'start_with_windows' = true # Keep this too\ntalk_key = \"F9\"\n";
        let updated = with_values(text, &[("start_with_windows", "false")]).unwrap();
        let settings = parse(&updated).expect("updated settings");
        assert!(!settings.start_with_windows);
        assert_eq!(settings.talk_key, "F9");
        assert!(updated.contains("# Keep this comment"));
        assert!(updated.contains("# Keep this too"));
    }

    #[test]
    fn empty_models_dir_means_the_default_folder() {
        let s = Settings::default();
        assert_eq!(s.models_dir(), paths::local_dir().join("models"));
        let s = parse("models_dir = 'D:\\models'").unwrap();
        assert_eq!(s.models_dir(), PathBuf::from("D:\\models"));
    }
}
