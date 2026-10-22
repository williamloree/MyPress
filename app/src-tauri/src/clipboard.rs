//! Accès au presse-papiers système et collage dans l'application précédente.

use std::time::Duration;

/// Taille max d'un texte capturé (le blob chiffré doit rester sous `MAX_BLOB_LEN`).
pub const MAX_TEXT_BYTES: usize = 512 * 1024;

pub fn read_text() -> Option<String> {
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
        return None;
    }
    Some(text)
}

pub fn write_text(text: &str) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_text(text.to_string())?;
    Ok(())
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::DataExchange::{
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

    /// Compteur incrémenté par Windows à chaque modification : évite de relire le contenu.
    pub fn sequence() -> Option<u32> {
        Some(unsafe { GetClipboardSequenceNumber() })
    }

    fn format_available(name: &str) -> bool {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let id = RegisterClipboardFormatW(wide.as_ptr());
            id != 0 && IsClipboardFormatAvailable(id) != 0
        }
    }

    /// Formats posés par les gestionnaires de mots de passe (KeePass, 1Password,
    /// Bitwarden…) pour demander aux historiques de ne pas enregistrer la copie.
    pub fn is_excluded() -> bool {
        format_available("ExcludeClipboardContentFromMonitorProcessing")
            || format_available("Clipboard Viewer Ignore")
    }

    pub fn foreground_window() -> isize {
        unsafe { GetForegroundWindow() as isize }
    }

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    pub fn focus_and_paste(hwnd: isize) -> bool {
        unsafe {
            if hwnd == 0 || SetForegroundWindow(hwnd as _) == 0 {
                return false;
            }
            std::thread::sleep(super::Duration::from_millis(60));
            let inputs = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
            SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
        }
        true
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn sequence() -> Option<u32> {
        None
    }
    pub fn is_excluded() -> bool {
        false
    }
    pub fn foreground_window() -> isize {
        0
    }
    /// Collage automatique non implémenté hors Windows (version 2) :
    /// l'élément est seulement placé dans le presse-papiers.
    pub fn focus_and_paste(_hwnd: isize) -> bool {
        false
    }
}

pub use imp::{focus_and_paste, foreground_window, is_excluded, sequence};

/// Surveille le presse-papiers et appelle `on_text` à chaque nouveau texte.
/// Sous Windows on interroge le compteur de séquence (appel très léger) ;
/// ailleurs on compare le contenu.
pub fn watch(mut on_text: impl FnMut(String) + Send + 'static) {
    std::thread::spawn(move || {
        let mut last_seq = sequence();
        let mut last_text = read_text();
        loop {
            std::thread::sleep(Duration::from_millis(300));
            if let Some(seq) = sequence() {
                if Some(seq) == last_seq {
                    continue;
                }
                last_seq = Some(seq);
                if is_excluded() {
                    continue;
                }
            }
            let Some(text) = read_text() else { continue };
            if last_text.as_deref() == Some(text.as_str()) && sequence().is_none() {
                continue;
            }
            last_text = Some(text.clone());
            on_text(text);
        }
    });
}
