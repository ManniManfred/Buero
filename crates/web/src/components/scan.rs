// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Manfred Roßkamp

//! Dokumente mit der Kamera scannen (Handy oder Webcam): Seiten aufnehmen,
//! Ränder automatisch erkennen und von Hand nachziehen, perspektivisch
//! entzerren, aufhellen und alles als eine PDF hochladen. Die Texterkennung
//! übernimmt danach wie bei jedem anderen Scan der Server.

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::components::upload::{pick_files, start_uploads};
use crate::icons::Icon;
use crate::state::{ctx, Ctx};

const VIDEO_ID: &str = "scan-video";
/// Merkt sich „Beidseitig“ (Desktop-Client, Scanner mit Einzug)
const KEY_DUPLEX: &str = "buero.scan.duplex";

/// Antwort des Desktop-Clients auf `scan_acquire`
struct Scanned {
    pages: Vec<web_sys::Blob>,
    blank: u32,
    warning: Option<String>,
}

/// Binärpaket lesen: `BSCN`, Seitenzahl, je Seite Länge + Daten, Anzahl
/// leerer Seiten, Hinweis (UTF-8); Zahlen als u32 (LE)
fn decode_scan(buf: &js_sys::ArrayBuffer) -> Option<Scanned> {
    let all = js_sys::Uint8Array::new(buf);
    let len = all.length();
    let num = |o: u32| -> Option<u32> {
        (o + 4 <= len).then(|| {
            let mut b = [0u8; 4];
            all.subarray(o, o + 4).copy_to(&mut b);
            u32::from_le_bytes(b)
        })
    };
    let mut magic = [0u8; 4];
    if len < 8 {
        return None;
    }
    all.subarray(0, 4).copy_to(&mut magic);
    if &magic != b"BSCN" {
        return None;
    }
    let count = num(4)?;
    let mut o = 8u32;
    let mut pages = Vec::new();
    for _ in 0..count {
        let n = num(o)?;
        o += 4;
        if o.checked_add(n)? > len {
            return None;
        }
        let part = all.subarray(o, o + n);
        pages.push(web_sys::Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&part)).ok()?);
        o += n;
    }
    let blank = num(o)?;
    let wl = num(o + 4)?;
    let start = o + 8;
    if start.checked_add(wl)? > len {
        return None;
    }
    let mut w = vec![0u8; wl as usize];
    all.subarray(start, start + wl).copy_to(&mut w);
    let warning = String::from_utf8(w).ok().filter(|w| !w.is_empty());
    Some(Scanned { pages, blank, warning })
}

#[wasm_bindgen(inline_js = r#"
let stream = null;
let generation = 0;

export function scan_camera_stop() {
  generation++;
  if (stream) {
    stream.getTracks().forEach((t) => t.stop());
    stream = null;
  }
  const v = document.getElementById("scan-video");
  if (v) v.srcObject = null;
}

// Startet die Rückkamera im <video>; false, wenn inzwischen wieder gestoppt wurde
export async function scan_camera_start(videoId) {
  scan_camera_stop();
  const mine = generation;
  if (!(navigator.mediaDevices && navigator.mediaDevices.getUserMedia)) {
    throw new Error(window.isSecureContext
      ? "Dieser Browser erlaubt keinen Zugriff auf die Kamera."
      : "Die Kamera lässt sich nur über eine sichere Verbindung (HTTPS) nutzen.");
  }
  let s;
  try {
    s = await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: { facingMode: { ideal: "environment" }, width: { ideal: 3840 }, height: { ideal: 2160 } },
    });
  } catch (e) {
    const n = e && e.name;
    if (n === "NotAllowedError" || n === "SecurityError") throw new Error("Der Zugriff auf die Kamera wurde nicht erlaubt.");
    if (n === "NotFoundError" || n === "OverconstrainedError") throw new Error("Keine Kamera gefunden.");
    if (n === "NotReadableError") throw new Error("Die Kamera wird gerade von einer anderen App benutzt.");
    throw new Error("Kamera nicht verfügbar: " + ((e && e.message) || e));
  }
  if (mine !== generation) { s.getTracks().forEach((t) => t.stop()); return false; }
  stream = s;
  const v = document.getElementById(videoId);
  if (!v) { scan_camera_stop(); return false; }
  v.muted = true;
  v.srcObject = s;
  try { await v.play(); } catch (e) {}
  if (!v.videoWidth) await new Promise((r) => { v.onloadedmetadata = r; setTimeout(r, 3000); });
  return mine === generation;
}

// Aktuelles Kamerabild als JPEG
export function scan_grab(videoId) {
  const v = document.getElementById(videoId);
  if (!v || !v.videoWidth) return Promise.reject(new Error("Die Kamera ist noch nicht bereit."));
  const c = document.createElement("canvas");
  c.width = v.videoWidth; c.height = v.videoHeight;
  c.getContext("2d").drawImage(v, 0, 0);
  return toBlob(c, 0.92);
}

function toBlob(canvas, quality) {
  return new Promise((res, rej) => canvas.toBlob(
    (b) => b ? res(b) : rej(new Error("Bild konnte nicht erzeugt werden.")), "image/jpeg", quality));
}

function loadImage(src) {
  return new Promise((res, rej) => {
    const i = new Image();
    i.onload = () => res(i);
    i.onerror = () => rej(new Error("Das Bild lässt sich nicht lesen."));
    i.src = src;
  });
}

// Bild übernehmen: [url, breite, höhe, 8 Eckkoordinaten (oben links, oben rechts, unten rechts, unten links)]
export async function scan_prepare(blob) {
  const url = URL.createObjectURL(blob);
  let img;
  try { img = await loadImage(url); } catch (e) { URL.revokeObjectURL(url); throw e; }
  const w = img.naturalWidth, h = img.naturalHeight;
  return [url, w, h, ...detect(img, w, h)];
}

// Sucht das Blatt: größte helle, wenig farbige Fläche (Otsu-Schwelle) und deren äußerste Punkte
function detect(img, w, h) {
  const full = [0, 0, w, 0, w, h, 0, h];
  try {
    const k = 220 / Math.max(w, h);
    const dw = Math.max(2, Math.round(w * k)), dh = Math.max(2, Math.round(h * k));
    const c = document.createElement("canvas");
    c.width = dw; c.height = dh;
    const x = c.getContext("2d", { willReadFrequently: true });
    x.drawImage(img, 0, 0, dw, dh);
    const d = x.getImageData(0, 0, dw, dh).data;
    const n = dw * dh;
    const val = new Uint8Array(n);
    const hist = new Float64Array(256);
    for (let i = 0, j = 0; i < n; i++, j += 4) {
      const r = d[j], g = d[j + 1], b = d[j + 2];
      const v = Math.max(0, Math.min(255, (r + g + b) / 3 - (Math.max(r, g, b) - Math.min(r, g, b)) * 0.8)) | 0;
      val[i] = v; hist[v]++;
    }
    let sum = 0;
    for (let i = 0; i < 256; i++) sum += i * hist[i];
    let sumB = 0, wB = 0, best = -1, t = 0, diff = 0;
    for (let i = 0; i < 256; i++) {
      wB += hist[i];
      if (!wB) continue;
      const wF = n - wB;
      if (!wF) break;
      sumB += i * hist[i];
      const mB = sumB / wB, mF = (sum - sumB) / wF;
      const between = wB * wF * (mB - mF) * (mB - mF);
      if (between > best) { best = between; t = i; diff = mF - mB; }
    }
    if (diff < 25) return full;
    const seen = new Uint8Array(n);
    const stack = new Int32Array(n);
    let bestSize = 0, q = null;
    for (let s = 0; s < n; s++) {
      if (seen[s] || val[s] <= t) continue;
      let top = 0, size = 0;
      let tl = Infinity, br = -Infinity, tr = -Infinity, bl = Infinity, ptl = 0, pbr = 0, ptr = 0, pbl = 0;
      stack[top++] = s; seen[s] = 1;
      while (top) {
        const p = stack[--top];
        size++;
        const px = p % dw, py = (p / dw) | 0;
        const a = px + py, b = px - py;
        if (a < tl) { tl = a; ptl = p; }
        if (a > br) { br = a; pbr = p; }
        if (b > tr) { tr = b; ptr = p; }
        if (b < bl) { bl = b; pbl = p; }
        const nb = [px > 0 ? p - 1 : -1, px < dw - 1 ? p + 1 : -1, py > 0 ? p - dw : -1, py < dh - 1 ? p + dw : -1];
        for (const o of nb) {
          if (o >= 0 && !seen[o] && val[o] > t) { seen[o] = 1; stack[top++] = o; }
        }
      }
      if (size > bestSize) { bestSize = size; q = [ptl, ptr, pbr, pbl]; }
    }
    if (!q || bestSize < 0.1 * n) return full;
    // Ecken leicht nach innen setzen, damit kein Streifen vom Hintergrund bleibt
    const off = [[-1, -1], [1, -1], [1, 1], [-1, 1]];
    const out = [];
    q.forEach((p, i) => {
      const px = (p % dw) + 0.5 + off[i][0] * -0.6, py = ((p / dw) | 0) + 0.5 + off[i][1] * -0.6;
      out.push(Math.max(0, Math.min(w, px / k)), Math.max(0, Math.min(h, py / k)));
    });
    let area = 0;
    for (let i = 0; i < 4; i++) {
      const j = (i + 1) % 4;
      area += out[2 * i] * out[2 * j + 1] - out[2 * j] * out[2 * i + 1];
    }
    if (Math.abs(area) / 2 < 0.1 * w * h) return full;
    return out;
  } catch (e) {
    return full;
  }
}

// Homographie Einheitsquadrat -> Viereck (Heckbert)
function homography(q) {
  const [x0, y0, x1, y1, x2, y2, x3, y3] = q;
  const dx1 = x1 - x2, dx2 = x3 - x2, dx3 = x0 - x1 + x2 - x3;
  const dy1 = y1 - y2, dy2 = y3 - y2, dy3 = y0 - y1 + y2 - y3;
  let g = 0, h = 0;
  const den = dx1 * dy2 - dx2 * dy1;
  if (Math.abs(den) > 1e-9) {
    g = (dx3 * dy2 - dx2 * dy3) / den;
    h = (dx1 * dy3 - dx3 * dy1) / den;
  }
  return [x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h];
}

const dist = (ax, ay, bx, by) => Math.hypot(ax - bx, ay - by);

// Hintergrund (Papierhelligkeit) blockweise schätzen: 90-%-Wert der Helligkeit
function background(lum, w, h) {
  const B = Math.max(8, Math.round(Math.max(w, h) / 48));
  const bw = Math.ceil(w / B), bh = Math.ceil(h / B);
  const hist = new Uint32Array(bw * bh * 32);
  for (let y = 0; y < h; y++) {
    const row = ((y / B) | 0) * bw;
    for (let x = 0; x < w; x++) {
      hist[(row + ((x / B) | 0)) * 32 + (lum[y * w + x] >> 3)]++;
    }
  }
  let bg = new Float32Array(bw * bh);
  for (let i = 0; i < bw * bh; i++) {
    let total = 0;
    for (let k = 0; k < 32; k++) total += hist[i * 32 + k];
    let acc = 0, v = 255;
    for (let k = 0; k < 32; k++) {
      acc += hist[i * 32 + k];
      if (acc >= total * 0.9) { v = k * 8 + 4; break; }
    }
    bg[i] = v;
  }
  // glätten
  const sm = new Float32Array(bw * bh);
  for (let y = 0; y < bh; y++) for (let x = 0; x < bw; x++) {
    let s = 0, c = 0;
    for (let yy = Math.max(0, y - 1); yy <= Math.min(bh - 1, y + 1); yy++)
      for (let xx = Math.max(0, x - 1); xx <= Math.min(bw - 1, x + 1); xx++) { s += bg[yy * bw + xx]; c++; }
    sm[y * bw + x] = s / c;
  }
  bg = sm;
  return (x, y) => {
    let fx = (x + 0.5) / B - 0.5, fy = (y + 0.5) / B - 0.5;
    fx = Math.max(0, Math.min(bw - 1, fx)); fy = Math.max(0, Math.min(bh - 1, fy));
    const x0 = fx | 0, y0 = fy | 0, x1 = Math.min(x0 + 1, bw - 1), y1 = Math.min(y0 + 1, bh - 1);
    const ax = fx - x0, ay = fy - y0;
    const top = bg[y0 * bw + x0] * (1 - ax) + bg[y0 * bw + x1] * ax;
    const bot = bg[y1 * bw + x0] * (1 - ax) + bg[y1 * bw + x1] * ax;
    return Math.max(48, top * (1 - ay) + bot * ay);
  };
}

function enhance(img, w, h, mode) {
  if (mode === "original") return;
  const d = img.data, n = w * h;
  const lum = new Uint8Array(n);
  for (let i = 0, j = 0; i < n; i++, j += 4) lum[i] = (d[j] * 299 + d[j + 1] * 587 + d[j + 2] * 114) / 1000;
  if (mode === "grau") {
    for (let i = 0, j = 0; i < n; i++, j += 4) d[j] = d[j + 1] = d[j + 2] = lum[i];
    return;
  }
  const bg = background(lum, w, h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const i = y * w + x, j = i * 4, f = 255 / bg(x, y);
    if (mode === "farbe") {
      for (let k = 0; k < 3; k++) d[j + k] = (d[j + k] * f - 25) * 255 / 215;
    } else {
      // Dokument: Papier weiß, Schrift kräftig
      const v = (lum[i] * f - 80) * 255 / 150;
      d[j] = d[j + 1] = d[j + 2] = v;
    }
  }
}

// Seite entzerren, aufbereiten, drehen: [url, breite, höhe]
export async function scan_render(url, corners, rot, mode) {
  const img = await loadImage(url);
  let W = img.naturalWidth, H = img.naturalHeight;
  let q = Array.from(corners);
  const lim = 4096 / Math.max(W, H);
  if (lim < 1) { W = Math.round(W * lim); H = Math.round(H * lim); q = q.map((v) => v * lim); }
  const sc = document.createElement("canvas");
  sc.width = W; sc.height = H;
  const sx = sc.getContext("2d", { willReadFrequently: true });
  sx.drawImage(img, 0, 0, W, H);
  const sd = sx.getImageData(0, 0, W, H).data;
  sc.width = sc.height = 0;

  let ow = Math.max(dist(q[0], q[1], q[2], q[3]), dist(q[6], q[7], q[4], q[5]));
  let oh = Math.max(dist(q[0], q[1], q[6], q[7]), dist(q[2], q[3], q[4], q[5]));
  const s = Math.min(1, 2480 / Math.max(ow, oh, 1));
  ow = Math.max(1, Math.round(ow * s)); oh = Math.max(1, Math.round(oh * s));
  const [a, b, c, d, e, f, g, h] = homography(q);
  const out = new ImageData(ow, oh);
  const od = out.data;
  for (let y = 0, o = 0; y < oh; y++) {
    const v = (y + 0.5) / oh;
    for (let x = 0; x < ow; x++, o += 4) {
      const u = (x + 0.5) / ow;
      const z = g * u + h * v + 1;
      let px = (a * u + b * v + c) / z - 0.5, py = (d * u + e * v + f) / z - 0.5;
      px = px < 0 ? 0 : px > W - 1 ? W - 1 : px;
      py = py < 0 ? 0 : py > H - 1 ? H - 1 : py;
      const x0 = px | 0, y0 = py | 0, x1 = Math.min(x0 + 1, W - 1), y1 = Math.min(y0 + 1, H - 1);
      const fx = px - x0, fy = py - y0;
      const i00 = (y0 * W + x0) * 4, i10 = (y0 * W + x1) * 4, i01 = (y1 * W + x0) * 4, i11 = (y1 * W + x1) * 4;
      for (let k = 0; k < 3; k++) {
        od[o + k] = (sd[i00 + k] * (1 - fx) + sd[i10 + k] * fx) * (1 - fy) + (sd[i01 + k] * (1 - fx) + sd[i11 + k] * fx) * fy;
      }
      od[o + 3] = 255;
    }
  }
  enhance(out, ow, oh, mode);

  const pc = document.createElement("canvas");
  pc.width = ow; pc.height = oh;
  pc.getContext("2d").putImageData(out, 0, 0);
  const r = ((rot % 4) + 4) % 4;
  let res = pc;
  if (r) {
    res = document.createElement("canvas");
    res.width = r % 2 ? oh : ow; res.height = r % 2 ? ow : oh;
    const rx = res.getContext("2d");
    if (r === 1) rx.translate(oh, 0);
    if (r === 2) rx.translate(ow, oh);
    if (r === 3) rx.translate(0, ow);
    rx.rotate(r * Math.PI / 2);
    rx.drawImage(pc, 0, 0);
  }
  const blob = await toBlob(res, 0.85);
  return [URL.createObjectURL(blob), res.width, res.height];
}

// Mehrseitige PDF aus JPEG-Seiten ([url, breite, höhe]); jede Seite passt in A4
export async function scan_pdf(pages, name) {
  const enc = new TextEncoder();
  const parts = [];
  const offsets = [];
  let len = 0;
  const push = (x) => { const b = typeof x === "string" ? enc.encode(x) : x; parts.push(b); len += b.length; };
  const obj = (num, ...body) => { offsets[num] = len; push(num + " 0 obj\n"); body.forEach(push); push("\nendobj\n"); };
  push("%PDF-1.4\n");
  push(new Uint8Array([37, 226, 227, 207, 211, 10]));
  const n = pages.length;
  const kids = pages.map((_, i) => (3 + 3 * i) + " 0 R").join(" ");
  obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
  obj(2, "<< /Type /Pages /Kids [" + kids + "] /Count " + n + " >>");
  for (let i = 0; i < n; i++) {
    const [url, w, h] = pages[i];
    const data = new Uint8Array(await (await fetch(url)).arrayBuffer());
    const land = w > h;
    const bw = land ? 841.89 : 595.28, bh = land ? 595.28 : 841.89;
    const s = Math.min(bw / w, bh / h);
    const pw = (w * s).toFixed(2), ph = (h * s).toFixed(2);
    const p = 3 + 3 * i;
    obj(p, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 " + pw + " " + ph + "] /Resources << /XObject << /Im0 "
      + (p + 2) + " 0 R >> >> /Contents " + (p + 1) + " 0 R >>");
    const content = "q " + pw + " 0 0 " + ph + " 0 0 cm /Im0 Do Q";
    obj(p + 1, "<< /Length " + content.length + " >>\nstream\n" + content + "\nendstream");
    obj(p + 2, "<< /Type /XObject /Subtype /Image /Width " + w + " /Height " + h
      + " /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length " + data.length + " >>\nstream\n",
      data, "\nendstream");
  }
  const size = 3 + 3 * n;
  const xref = len;
  let x = "xref\n0 " + size + "\n0000000000 65535 f \n";
  for (let i = 1; i < size; i++) x += String(offsets[i]).padStart(10, "0") + " 00000 n \n";
  push(x + "trailer\n<< /Size " + size + " /Root 1 0 R >>\nstartxref\n" + xref + "\n%%EOF\n");
  return new File(parts, name, { type: "application/pdf" });
}

export function scan_free(url) {
  try { URL.revokeObjectURL(url); } catch (e) {}
}
"#)]
extern "C" {
    fn scan_camera_start(video_id: &str) -> js_sys::Promise;
    fn scan_camera_stop();
    fn scan_grab(video_id: &str) -> js_sys::Promise;
    fn scan_prepare(blob: &web_sys::Blob) -> js_sys::Promise;
    fn scan_render(url: &str, corners: &[f64], rot: u32, mode: &str) -> js_sys::Promise;
    fn scan_pdf(pages: &js_sys::Array, name: &str) -> js_sys::Promise;
    fn scan_free(url: &str);
}

async fn call(p: js_sys::Promise) -> Result<JsValue, String> {
    JsFuture::from(p).await.map_err(|e| {
        e.dyn_ref::<js_sys::Error>()
            .map(|e| String::from(e.message()))
            .or_else(|| e.as_string())
            .unwrap_or_else(|| "Unbekannter Fehler".into())
    })
}

const FILTERS: [(&str, &str); 4] = [
    ("dokument", "Dokument"),
    ("farbe", "Farbe"),
    ("grau", "Graustufen"),
    ("original", "Original"),
];

#[derive(Clone, Debug, PartialEq)]
struct Page {
    id: u32,
    /// Aufnahme (Object-URL) und ihre Größe
    src: String,
    w: f64,
    h: f64,
    /// Ecken des Blatts: oben links, oben rechts, unten rechts, unten links
    q: [f64; 8],
    /// automatisch erkannte Ecken
    auto: [f64; 8],
    /// Drehung in Vierteldrehungen im Uhrzeigersinn
    rot: u32,
    filter: String,
    /// fertige Seite (Object-URL, Breite, Höhe)
    out: Option<(String, f64, f64)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Camera,
    Pages,
    Crop(u32),
}

#[derive(Clone, Copy)]
struct Scan {
    c: Ctx,
    pages: RwSignal<Vec<Page>>,
    filter: RwSignal<String>,
    next_id: RwSignal<u32>,
    busy: RwSignal<u32>,
}

impl Scan {
    /// Neues Bild als Seite übernehmen
    async fn add(self, blob: web_sys::Blob) {
        match call(scan_prepare(&blob)).await {
            Ok(v) => {
                let a: js_sys::Array = v.unchecked_into();
                let num = |i: u32| a.get(i).as_f64().unwrap_or(0.0);
                let src = a.get(0).as_string().unwrap_or_default();
                let mut q = [0.0; 8];
                for (i, x) in q.iter_mut().enumerate() {
                    *x = num(3 + i as u32);
                }
                let Some(id) = self.next_id.try_update(|n| {
                    *n += 1;
                    *n
                }) else {
                    scan_free(&src);
                    return;
                };
                let page = Page {
                    id,
                    src: src.clone(),
                    w: num(1),
                    h: num(2),
                    q,
                    auto: q,
                    rot: 0,
                    filter: self.filter.get_untracked(),
                    out: None,
                };
                if self.pages.try_update(|p| p.push(page)).is_none() {
                    scan_free(&src);
                    return;
                }
                self.render(id).await;
            }
            Err(e) => self.c.notify(e, true),
        }
    }

    /// Seite (neu) berechnen
    async fn render(self, id: u32) {
        let Some(Some(p)) = self
            .pages
            .try_with_untracked(|ps| ps.iter().find(|p| p.id == id).cloned())
        else {
            return;
        };
        match call(scan_render(&p.src, &p.q, p.rot, &p.filter)).await {
            Ok(v) => {
                let a: js_sys::Array = v.unchecked_into();
                let url = a.get(0).as_string().unwrap_or_default();
                let (w, h) = (
                    a.get(1).as_f64().unwrap_or(1.0),
                    a.get(2).as_f64().unwrap_or(1.0),
                );
                let mut taken = false;
                self.pages.try_update(|ps| {
                    // nur übernehmen, wenn sich die Einstellungen inzwischen nicht geändert haben
                    if let Some(x) = ps.iter_mut().find(|x| x.id == id) {
                        if x.q == p.q && x.rot == p.rot && x.filter == p.filter {
                            if let Some((old, ..)) = x.out.take() {
                                scan_free(&old);
                            }
                            x.out = Some((url.clone(), w, h));
                            taken = true;
                        }
                    }
                });
                if !taken {
                    scan_free(&url);
                }
            }
            Err(e) => self.c.notify(e, true),
        }
    }

    fn change(self, id: u32, f: impl FnOnce(&mut Page)) {
        self.pages.update(|ps| {
            if let Some(p) = ps.iter_mut().find(|p| p.id == id) {
                f(p);
            }
        });
        spawn_local(self.render(id));
    }

    fn remove(self, id: u32) {
        self.pages.update(|ps| {
            if let Some(i) = ps.iter().position(|p| p.id == id) {
                let p = ps.remove(i);
                free_page(&p);
            }
        });
    }

    /// Bilder nacheinander übernehmen (Reihenfolge bleibt erhalten)
    fn add_files(self, files: Vec<web_sys::File>) {
        if files.is_empty() {
            return;
        }
        self.busy.update(|b| *b += 1);
        spawn_local(async move {
            for f in files {
                self.add(f.into()).await;
            }
            self.busy.try_update(|b| *b = b.saturating_sub(1));
        });
    }
}

fn free_page(p: &Page) {
    scan_free(&p.src);
    if let Some((u, ..)) = &p.out {
        scan_free(u);
    }
}

fn default_title() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "Scan {:04}-{:02}-{:02} {:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

fn file_name(title: &str) -> String {
    let t: String = title
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let t = if t.is_empty() { default_title() } else { t };
    if t.to_lowercase().ends_with(".pdf") {
        t
    } else {
        format!("{t}.pdf")
    }
}

#[component]
pub fn Scanner() -> impl IntoView {
    let c = ctx();
    let s = Scan {
        c,
        pages: RwSignal::new(Vec::new()),
        filter: RwSignal::new("dokument".to_string()),
        next_id: RwSignal::new(0),
        busy: RwSignal::new(0),
    };
    let pages = s.pages;
    // Desktop-Client: Scanner (Windows-Scan-Dialog) statt Kamera als Standard
    let desktop = crate::api::is_desktop();
    let mode = RwSignal::new(if desktop { Mode::Pages } else { Mode::Camera });
    let scanning = RwSignal::new(false);
    let cam_err = RwSignal::new(None::<String>);
    let cam_ready = RwSignal::new(false);
    let flash = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let title = RwSignal::new(default_title());
    let gallery = NodeRef::<leptos::html::Input>::new();
    let camera_app = NodeRef::<leptos::html::Input>::new();

    // Kamera nur in der Aufnahme-Ansicht laufen lassen
    Effect::new(move |_| {
        if mode.get() == Mode::Camera {
            cam_err.set(None);
            cam_ready.set(false);
            spawn_local(async move {
                match call(scan_camera_start(VIDEO_ID)).await {
                    Ok(v) => {
                        if v.as_bool() == Some(true) {
                            cam_ready.try_set(true);
                        }
                    }
                    Err(e) => {
                        cam_err.try_set(Some(e));
                    }
                }
            });
        } else {
            scan_camera_stop();
            cam_ready.set(false);
        }
    });

    on_cleanup(move || {
        scan_camera_stop();
        pages.try_with_untracked(|ps| ps.iter().for_each(free_page));
    });

    let close = move || {
        let n = pages.with_untracked(|p| p.len());
        if n == 0 {
            c.scan.set(false);
            return;
        }
        let what = if n == 1 { "Die gescannte Seite".to_string() } else { format!("Die {n} gescannten Seiten") };
        crate::dialog::confirm_then(
            crate::dialog::confirm("Scan verwerfen?", format!("{what} gehen verloren.")).ok("Verwerfen").danger(),
            move || c.scan.set(false),
        );
    };

    let capture = move || {
        if !cam_ready.get_untracked() {
            return;
        }
        flash.set(true);
        set_timeout(
            move || {
                flash.try_set(false);
            },
            std::time::Duration::from_millis(180),
        );
        s.busy.update(|b| *b += 1);
        spawn_local(async move {
            match call(scan_grab(VIDEO_ID)).await {
                Ok(b) => s.add(b.unchecked_into()).await,
                Err(e) => c.notify(e, true),
            }
            s.busy.try_update(|b| *b = b.saturating_sub(1));
        });
    };

    // Vom Scanner (Desktop-Client): Einzug Blatt für Blatt, Flachbett über
    // den Scan-Dialog von Windows
    let duplex = RwSignal::new(crate::api::load(KEY_DUPLEX).as_deref() == Some("1"));
    let from_scanner = move || {
        if scanning.get_untracked() {
            return;
        }
        scanning.set(true);
        s.busy.update(|b| *b += 1);
        let args = serde_json::json!({ "duplex": duplex.get_untracked() });
        spawn_local(async move {
            match crate::api::invoke_js("scan_acquire", args).await {
                Ok(v) => match v.dyn_into::<js_sys::ArrayBuffer>().ok().and_then(|b| decode_scan(&b)) {
                    Some(r) => {
                        if !r.pages.is_empty() {
                            mode.try_set(Mode::Pages);
                        }
                        for p in r.pages {
                            s.add(p).await;
                        }
                        if let Some(w) = r.warning {
                            c.notify(w, true);
                        } else if r.blank > 0 {
                            let what = if r.blank == 1 { "1 leere Seite".to_string() } else { format!("{} leere Seiten", r.blank) };
                            c.notify(format!("{what} weggelassen."), false);
                        }
                    }
                    None => c.notify("Unerwartete Antwort vom Scanner.", true),
                },
                Err(e) => c.notify(e, true),
            }
            scanning.try_set(false);
            s.busy.try_update(|b| *b = b.saturating_sub(1));
        });
    };

    let set_filter = move |f: String| {
        s.filter.set(f.clone());
        let ids: Vec<u32> =
            pages.with_untracked(|ps| ps.iter().filter(|p| p.filter != f).map(|p| p.id).collect());
        for id in ids {
            let f = f.clone();
            s.change(id, move |p| p.filter = f);
        }
    };

    let save = move || {
        if s.busy.get_untracked() > 0 {
            c.notify("Die Aufnahmen werden noch verarbeitet …", false);
            return;
        }
        let list =
            pages.with_untracked(|ps| ps.iter().map(|p| p.out.clone()).collect::<Option<Vec<_>>>());
        let Some(list) = list else {
            c.notify("Die Seiten werden noch berechnet …", false);
            return;
        };
        if list.is_empty() {
            return;
        }
        saving.set(true);
        let name = file_name(&title.get_untracked());
        spawn_local(async move {
            let arr = js_sys::Array::new();
            for (u, w, h) in list {
                arr.push(&js_sys::Array::of3(&u.into(), &w.into(), &h.into()));
            }
            match call(scan_pdf(&arr, &name)).await {
                Ok(f) => {
                    start_uploads(c, vec![f.unchecked_into()]);
                    c.scan.set(false);
                }
                Err(e) => {
                    c.notify(e, true);
                    saving.try_set(false);
                }
            }
        });
    };

    let on_files = move |input: NodeRef<leptos::html::Input>| {
        if let Some(i) = input.get() {
            let files = pick_files(i.files());
            i.set_value("");
            if !files.is_empty() {
                s.add_files(files);
                if mode.get_untracked() == Mode::Camera && cam_err.get_untracked().is_some() {
                    mode.set(Mode::Pages);
                }
            }
        }
    };

    let page_count = move || pages.with(|p| p.len());
    let target = move || match c.collection.get() {
        Some(id) => format!("Wird in „{}“ gespeichert", c.collection_name(id)),
        None => "Wird im Eingang gespeichert".to_string(),
    };

    view! {
        <div class="scan">
            <input type="file" accept="image/*" multiple style="display:none" node_ref=gallery
                on:change=move |_| on_files(gallery) />
            <input type="file" accept="image/*" capture="environment" style="display:none" node_ref=camera_app
                on:change=move |_| on_files(camera_app) />

            // Aufnahme
            <div class="scan-view" style:display=move || if mode.get() == Mode::Camera { "flex" } else { "none" }>
                <div class="scan-bar">
                    <button class="icon" title="Schließen" aria-label="Schließen" on:click=move |_| close()><Icon name="x" /></button>
                    <span class="grow"><b>"Dokument scannen"</b></span>
                    <Show when=move || { s.busy.get() > 0 }><span class="scan-spin"></span></Show>
                </div>
                <div class="scan-stage">
                    <video id=VIDEO_ID autoplay playsinline muted prop:muted=true
                        style:display=move || if cam_err.get().is_some() { "none" } else { "block" }></video>
                    <Show when=move || !cam_ready.get() && cam_err.get().is_none()>
                        <div class="scan-msg">"Kamera wird gestartet …"</div>
                    </Show>
                    {move || cam_err.get().map(|e| view! {
                        <div class="scan-msg">
                            <Icon name="camera" class="scan-msg-icon" />
                            <p>{e}</p>
                            <p class="small">"Fotos lassen sich trotzdem mit der Kamera-App aufnehmen oder aus der Galerie wählen."</p>
                            <div class="button-row">
                                <button class="primary" on:click=move |_| { if let Some(i) = camera_app.get() { i.click(); } }>
                                    <Icon name="camera" />"Foto aufnehmen"
                                </button>
                                <button on:click=move |_| { if let Some(i) = gallery.get() { i.click(); } }>
                                    <Icon name="image-plus" />"Bilder wählen"
                                </button>
                            </div>
                        </div>
                    })}
                    <Show when=move || flash.get()><div class="scan-flash"></div></Show>
                </div>
                <div class="scan-controls">
                    <button class="scan-side" title="Bilder aus der Galerie" on:click=move |_| { if let Some(i) = gallery.get() { i.click(); } }>
                        <Icon name="image-plus" />
                    </button>
                    <button class="shutter" title="Aufnehmen" aria-label="Aufnehmen"
                        disabled=move || !cam_ready.get() on:click=move |_| capture()></button>
                    <button class="scan-side scan-next" title="Seiten ansehen und speichern"
                        disabled=move || !desktop && page_count() == 0 on:click=move |_| mode.set(Mode::Pages)>
                        {move || pages.with(|ps| ps.last().and_then(|p| p.out.clone())).map(|(u, ..)| view! { <img src=u alt="" /> })}
                        <span class="scan-count">{page_count}</span>
                    </button>
                </div>
            </div>

            // Seitenübersicht
            <Show when=move || mode.get() == Mode::Pages>
                <div class="scan-view">
                    <div class="scan-bar">
                        <button class="icon" title="Schließen" aria-label="Schließen" on:click=move |_| close()><Icon name="x" /></button>
                        <span class="grow"><b>{move || match page_count() { 1 => "1 Seite".to_string(), n => format!("{n} Seiten") }}</b></span>
                        <Show when=move || { s.busy.get() > 0 }><span class="scan-spin"></span></Show>
                    </div>
                    <div class="scan-pages">
                        <For each=move || pages.get() key=|p| (p.id, p.out.as_ref().map(|o| o.0.clone())) let:p>
                            {
                                let id = p.id;
                                let nr = move || pages.with(|ps| ps.iter().position(|x| x.id == id).map(|i| i + 1).unwrap_or(0));
                                view! {
                                    <div class="scan-page">
                                        <div class="scan-thumb" on:click=move |_| mode.set(Mode::Crop(id))>
                                            {match p.out.clone() {
                                                Some((u, ..)) => view! { <img src=u alt="" /> }.into_any(),
                                                None => view! { <span class="scan-spin"></span> }.into_any(),
                                            }}
                                            <span class="scan-nr">{nr}</span>
                                        </div>
                                        <div class="scan-acts">
                                            <button class="icon" title="Zuschneiden" on:click=move |_| mode.set(Mode::Crop(id))><Icon name="crop" /></button>
                                            <button class="icon" title="Drehen" on:click=move |_| s.change(id, |p| p.rot = (p.rot + 1) % 4)><Icon name="rotate" /></button>
                                            <button class="icon" title="Nach vorne"
                                                disabled=move || nr() <= 1
                                                on:click=move |_| pages.update(|ps| {
                                                    if let Some(i) = ps.iter().position(|x| x.id == id) {
                                                        if i > 0 { ps.swap(i, i - 1); }
                                                    }
                                                })>
                                                <span class="flip"><Icon name="chevron-right" /></span>
                                            </button>
                                            <button class="icon danger" title="Seite löschen" on:click=move |_| s.remove(id)><Icon name="trash" /></button>
                                        </div>
                                    </div>
                                }
                            }
                        </For>
                        {if desktop {
                            view! {
                                <button class="scan-add" disabled=move || scanning.get() on:click=move |_| from_scanner()>
                                    <Icon name="scanner" />
                                    {move || if scanning.get() { "Scanner läuft …" } else if page_count() == 0 { "Vom Scanner scannen" } else { "Weiter scannen" }}
                                </button>
                                <div class="scan-add-more">
                                    <label class="scan-check" title="Bei Scannern mit Einzug auch die Rückseiten scannen; leere Rückseiten fallen weg">
                                        <input type="checkbox" prop:checked=move || duplex.get()
                                            on:change=move |ev| {
                                                let on = event_target_checked(&ev);
                                                duplex.set(on);
                                                crate::api::save(KEY_DUPLEX, on.then_some("1"));
                                            } />
                                        "Beidseitig"
                                    </label>
                                    <button on:click=move |_| mode.set(Mode::Camera)><Icon name="camera" />"Kamera"</button>
                                    <button on:click=move |_| { if let Some(i) = gallery.get() { i.click(); } }><Icon name="image-plus" />"Bilder wählen"</button>
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <button class="scan-add" on:click=move |_| mode.set(Mode::Camera)>
                                    <Icon name="plus" />"Seite hinzufügen"
                                </button>
                            }.into_any()
                        }}
                    </div>
                    <div class="scan-foot">
                        <input class="grow" type="text" placeholder="Titel" aria-label="Titel"
                            prop:value=move || title.get() on:input=move |ev| title.set(event_target_value(&ev)) />
                        <select aria-label="Bildfilter" prop:value=move || s.filter.get()
                            on:change=move |ev| set_filter(event_target_value(&ev))>
                            {FILTERS.iter().map(|(v, l)| view! { <option value=*v>{*l}</option> }).collect_view()}
                        </select>
                        <button class="primary" disabled=move || saving.get() || page_count() == 0 on:click=move |_| save()>
                            <Icon name="upload" />{move || if saving.get() { "Speichert …" } else { "Als PDF speichern" }}
                        </button>
                        <span class="small scan-target">{target}</span>
                    </div>
                </div>
            </Show>

            // Zuschneiden
            {move || match mode.get() {
                Mode::Crop(id) => pages.with_untracked(|ps| ps.iter().find(|p| p.id == id).cloned()).map(|p| view! {
                    <CropEditor page=p on_done=Callback::new(move |q: Option<[f64; 8]>| {
                        if let Some(q) = q {
                            s.change(id, |p| p.q = q);
                        }
                        mode.set(Mode::Pages);
                    }) />
                }),
                _ => None,
            }}
        </div>
    }
}

/// Ecken des Blatts von Hand verschieben (mit Lupe)
#[component]
fn CropEditor(page: Page, on_done: Callback<Option<[f64; 8]>>) -> impl IntoView {
    let (w, h) = (page.w.max(1.0), page.h.max(1.0));
    let q = RwSignal::new(page.q);
    let drag = RwSignal::new(None::<usize>);
    // Anzeigegröße in Bildschirmpixeln (für die Lupe)
    let shown = RwSignal::new((0.0f64, 0.0f64));
    let svg = NodeRef::<leptos::svg::Svg>::new();
    let full = [0.0, 0.0, w, 0.0, w, h, 0.0, h];
    let auto = page.auto;
    let src = page.src.clone();

    let to_img = move |ev: &web_sys::PointerEvent| -> Option<(f64, f64, f64)> {
        let el = svg.get_untracked()?;
        let r = el.get_bounding_client_rect();
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return None;
        }
        shown.set((r.width(), r.height()));
        let k = w / r.width();
        Some((
            (ev.client_x() as f64 - r.left()) * k,
            (ev.client_y() as f64 - r.top()) * (h / r.height()),
            k,
        ))
    };

    let down = move |ev: web_sys::PointerEvent| {
        let Some((x, y, k)) = to_img(&ev) else { return };
        let cur = q.get_untracked();
        let (i, d) = (0..4)
            .map(|i| {
                (
                    i,
                    ((cur[2 * i] - x).powi(2) + (cur[2 * i + 1] - y).powi(2)).sqrt(),
                )
            })
            .fold((0, f64::MAX), |a, b| if b.1 < a.1 { b } else { a });
        if d / k < 60.0 {
            ev.prevent_default();
            if let Some(el) = svg.get_untracked() {
                let _ = el.set_pointer_capture(ev.pointer_id());
            }
            drag.set(Some(i));
        }
    };
    let moved = move |ev: web_sys::PointerEvent| {
        let Some(i) = drag.get_untracked() else {
            return;
        };
        let Some((x, y, _)) = to_img(&ev) else { return };
        ev.prevent_default();
        q.update(|q| {
            q[2 * i] = x.clamp(0.0, w);
            q[2 * i + 1] = y.clamp(0.0, h);
        });
    };
    let up = move |_ev: web_sys::PointerEvent| drag.set(None);

    let r = w.max(h) * 0.022;
    let points = move || {
        let q = q.get();
        format!(
            "{},{} {},{} {},{} {},{}",
            q[0], q[1], q[2], q[3], q[4], q[5], q[6], q[7]
        )
    };
    let shade = move || {
        let q = q.get();
        format!(
            "M0 0H{w}V{h}H0Z M{} {}L{} {}L{} {}L{} {}Z",
            q[0], q[1], q[2], q[3], q[4], q[5], q[6], q[7]
        )
    };
    let loupe_src = src.clone();
    let loupe = move || {
        let i = drag.get()?;
        let (dw, dh) = shown.get();
        let q = q.get();
        const Z: f64 = 2.5;
        const S: f64 = 110.0;
        let (px, py) = (q[2 * i] / w * dw * Z, q[2 * i + 1] / h * dh * Z);
        // Lupe auf der gegenüberliegenden Seite, damit der Finger sie nicht verdeckt
        let side = if q[2 * i] < w / 2.0 { "right" } else { "left" };
        Some(view! {
            <div class="scan-loupe" style=format!(
                "{side}: 8px; width: {S}px; height: {S}px; background-image: url('{}'); background-size: {}px {}px; background-position: {}px {}px;",
                loupe_src, dw * Z, dh * Z, S / 2.0 - px, S / 2.0 - py
            )></div>
        })
    };

    view! {
        <div class="scan-view">
            <div class="scan-bar">
                <button class="icon" title="Abbrechen" aria-label="Abbrechen" on:click=move |_| on_done.run(None)><Icon name="x" /></button>
                <span class="grow"><b>"Zuschneiden"</b></span>
                <button class="primary" on:click=move |_| on_done.run(Some(q.get_untracked()))><Icon name="check" />"Übernehmen"</button>
            </div>
            <div class="scan-stage">
                <div class="scan-crop" style=format!("aspect-ratio: {w} / {h}; width: min(100%, calc((100dvh - 170px) * {}));", w / h)>
                    <img src=src alt="" draggable="false" />
                    <svg node_ref=svg viewBox=format!("0 0 {w} {h}") preserveAspectRatio="none"
                        on:pointerdown=down on:pointermove=moved on:pointerup=up on:pointercancel=up>
                        <path class="quad-shade" fill-rule="evenodd" d=shade />
                        <polygon class="quad-line" points=points vector-effect="non-scaling-stroke" />
                        {(0..4).map(|i| view! {
                            <circle class="quad-handle" class:active=move || drag.get() == Some(i)
                                cx=move || q.get()[2 * i] cy=move || q.get()[2 * i + 1] r=r
                                vector-effect="non-scaling-stroke" />
                        }).collect_view()}
                    </svg>
                    {loupe}
                </div>
            </div>
            <div class="scan-foot center-row">
                <button on:click=move |_| q.set(auto)>"Automatisch"</button>
                <button on:click=move |_| q.set(full)>"Ganzes Bild"</button>
                <span class="small muted">"Ecken auf die Blattecken ziehen"</span>
            </div>
        </div>
    }
}
