//! State for the native PDF viewer. Rendering stays on its own worker.

use std::path::{Path, PathBuf};

use egui::{Key, Modifiers, TextureHandle};

use crate::model::ChatId;
use crate::pdf::{self, Event, Request, Worker};

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
    fit: bool,
    target_scale: Option<f32>,
    requested: Option<(u64, usize, f32)>,
    rendered: Option<(usize, f32, TextureHandle)>,
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
            fit: true,
            target_scale: None,
            requested: None,
            rendered: None,
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

    pub fn poll(&mut self, ctx: &egui::Context, viewport: egui::Vec2) {
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
                        self.rendered = Some((
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
                    let current = self
                        .requested
                        .is_some_and(|(id, page, _)| Some(id) == request && page == self.page);
                    if request.is_none() || current {
                        self.requested = None;
                        self.status = Status::Failed(message);
                    } else if self.requested.is_some_and(|(id, _, _)| Some(id) == request) {
                        self.requested = None;
                    }
                }
            }
        }

        if self.status == Status::Ready
            && let Some((width, height)) = self.pages.get(self.page).copied()
        {
            let fit = fit_scale((width, height), viewport);
            let scale = (fit * self.zoom * ctx.pixels_per_point()).max(0.01);
            self.target_scale = pdf::render_scale(width, height, scale);
            self.request_page_if_needed();
        }
    }

    fn request_page_if_needed(&mut self) {
        let Some(scale) = self.target_scale else {
            return;
        };
        if self.requested.is_some_and(|(_, page, requested_scale)| {
            page == self.page && (scale - requested_scale).abs() < 0.05
        }) || self
            .rendered
            .as_ref()
            .is_some_and(|(page, rendered_scale, _)| {
                *page == self.page && (scale - rendered_scale).abs() < 0.05
            })
        {
            return;
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = Request {
            id: self.next_request,
            page: self.page,
            scale,
        };
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.request(request))
        {
            self.requested = Some((self.next_request, self.page, scale));
        }
    }

    pub fn move_page(&mut self, step: i8) {
        let Some(next) = self.page.checked_add_signed(isize::from(step)) else {
            return;
        };
        if next < self.pages.len() {
            self.page = next;
        }
    }

    pub fn zoom_by(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom = (self.zoom * factor).clamp(0.25, 4.0);
            self.fit = false;
        }
    }

    pub fn fit(&mut self) {
        self.zoom = 1.0;
        self.fit = true;
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

    pub fn page_size(&self) -> Option<(f32, f32)> {
        self.pages.get(self.page).copied()
    }

    pub fn display_size(&self, viewport: egui::Vec2) -> Option<egui::Vec2> {
        let (width, height) = self.page_size()?;
        Some(egui::vec2(width, height) * (fit_scale((width, height), viewport) * self.zoom))
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn is_fit(&self) -> bool {
        self.fit
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn texture(&self) -> Option<&TextureHandle> {
        self.rendered
            .as_ref()
            .filter(|(page, _, _)| *page == self.page)
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
        let state = PreviewState {
            path,
            chat: "chat".into(),
            worker: None,
            started: true,
            status: Status::Ready,
            pages: vec![(200.0, 300.0), (300.0, 200.0)],
            page: 0,
            zoom: 1.0,
            fit: true,
            target_scale: None,
            requested: None,
            rendered: None,
            next_request: 0,
        };
        let mut state = state;
        state.move_page(-1);
        assert_eq!(state.page(), 0);
        state.move_page(1);
        assert_eq!(state.page(), 1);
        state.move_page(1);
        assert_eq!(state.page(), 1);
        state.zoom_by(2.0);
        assert_eq!(state.zoom(), 2.0);
        assert!(!state.is_fit());
        state.zoom_by(f32::INFINITY);
        assert_eq!(state.zoom(), 2.0);
        state.fit();
        assert_eq!(state.zoom(), 1.0);
        assert!(state.is_fit());
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
