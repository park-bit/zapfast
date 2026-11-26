//! Bounded, off-thread rendering of downloaded PDF attachments.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread;

use egui::ColorImage;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PAGES: usize = 2_000;
pub const MAX_OBJECTS: usize = 100_000;
pub const MAX_RENDER_SIDE: f32 = 4096.0;
pub const MAX_RENDER_PIXELS: f32 = 8_000_000.0;

const MAX_RENDER_SCALE: f32 = 4.0;
const MAX_PAGE_POINTS: f32 = 100_000.0;

#[derive(Debug)]
pub enum Event {
    Ready(Vec<(f32, f32)>),
    Rendered {
        request: u64,
        page: usize,
        image: ColorImage,
    },
    Failed {
        request: Option<u64>,
        message: &'static str,
    },
}

#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub page: usize,
    pub scale: f32,
}

pub struct Worker {
    requests: SyncSender<Request>,
    events: Receiver<Event>,
}

impl Worker {
    pub fn spawn(path: &Path, ctx: egui::Context) -> Result<Self, std::io::Error> {
        let (request_tx, request_rx) = sync_channel(1);
        let (event_tx, event_rx) = sync_channel(1);
        let path = path.to_owned();
        thread::Builder::new()
            .name("zapfast-pdf-preview".into())
            .spawn(move || run(path, request_rx, event_tx, ctx))?;
        Ok(Self {
            requests: request_tx,
            events: event_rx,
        })
    }

    pub fn request(&self, request: Request) -> bool {
        self.requests.try_send(request).is_ok()
    }

    pub fn next_event(&self) -> Result<Event, TryRecvError> {
        self.events.try_recv()
    }
}

fn run(
    path: std::path::PathBuf,
    requests: Receiver<Request>,
    events: SyncSender<Event>,
    ctx: egui::Context,
) {
    let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load(&path)));
    let pdf = match loaded {
        Ok(Ok(pdf)) => pdf,
        Ok(Err(message)) => {
            let _ = events.send(Event::Failed {
                request: None,
                message,
            });
            ctx.request_repaint();
            return;
        }
        Err(_) => {
            let _ = events.send(Event::Failed {
                request: None,
                message: "This PDF could not be read.",
            });
            ctx.request_repaint();
            return;
        }
    };

    let pages = pdf.pages();
    let dimensions: Vec<_> = pages.iter().map(|page| page.render_dimensions()).collect();
    if dimensions.is_empty() {
        let _ = events.send(Event::Failed {
            request: None,
            message: "This PDF has no pages.",
        });
        ctx.request_repaint();
        return;
    }
    if dimensions.len() > MAX_PAGES {
        let _ = events.send(Event::Failed {
            request: None,
            message: "This PDF has too many pages to preview.",
        });
        ctx.request_repaint();
        return;
    }
    if dimensions
        .iter()
        .any(|&(width, height)| !valid_dimensions(width, height))
    {
        let _ = events.send(Event::Failed {
            request: None,
            message: "This PDF has an invalid page size.",
        });
        ctx.request_repaint();
        return;
    }
    if events.send(Event::Ready(dimensions)).is_err() {
        return;
    }
    ctx.request_repaint();

    let interpreter = InterpreterSettings::default();
    let settings = RenderSettings::default();
    while let Ok(request) = requests.recv() {
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_page(
                &pages[request.page],
                // Hayro cannot clear its cache. Don't retain resources from every page read.
                &RenderCache::new(),
                &interpreter,
                &settings,
                request.scale,
            )
        }));
        match rendered {
            Ok(Some(image)) => {
                if events
                    .send(Event::Rendered {
                        request: request.id,
                        page: request.page,
                        image,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(None) => {
                if events
                    .send(Event::Failed {
                        request: Some(request.id),
                        message: "This PDF page could not be rendered.",
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => {
                if events
                    .send(Event::Failed {
                        request: Some(request.id),
                        message: "This PDF page could not be rendered.",
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
        ctx.request_repaint();
    }
}

fn load(path: &Path) -> Result<Pdf, &'static str> {
    let mut file = std::fs::File::open(path).map_err(|_| "This PDF is no longer available.")?;
    if file
        .metadata()
        .map_err(|_| "This PDF could not be read.")?
        .len()
        > MAX_FILE_BYTES
    {
        return Err("This PDF is too large to preview.");
    }
    let mut bytes = Vec::new();
    Read::take(&mut file, MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "This PDF could not be read.")?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("This PDF is too large to preview.");
    }
    let pdf = Pdf::new(bytes).map_err(|_| "This file is not a readable PDF.")?;
    if pdf.len() > MAX_OBJECTS {
        return Err("This PDF is too complex to preview.");
    }
    Ok(pdf)
}

fn valid_dimensions(width: f32, height: f32) -> bool {
    width.is_finite()
        && height.is_finite()
        && width > 0.0
        && height > 0.0
        && width <= MAX_PAGE_POINTS
        && height <= MAX_PAGE_POINTS
}

pub fn render_scale(width: f32, height: f32, requested: f32) -> Option<f32> {
    if !valid_dimensions(width, height) || !requested.is_finite() || requested <= 0.0 {
        return None;
    }
    let area = f64::from(width) * f64::from(height);
    let area_scale = (f64::from(MAX_RENDER_PIXELS) / area).sqrt() as f32;
    let scale = requested
        .min(MAX_RENDER_SCALE)
        .min(MAX_RENDER_SIDE / width.max(height))
        .min(area_scale);
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

fn render_page<'a>(
    page: &'a hayro::hayro_syntax::page::Page<'a>,
    cache: &RenderCache<'a>,
    interpreter: &InterpreterSettings,
    settings: &RenderSettings,
    requested_scale: f32,
) -> Option<ColorImage> {
    let (width, height) = page.render_dimensions();
    let scale = render_scale(width, height, requested_scale)?;
    let pixmap = render(
        page,
        cache,
        interpreter,
        settings,
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (width, height) = (usize::from(pixmap.width()), usize::from(pixmap.height()));
    if width == 0
        || height == 0
        || width.max(height) as f32 > MAX_RENDER_SIDE
        || (width * height) as f32 > MAX_RENDER_PIXELS
    {
        return None;
    }
    Some(ColorImage::from_rgba_premultiplied(
        [width, height],
        pixmap.data_as_u8_slice(),
    ))
}

#[cfg(any(test, feature = "demo"))]
pub fn demo_pdf() -> Vec<u8> {
    let first = b"0.16 0.25 0.45 rg 20 270 160 12 re f\n0.72 0.75 0.80 rg 20 250 120 4 re f\n0.82 0.84 0.87 rg 20 235 156 3 re f\n0.82 0.84 0.87 rg 20 224 148 3 re f\n0.82 0.84 0.87 rg 20 213 155 3 re f\n0.82 0.84 0.87 rg 20 202 103 3 re f\n0.16 0.56 0.62 rg 20 105 74 68 re f\n0.88 0.25 0.28 rg 106 105 74 68 re f\n0.82 0.84 0.87 rg 20 86 160 3 re f\n0.82 0.84 0.87 rg 20 75 140 3 re f\n";
    let second = b"0.16 0.25 0.45 rg 20 270 160 12 re f\n0.82 0.84 0.87 rg 20 248 156 4 re f\n0.82 0.84 0.87 rg 20 235 144 3 re f\n0.25 0.35 0.8 rg 20 80 160 112 re f\n0.18 0.58 0.55 rg 34 94 25 48 re f\n0.88 0.48 0.24 rg 72 94 25 78 re f\n0.5 0.38 0.72 rg 110 94 25 60 re f\n0.19 0.62 0.48 rg 148 94 20 92 re f\n";
    let bodies = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 4 0 R >>".to_vec(),
        stream(first),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Contents 6 0 R >>".to_vec(),
        stream(second),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0usize];
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}

#[cfg(any(test, feature = "demo"))]
fn stream(contents: &[u8]) -> Vec<u8> {
    let mut bytes = format!("<< /Length {} >>\nstream\n", contents.len()).into_bytes();
    bytes.extend_from_slice(contents);
    bytes.extend_from_slice(b"endstream");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bounds_reject_invalid_geometry_and_cap_large_pages() {
        assert_eq!(render_scale(f32::NAN, 100.0, 1.0), None);
        assert_eq!(render_scale(100.0, f32::INFINITY, 1.0), None);
        assert_eq!(render_scale(100.0, 100.0, f32::NAN), None);
        let scale = render_scale(20_000.0, 20_000.0, 2.0).unwrap();
        assert!(scale * 20_000.0 <= MAX_RENDER_SIDE + 1.0);
        assert!(scale * scale * 20_000.0 * 20_000.0 <= MAX_RENDER_PIXELS + 1_000.0);
        assert_eq!(render_scale(100.0, 200.0, 1.0), Some(1.0));
    }

    #[test]
    fn synthetic_multipage_pdf_parses_and_renders_locally() {
        let pdf = Pdf::new(demo_pdf()).unwrap();
        assert_eq!(pdf.pages().len(), 2);
        let page = &pdf.pages()[0];
        let image = render_page(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            1.0,
        )
        .unwrap();
        assert_eq!(image.size, [200, 300]);
        assert!(
            image
                .pixels
                .iter()
                .any(|pixel| *pixel != egui::Color32::WHITE)
        );
    }

    #[test]
    fn invalid_pdf_is_rejected_without_exposing_its_contents() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), b"private fixture text, not a PDF").unwrap();
        assert!(matches!(
            load(path.path()),
            Err("This file is not a readable PDF.")
        ));
    }
}
