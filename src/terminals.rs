//! Terminals paste with Ctrl+Shift+V, not Ctrl+V. The lists are from OpenWhispr
//! (`resources/windows-fast-paste.c`, MIT).

const TERMINAL_CLASSES: [&str; 10] = [
    "ConsoleWindowClass",
    "CASCADIA_HOSTING_WINDOW_CLASS",
    "mintty",
    "VirtualConsoleClass",
    "PuTTY",
    "Alacritty",
    "org.wezfurlong.wezterm",
    "Hyper",
    "TMobaXterm",
    "kitty",
];

/// Electron terminals use the browser window class, so they are found by their exe name.
const TERMINAL_EXES: [&str; 4] = ["termius.exe", "tabby.exe", "wave.exe", "rio.exe"];

/// `class` is the window class of the target; `exe` is its file name or full path.
pub fn is_terminal(class: &str, exe: &str) -> bool {
    let file_name = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    TERMINAL_CLASSES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(class))
        || TERMINAL_EXES
            .iter()
            .any(|e| e.eq_ignore_ascii_case(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_by_class() {
        assert!(is_terminal(
            "CASCADIA_HOSTING_WINDOW_CLASS",
            "WindowsTerminal.exe"
        ));
        assert!(is_terminal("consolewindowclass", ""));
    }

    #[test]
    fn terminals_by_exe() {
        assert!(is_terminal(
            "Chrome_WidgetWin_1",
            r"C:\Users\x\AppData\Local\Programs\Tabby\Tabby.exe"
        ));
    }

    #[test]
    fn other_apps_are_not_terminals() {
        assert!(!is_terminal("Notepad", r"C:\Windows\notepad.exe"));
        assert!(!is_terminal(
            "Chrome_WidgetWin_1",
            r"C:\Program Files\Google\Chrome\chrome.exe"
        ));
    }
}
