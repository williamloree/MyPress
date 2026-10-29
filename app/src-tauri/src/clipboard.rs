//! Accès au presse-papiers système et collage dans l'application précédente.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use image::{imageops::FilterType, RgbaImage};

/// Taille max d'un texte capturé (le blob chiffré doit rester sous `MAX_BLOB_LEN`).
pub const MAX_TEXT_BYTES: usize = 512 * 1024;
/// Taille max d'une image PNG capturée : encodée deux fois en base64 (contenu puis
/// blob chiffré), elle doit rester sous `MAX_BLOB_LEN`.
pub const MAX_IMAGE_BYTES: usize = 560 * 1024;
/// Cadre de l'aperçu affiché dans l'historique (pixels).
const THUMB_W: u32 = 320;
const THUMB_H: u32 = 160;

/// Contenu capturé dans le presse-papiers.
pub enum Clip {
    Text(String),
    /// Image PNG en base64.
    Image(String),
}

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

fn read_rgba() -> Option<RgbaImage> {
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn decode_png(png_b64: &str) -> Option<RgbaImage> {
    let bytes = B64.decode(png_b64).ok()?;
    Some(image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.to_rgba8())
}

/// Image en PNG base64, réduite par paliers jusqu'à tenir dans `MAX_IMAGE_BYTES`.
fn image_to_png(mut img: RgbaImage) -> Option<String> {
    loop {
        let png = encode_png(&img)?;
        if png.len() <= MAX_IMAGE_BYTES {
            return Some(B64.encode(png));
        }
        let (w, h) = (img.width() * 3 / 4, img.height() * 3 / 4);
        if w == 0 || h == 0 {
            return None;
        }
        img = image::imageops::resize(&img, w, h, FilterType::Triangle);
    }
}

pub fn write_image(png_b64: &str) -> anyhow::Result<()> {
    let img = decode_png(png_b64).ok_or_else(|| anyhow::anyhow!("image illisible"))?;
    let data = arboard::ImageData { width: img.width() as usize, height: img.height() as usize, bytes: img.into_raw().into() };
    arboard::Clipboard::new()?.set_image(data)?;
    Ok(())
}

/// Aperçu PNG (base64) d'une image, pour l'historique.
pub fn thumbnail(png_b64: &str) -> Option<String> {
    let img = decode_png(png_b64)?;
    let thumb = image::DynamicImage::ImageRgba8(img).thumbnail(THUMB_W, THUMB_H).to_rgba8();
    Some(B64.encode(encode_png(&thumb)?))
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

/// Surveille le presse-papiers et appelle `on_clip` à chaque nouveau contenu
/// (le texte l'emporte quand une copie fournit aussi une image, ex. cellules Excel).
/// Sous Windows on interroge le compteur de séquence (appel très léger) ;
/// ailleurs on compare le contenu (empreinte des pixels pour les images, pour
/// n'encoder en PNG qu'en cas de changement).
pub fn watch(mut on_clip: impl FnMut(Clip) + Send + 'static) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let fingerprint = |bytes: &[u8]| {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    std::thread::spawn(move || {
        let mut last_seq = sequence();
        let mut last = read_text().map(|t| fingerprint(t.as_bytes())).or_else(|| read_rgba().map(|i| fingerprint(&i)));
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
            let (fp, clip) = if let Some(text) = read_text() {
                (fingerprint(text.as_bytes()), Clip::Text(text))
            } else if let Some(img) = read_rgba() {
                let fp = fingerprint(&img);
                if last == Some(fp) && sequence().is_none() {
                    continue;
                }
                let Some(png) = image_to_png(img) else { continue };
                (fp, Clip::Image(png))
            } else {
                continue;
            };
            if last == Some(fp) && sequence().is_none() {
                continue;
            }
            last = Some(fp);
            on_clip(clip);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_image_is_downscaled_and_thumbnailed() {
        // Bruit : incompressible, dépasse largement la limite à pleine taille.
        let mut seed = 1u32;
        let img = RgbaImage::from_fn(1200, 900, |_, _| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgba(seed.to_le_bytes())
        });
        let png = image_to_png(img).unwrap();
        let decoded = decode_png(&png).unwrap();
        assert!(B64.decode(&png).unwrap().len() <= MAX_IMAGE_BYTES);
        assert!(decoded.width() < 1200);
        let thumb = decode_png(&thumbnail(&png).unwrap()).unwrap();
        assert!(thumb.width() <= THUMB_W && thumb.height() <= THUMB_H);
    }
}
