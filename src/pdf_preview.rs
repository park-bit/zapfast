//! State for the native PDF viewer. Rendering stays on its own worker.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use egui::{Key, Modifiers, TextureHandle};

use crate::model::ChatId;
use crate::pdf::{self, Event, Request, Worker};

const CACHED_PAGES: usize = 6;
const PAGE_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Failed(&'static str),
}

pub struct PreviewState {
    path: PathBuf,
    chat: ChatId,
    worker: Option<Worker>,
    started: bool,
    status: Status,
    pages: Vec<(f32, f32)>,
    page: usize,
    zoom: f32,
    jump: Option<usize>,
    requested: Option<(u64, usize, f32)>,
    rendered: VecDeque<(usize, f32, TextureHandle)>,
    page_errors: HashMap<usize, &'static str>,
    next_request: u64,
}

impl PreviewState {
    pub fn new(path: PathBuf, chat: ChatId) -> Self {
        Self {
            worker: None,
            started: false,
            path,
            chat,
            status: Status::Loading,
            pages: Vec::new(),
            page: 0,
            zoom: 1.0,
            jump: Some(0),
            requested: None,
            rendered: VecDeque::new(),
            page_errors: HashMap::new(),
            next_request: 0,
        }
    }

    pub fn start(&mut self, ctx: &egui::Context) {
        if self.started {
            return;
        }
        self.started = true;
        match Worker::spawn(&self.path, ctx.clone()) {
            Ok(worker) => self.worker = Some(worker),
            Err(_) => self.status = Status::Failed("The PDF viewer could not be started."),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        self.start(ctx);
        loop {
            let event = match self.worker.as_ref().map(Worker::next_event) {
                Some(Ok(event)) => event,
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.worker = None;
                    self.status = Status::Failed("The PDF preview stopped unexpectedly.");
                    break;
                }
            };
            match event {
                Event::Ready(pages) => {
                    self.pages = pages;
                    self.status = Status::Ready;
                }
                Event::Rendered {
                    request,
                    page,
                    image,
                } => {
                    let current = self
                        .requested
                        .is_some_and(|(id, requested, _)| id == request && requested == page);
                    if current {
                        let scale = self.requested.map_or(1.0, |(_, _, scale)| scale);
                        self.requested = None;
                        self.rendered.retain(|(cached, _, _)| *cached != page);
                        if self.rendered.len() == CACHED_PAGES {
                            self.rendered.pop_front();
                        }
                        self.rendered.push_back((
                            page,
                            scale,
                            ctx.load_texture(
                                format!("pdf-page-{request}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            ),
                        ));
                    }
                }
                Event::Failed { request, message } => {
                    if request.is_none() {
                        self.worker = None;
                        self.status = Status::Failed(message);
                        break;
                    }
                    if let Some((id, page, _)) = self.requested
                        && Some(id) == request
                    {
                        self.requested = None;
                        self.page_errors.insert(page, message);
                    }
                }
            }
        }
    }

    pub fn request_visible(
        &mut self,
        viewport: egui::Vec2,
        pixels_per_point: f32,
        visible: std::ops::Range<usize>,
    ) {
        if self.requested.is_some() {
            return;
        }
        let nearby = [visible.end, visible.start.saturating_sub(1)];
        let candidates: Vec<_> = visible
            .chain(nearby)
            .filter(|&page| page < self.pages.len())
            .take(CACHED_PAGES)
            .collect();
        let Some((page, scale)) = candidates.iter().find_map(|&page| {
            let (width, height) = self.pages[page];
            let scale = pdf::render_scale(
                width,
                height,
                fit_scale((width, height), viewport) * self.zoom * pixels_per_point,
            )?;
            (!self.page_errors.contains_key(&page)
                && !self.rendered.iter().any(|(cached, rendered_scale, _)| {
                    *cached == page && (scale - rendered_scale).abs() < scale * 0.05
                }))
            .then_some((page, scale))
        }) else {
            return;
        };
        // Keep visible pages ahead of old pages in the eviction order.
        self.rendered
            .make_contiguous()
            .sort_by_key(|(page, _, _)| candidates.contains(page));
        self.next_request = self.next_request.wrapping_add(1);
        let request = Request {
            id: self.next_request,
            page,
            scale,
        };
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.request(request))
        {
            self.requested = Some((self.next_request, page, scale));
        }
    }

    pub fn move_page(&mut self, step: i8) {
        let Some(next) = self.page.checked_add_signed(isize::from(step)) else {
            return;
        };
        if next < self.pages.len() {
            self.page = next;
            self.jump = Some(next);
        }
    }

    pub fn zoom_by(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom = (self.zoom * factor).clamp(0.25, 4.0);
            self.jump = Some(self.page);
            self.page_errors.clear();
        }
    }

    pub fn fit(&mut self) {
        self.zoom = 1.0;
        self.jump = Some(self.page);
        self.page_errors.clear();
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn chat(&self) -> &str {
        &self.chat
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn layout(&self, viewport: egui::Vec2) -> Vec<egui::Rect> {
        let mut top = 0.0;
        self.pages
            .iter()
            .map(|&(width, height)| {
                let size =
                    egui::vec2(width, height) * (fit_scale((width, height), viewport) * self.zoom);
                let rect = egui::Rect::from_min_size(egui::pos2(0.0, top), size);
                top += size.y + PAGE_GAP;
                rect
            })
            .collect()
    }

    pub fn take_jump(&mut self) -> Option<usize> {
        self.jump.take()
    }

    pub fn scrolled_to(&mut self, page: usize) -> bool {
        let changed = self.page != page;
        self.page = page;
        changed
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn page_error(&self, page: usize) -> Option<&'static str> {
        self.page_errors.get(&page).copied()
    }

    pub fn texture(&self, page: usize) -> Option<&TextureHandle> {
        self.rendered
            .iter()
            .find(|(cached, _, _)| *cached == page)
            .map(|(_, _, texture)| texture)
    }
}

fn fit_scale(page: (f32, f32), viewport: egui::Vec2) -> f32 {
    (viewport.x / page.0)
        .min(viewport.y / page.1)
        .clamp(0.0001, 2.0)
}

pub fn key_action(key: Key, modifiers: Modifiers) -> Option<crate::model::Action> {
    let command = modifiers.command || modifiers.ctrl;
    match (command, key) {
        (true, Key::Plus) | (true, Key::Equals) => Some(crate::model::Action::ZoomPdfBy(1.25)),
        (true, Key::Minus) => Some(crate::model::Action::ZoomPdfBy(0.8)),
        (false, Key::Plus) | (false, Key::Equals) if !modifiers.any() => {
            Some(crate::model::Action::ZoomPdfBy(1.25))
        }
        (false, Key::Minus) if !modifiers.any() => Some(crate::model::Action::ZoomPdfBy(0.8)),
        (false, Key::Num0) if !modifiers.any() => Some(crate::model::Action::FitPdf),
        (false, Key::ArrowLeft | Key::PageUp) if !modifiers.any() => {
            Some(crate::model::Action::PdfPage(-1))
        }
        (false, Key::ArrowRight | Key::PageDown) if !modifiers.any() => {
            Some(crate::model::Action::PdfPage(1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_and_zoom_stay_in_bounds() {
        let path = PathBuf::from("fixture.pdf");
        let mut state = PreviewState {
            path,
            chat: "chat".into(),
            worker: None,
            started: true,
            status: Status::Ready,
            pages: vec![(200.0, 300.0), (300.0, 200.0)],
            page: 0,
            zoom: 1.0,
            jump: None,
            requested: None,
            rendered: VecDeque::new(),
            page_errors: HashMap::new(),
            next_request: 0,
        };
        state.move_page(-1);
        assert_eq!(state.page(), 0);
        state.move_page(1);
        assert_eq!(state.page(), 1);
        assert_eq!(state.take_jump(), Some(1));
        state.move_page(1);
        assert_eq!(state.page(), 1);
        state.zoom_by(2.0);
        assert_eq!(state.zoom(), 2.0);
        state.zoom_by(f32::INFINITY);
        assert_eq!(state.zoom(), 2.0);
        state.fit();
        assert_eq!(state.zoom(), 1.0);
    }

    #[test]
    fn preview_keys_only_map_navigation_and_zoom() {
        assert_eq!(
            key_action(Key::ArrowRight, Modifiers::NONE),
            Some(crate::model::Action::PdfPage(1))
        );
        assert_eq!(
            key_action(Key::PageUp, Modifiers::NONE),
            Some(crate::model::Action::PdfPage(-1))
        );
        assert_eq!(
            key_action(Key::Num0, Modifiers::NONE),
            Some(crate::model::Action::FitPdf)
        );
        assert_eq!(key_action(Key::ArrowRight, Modifiers::SHIFT), None);
    }
}
