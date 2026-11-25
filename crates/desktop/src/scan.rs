// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Scannen über die Windows-Bilderfassung (WIA).
//!
//! * Scanner mit Einzug (Dokumentenscanner): Büro holt Blatt für Blatt ab,
//!   bis der Einzug leer ist – auf Wunsch beidseitig, leere Rückseiten fallen
//!   dann weg. Fortschritt und Abbrechen zeigt Windows an.
//! * Flachbettscanner: der Scan-Dialog von Windows (Vorschau, Farbe,
//!   Auflösung), eine Seite je Durchgang.
//!
//! Zuschneiden, Aufbereiten und das Zusammenfügen zur PDF übernimmt die
//! Scan-Ansicht der Oberfläche, die Texterkennung danach der Server.

use std::path::Path;

use windows::core::{BSTR, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_INPROC_SERVER,
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET,
    DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO,
};
use windows::Win32::System::Variant::VARIANT;

/// `WiaDeviceType.ScannerDeviceType`
const SCANNER: i32 = 1;
/// `WiaImageIntent.UnspecifiedIntent` (Farbe wählt man im Dialog)
const INTENT_UNSPECIFIED: i32 = 0;
/// `WiaImageIntent.ColorIntent`
const INTENT_COLOR: i32 = 1;
/// `WiaImageBias.MaximizeQuality`
const MAXIMIZE_QUALITY: i32 = 131072;
/// BMP: das Format, das jeder WIA-Treiber liefern muss
const FORMAT_BMP: &str = "{B96B3CAB-0728-11D3-9D7B-0000F81EF32E}";
/// Auflösung beim Scannen über den Einzug (gut für die Texterkennung)
const FEEDER_DPI: i32 = 300;

// WIA-Eigenschaften
const DOCUMENT_HANDLING_CAPABILITIES: &str = "3086";
const DOCUMENT_HANDLING_SELECT: &str = "3088";
const PAGES: &str = "3096";
const CURRENT_INTENT: &str = "6146";
const HORIZONTAL_RESOLUTION: &str = "6147";
const VERTICAL_RESOLUTION: &str = "6148";
const FEEDER: i32 = 1;
const DUPLEX: i32 = 4;

const WIA_ERROR_PAPER_EMPTY: u32 = 0x8021_0003;
const DISP_E_EXCEPTION: i32 = 0x8002_0009_u32 as i32;
const DISPID_PROPERTYPUT: i32 = -3;
const LOCALE_USER_DEFAULT: u32 = 0x0400;

/// Ergebnis eines Scan-Durchgangs
#[derive(Default)]
pub struct Scanned {
    /// Seiten als Bilddateien (meist BMP)
    pub pages: Vec<Vec<u8>>,
    /// Hinweis, wenn der Durchgang vorzeitig endete (Seiten bis dahin sind dabei)
    pub warning: Option<String>,
    /// Weggelassene leere Seiten (beidseitig)
    pub blank: u32,
}

impl Scanned {
    /// Für die Oberfläche: `BSCN`, Seitenzahl, je Seite Länge + Daten, dann
    /// Anzahl leerer Seiten und der Hinweis (UTF-8). Zahlen als u32 (LE).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pages.iter().map(|p| p.len() + 4).sum::<usize>() + 64);
        out.extend_from_slice(b"BSCN");
        out.extend_from_slice(&(self.pages.len() as u32).to_le_bytes());
        for p in &self.pages {
            out.extend_from_slice(&(p.len() as u32).to_le_bytes());
            out.extend_from_slice(p);
        }
        out.extend_from_slice(&self.blank.to_le_bytes());
        let w = self.warning.as_deref().unwrap_or("").as_bytes();
        out.extend_from_slice(&(w.len() as u32).to_le_bytes());
        out.extend_from_slice(w);
        out
    }
}

struct WiaError {
    code: u32,
    message: String,
}

type WiaResult<T> = Result<T, WiaError>;

fn plain(message: impl Into<String>) -> WiaError {
    WiaError { code: 0, message: message.into() }
}

/// Scannt über den Einzug (alle eingelegten Blätter) oder, bei einem
/// Flachbettscanner, eine Seite über den Windows-Dialog.
pub fn acquire(tmp_dir: &Path, duplex: bool) -> Result<Scanned, String> {
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let res = acquire_inner(tmp_dir, duplex).map_err(|e| e.message);
        if init.is_ok() {
            CoUninitialize();
        }
        res
    }
}

unsafe fn acquire_inner(tmp_dir: &Path, duplex: bool) -> WiaResult<Scanned> {
    let wide: Vec<u16> = "WIA.CommonDialog\0".encode_utf16().collect();
    let clsid = CLSIDFromProgID(PCWSTR(wide.as_ptr()))
        .map_err(|_| plain("Die Windows-Bilderfassung (WIA) ist auf diesem Rechner nicht verfügbar."))?;
    let dialog: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER)
        .map_err(|e| plain(format!("Windows-Bilderfassung lässt sich nicht starten: {}", e.message())))?;
    std::fs::create_dir_all(tmp_dir).map_err(|e| plain(format!("Zwischenspeicher: {e}")))?;

    // Scanner wählen (fragt nur nach, wenn mehrere angeschlossen sind)
    let device = invoke(&dialog, "ShowSelectDevice", DISPATCH_METHOD, vec![SCANNER.into(), false.into(), false.into()])?;
    let Ok(device) = IDispatch::try_from(&device) else {
        return Ok(Scanned::default());
    };
    let caps = get_prop(&device, DOCUMENT_HANDLING_CAPABILITIES).unwrap_or(0);
    if caps & FEEDER == 0 {
        return flatbed(&dialog, tmp_dir);
    }

    // Einzug: Gerät einstellen, dann Blatt für Blatt abholen
    let select = if duplex && caps & DUPLEX != 0 { FEEDER | DUPLEX } else { FEEDER };
    set_prop(&device, DOCUMENT_HANDLING_SELECT, select)?;
    let _ = set_prop(&device, PAGES, 1);
    let items = invoke(&device, "Items", DISPATCH_PROPERTYGET, vec![])?;
    let items = IDispatch::try_from(&items).map_err(|_| plain("Der Scanner meldet keinen Scanbereich."))?;
    let item = invoke(&items, "Item", DISPATCH_METHOD | DISPATCH_PROPERTYGET, vec![1i32.into()])?;
    let item = IDispatch::try_from(&item).map_err(|_| plain("Der Scanner meldet keinen Scanbereich."))?;
    // Farbe und Auflösung (die Absicht setzt die Auflösung zurück, daher zuerst)
    let _ = set_prop(&item, CURRENT_INTENT, INTENT_COLOR);
    let _ = set_prop(&item, HORIZONTAL_RESOLUTION, FEEDER_DPI);
    let _ = set_prop(&item, VERTICAL_RESOLUTION, FEEDER_DPI);

    let mut out = Scanned::default();
    // Sicherheitsgrenze gegen Endlosschleifen fehlerhafter Treiber
    for _ in 0..500 {
        // ShowTransfer(Item, FormatID, CancelError): zeigt den Fortschritt an
        let image = match invoke(&dialog, "ShowTransfer", DISPATCH_METHOD, vec![item.clone().into(), FORMAT_BMP.into(), false.into()]) {
            Ok(v) => v,
            Err(e) if e.code == WIA_ERROR_PAPER_EMPTY => {
                if out.pages.is_empty() && out.blank == 0 {
                    return Err(e);
                }
                break;
            }
            Err(e) => {
                if out.pages.is_empty() {
                    return Err(e);
                }
                out.warning = Some(format!("{} Die Seiten bis dahin sind übernommen.", e.message));
                break;
            }
        };
        // Abbrechen im Fortschrittsfenster liefert „Nothing“
        let Ok(image) = IDispatch::try_from(&image) else {
            if !out.pages.is_empty() {
                out.warning = Some("Scan abgebrochen. Die Seiten bis dahin sind übernommen.".into());
            }
            break;
        };
        let data = save(&image, tmp_dir)?;
        if select & DUPLEX != 0 && is_blank(&data) {
            out.blank += 1;
        } else {
            out.pages.push(data);
        }
    }
    Ok(out)
}

/// Flachbett: Scan-Dialog von Windows, eine Seite
unsafe fn flatbed(dialog: &IDispatch, tmp_dir: &Path) -> WiaResult<Scanned> {
    // ShowAcquireImage(DeviceType, Intent, Bias, FormatID, AlwaysSelectDevice, UseCommonUI, CancelError)
    let image = invoke(
        dialog,
        "ShowAcquireImage",
        DISPATCH_METHOD,
        vec![
            SCANNER.into(),
            INTENT_UNSPECIFIED.into(),
            MAXIMIZE_QUALITY.into(),
            FORMAT_BMP.into(),
            false.into(),
            true.into(),
            false.into(),
        ],
    )?;
    let mut out = Scanned::default();
    if let Ok(image) = IDispatch::try_from(&image) {
        out.pages.push(save(&image, tmp_dir)?);
    }
    Ok(out)
}

/// Bild (`ImageFile`) über eine temporäre Datei einlesen
unsafe fn save(image: &IDispatch, tmp_dir: &Path) -> WiaResult<Vec<u8>> {
    let ext = invoke(image, "FileExtension", DISPATCH_PROPERTYGET, vec![])
        .ok()
        .and_then(|v| BSTR::try_from(&v).ok())
        .map(|b| b.to_string())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bmp".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let file = tmp_dir.join(format!("scan-{nanos}.{ext}"));
    invoke(image, "SaveFile", DISPATCH_METHOD, vec![file.display().to_string().as_str().into()])?;
    let data = std::fs::read(&file).map_err(|e| plain(format!("Gescanntes Bild: {e}")));
    let _ = std::fs::remove_file(&file);
    data
}

/// `obj.Properties("id")`
unsafe fn property(obj: &IDispatch, id: &str) -> WiaResult<IDispatch> {
    let props = invoke(obj, "Properties", DISPATCH_PROPERTYGET, vec![])?;
    let props = IDispatch::try_from(&props).map_err(|_| plain("WIA: keine Eigenschaften"))?;
    let p = invoke(&props, "Item", DISPATCH_METHOD | DISPATCH_PROPERTYGET, vec![id.into()])?;
    IDispatch::try_from(&p).map_err(|_| plain(format!("WIA: Eigenschaft {id} fehlt")))
}

unsafe fn get_prop(obj: &IDispatch, id: &str) -> WiaResult<i32> {
    let v = invoke(&property(obj, id)?, "Value", DISPATCH_PROPERTYGET, vec![])?;
    i32::try_from(&v).map_err(|_| plain(format!("WIA: Eigenschaft {id} ist keine Zahl")))
}

unsafe fn set_prop(obj: &IDispatch, id: &str, value: i32) -> WiaResult<()> {
    let p = property(obj, id)?;
    call(&p, "Value", DISPATCH_PROPERTYPUT, vec![value.into()], true).map(|_| ())
}

/// Methode oder Eigenschaft eines Automatisierungsobjekts aufrufen
/// (`args` in der üblichen Reihenfolge)
unsafe fn invoke(obj: &IDispatch, name: &str, flags: DISPATCH_FLAGS, args: Vec<VARIANT>) -> WiaResult<VARIANT> {
    call(obj, name, flags, args, false)
}

unsafe fn call(obj: &IDispatch, name: &str, flags: DISPATCH_FLAGS, mut args: Vec<VARIANT>, put: bool) -> WiaResult<VARIANT> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let names = [PCWSTR(wide.as_ptr())];
    let mut id = 0i32;
    obj.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id)
        .map_err(|e| plain(format!("WIA: {name} fehlt ({})", e.message())))?;
    // IDispatch erwartet die Argumente in umgekehrter Reihenfolge
    args.reverse();
    let mut named = DISPID_PROPERTYPUT;
    let params = DISPPARAMS {
        rgvarg: if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() },
        rgdispidNamedArgs: if put { &mut named } else { std::ptr::null_mut() },
        cArgs: args.len() as u32,
        cNamedArgs: u32::from(put),
    };
    let mut result = VARIANT::default();
    let mut excep = EXCEPINFO::default();
    let mut arg_err = 0u32;
    match obj.Invoke(
        id,
        &GUID::zeroed(),
        LOCALE_USER_DEFAULT,
        flags,
        &params,
        if put { None } else { Some(&mut result) },
        Some(&mut excep),
        Some(&mut arg_err),
    ) {
        Ok(()) => Ok(result),
        Err(e) => {
            let code = if e.code().0 == DISP_E_EXCEPTION && excep.scode != 0 { excep.scode } else { e.code().0 } as u32;
            let detail = excep.bstrDescription.to_string();
            let message = error_text(code).map(str::to_string).unwrap_or_else(|| {
                let d = if detail.is_empty() { e.message() } else { detail };
                format!("Scannen fehlgeschlagen: {d} (0x{code:08X})")
            });
            Err(WiaError { code, message })
        }
    }
}

/// Fehlercodes der Windows-Bilderfassung
fn error_text(code: u32) -> Option<&'static str> {
    Some(match code {
        0x8021_0015 => "Kein Scanner gefunden. Ist er eingeschaltet und angeschlossen?",
        0x8021_0002 => "Papierstau im Scanner.",
        0x8021_0003 => "Kein Papier im Einzug.",
        0x8021_0004 => "Problem mit dem Papier im Scanner.",
        0x8021_0005 => "Der Scanner ist nicht erreichbar (offline).",
        0x8021_0006 => "Der Scanner ist gerade beschäftigt.",
        0x8021_0007 => "Der Scanner wärmt sich noch auf. Bitte gleich noch einmal versuchen.",
        0x8021_0008 => "Der Scanner braucht Ihre Aufmerksamkeit (siehe Gerät).",
        0x8021_000A => "Verbindung zum Scanner unterbrochen.",
        0x8021_0016 => "Die Abdeckung des Scanners ist offen.",
        0x8021_0017 => "Die Lampe des Scanners ist aus.",
        _ => return None,
    })
}

/// Leere Seite (z. B. unbedruckte Rückseite)? Nur für unkomprimierte
/// 24/32-Bit-BMP; alles andere gilt als nicht leer. Zählt dunkle Punkte
/// abseits der Ränder.
fn is_blank(bmp: &[u8]) -> bool {
    let u32_at = |o: usize| bmp.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let i32_at = |o: usize| u32_at(o).map(|v| v as i32);
    let u16_at = |o: usize| bmp.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if bmp.get(0..2) != Some(b"BM") {
        return false;
    }
    let (Some(off), Some(w), Some(h), Some(bpp), Some(comp)) =
        (u32_at(10), i32_at(18), i32_at(22), u16_at(28), u32_at(30))
    else {
        return false;
    };
    if comp != 0 || !(bpp == 24 || bpp == 32) || w <= 0 || h == 0 {
        return false;
    }
    let (w, h) = (w as usize, h.unsigned_abs() as usize);
    let px = bpp as usize / 8;
    let stride = (w * px).div_ceil(4) * 4;
    if bmp.len() < off as usize + stride * h {
        return false;
    }
    // 5 % Rand weglassen (Schatten der Blattkanten), jeden 2. Punkt prüfen
    let (mx, my) = (w / 20, h / 20);
    let (mut dark, mut total) = (0usize, 0usize);
    for y in (my..h - my).step_by(2) {
        let row = off as usize + y * stride;
        for x in (mx..w - mx).step_by(2) {
            let p = row + x * px;
            let lum = (bmp[p] as u32 * 114 + bmp[p + 1] as u32 * 587 + bmp[p + 2] as u32 * 299) / 1000;
            if lum < 140 {
                dark += 1;
            }
            total += 1;
        }
    }
    total > 0 && dark * 1000 < total * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(w: usize, h: usize, dark: &[(usize, usize)]) -> Vec<u8> {
        let stride = (w * 3).div_ceil(4) * 4;
        let mut v = vec![0u8; 54 + stride * h];
        v[0..2].copy_from_slice(b"BM");
        v[10..14].copy_from_slice(&54u32.to_le_bytes());
        v[14..18].copy_from_slice(&40u32.to_le_bytes());
        v[18..22].copy_from_slice(&(w as i32).to_le_bytes());
        v[22..26].copy_from_slice(&(h as i32).to_le_bytes());
        v[26..28].copy_from_slice(&1u16.to_le_bytes());
        v[28..30].copy_from_slice(&24u16.to_le_bytes());
        for y in 0..h {
            for x in 0..w {
                let p = 54 + y * stride + x * 3;
                let c = if dark.contains(&(x, y)) { 20 } else { 245 };
                v[p..p + 3].fill(c);
            }
        }
        v
    }

    #[test]
    fn blank_pages() {
        assert!(is_blank(&bmp(100, 140, &[])));
        // ein paar Staubkörner sind noch leer
        assert!(is_blank(&bmp(100, 140, &[(50, 50)])));
        // Text: eine Zeile dunkler Punkte
        let text: Vec<_> = (10..90).flat_map(|x| [(x, 70), (x, 71), (x, 72)]).collect();
        assert!(!is_blank(&bmp(100, 140, &text)));
        // keine BMP: nie leer
        assert!(!is_blank(b"\x89PNG"));
    }

    #[test]
    fn encode() {
        let s = Scanned { pages: vec![vec![1, 2, 3], vec![]], warning: Some("ä".into()), blank: 2 };
        let e = s.encode();
        assert_eq!(&e[0..4], b"BSCN");
        assert_eq!(u32::from_le_bytes(e[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(e[8..12].try_into().unwrap()), 3);
        assert_eq!(&e[12..15], &[1, 2, 3]);
        assert_eq!(u32::from_le_bytes(e[15..19].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(e[19..23].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(e[23..27].try_into().unwrap()), 2);
        assert_eq!(&e[27..], "ä".as_bytes());
    }
}
