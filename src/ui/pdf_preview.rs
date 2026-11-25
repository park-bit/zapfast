//! Full-window native PDF reader for downloaded chat documents.

use egui::{Align, Frame, Layout, Margin, Rect, Sense, UiBuilder, Vec2, vec2};

use crate::app::App;
use crate::model::Action;
use crate::pdf_preview::{PreviewState, Status};
use crate::theme::{self, Icon};

const HEADER: f32 = 56.0;

pub(crate) fn modal_id() -> egui::Id {
    egui::Id::new("pdf-preview")
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(preview) = app.pdf_preview.as_mut() else {
        return;
    };
    let palette = app.palette;
    let locale = app.locale;
    let screen = ctx.content_rect();
    let header_rect = Rect::from_min_size(screen.min, vec2(screen.width(), HEADER));
    let stage = Rect::from_min_max(egui::pos2(screen.left(), header_rect.bottom()), screen.max);
    preview.poll(ctx, (stage.size() - vec2(32.0, 32.0)).max(Vec2::splat(1.0)));

    let mut actions = Vec::new();
    let mut backdrop_clicked = false;
    let id = modal_id();
    let area = egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min);
    let response = egui::Modal::new(id)
        .area(area)
        .frame(Frame::new().fill(palette.overlay))
        .backdrop_color(palette.shadow)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.painter().rect_filled(screen, 0.0, palette.overlay);
            let (_, backdrop) = ui.allocate_exact_size(screen.size(), Sense::CLICK);

            let mut head = ui.new_child(UiBuilder::new().max_rect(header_rect));
            head.set_clip_rect(header_rect);
            header(&mut head, preview, &palette, locale, &mut actions);

            let mut body = ui.new_child(UiBuilder::new().max_rect(stage));
            body.set_clip_rect(stage);
            page(&mut body, preview, stage, &palette, locale, &mut actions);
            backdrop_clicked = backdrop.clicked();
        });
    if response.should_close() || backdrop_clicked {
        actions.push(Action::ClosePdfPreview);
    }
    app.actions.extend(actions);
}

fn header(
    ui: &mut egui::Ui,
    preview: &PreviewState,
    palette: &theme::Palette,
    locale: crate::i18n::Locale,
    actions: &mut Vec<Action>,
) {
    let rect = ui.max_rect();
    super::caption_drag(ui, rect);
    let room = super::caption_room(ui.ctx(), rect, 0.0);
    let inset = Rect::from_min_max(
        rect.min + vec2(room.left, 0.0),
        rect.max - vec2(room.right, 0.0),
    );
    let mut header = ui.new_child(UiBuilder::new().max_rect(inset));
    header.set_clip_rect(rect);
    Frame::new()
        .fill(palette.panel)
        .inner_margin(Margin::symmetric(12, 6))
        .show(&mut header, |ui| {
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), HEADER - 12.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    if theme::icon_button(
                        ui,
                        Icon::X,
                        18.0,
                        palette.secondary,
                        palette.text,
                        crate::i18n::gettext(locale, "Close PDF preview (Esc)").as_ref(),
                    )
                    .clicked()
                    {
                        actions.push(Action::ClosePdfPreview);
                    }
                    ui.add_space(8.0);
                    let name = preview
                        .path()
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| {
                            crate::i18n::gettext(locale, "PDF document").into_owned()
                        });
                    ui.allocate_ui_with_layout(
                        vec2((ui.available_width() - 330.0).max(0.0), 36.0),
                        Layout::left_to_right(Align::Center),
                        |ui| theme::text(ui, name, theme::medium(14.0), palette.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(
                            ui,
                            Icon::ExternalLink,
                            18.0,
                            palette.secondary,
                            palette.text,
                            crate::i18n::gettext(locale, "Open in another app").as_ref(),
                        )
                        .clicked()
                        {
                            actions.push(Action::ClosePdfPreview);
                            actions.push(Action::OpenFile(preview.path().to_owned()));
                        }
                        if theme::icon_button(
                            ui,
                            Icon::Plus,
                            18.0,
                            palette.secondary,
                            palette.text,
                            crate::i18n::gettext(locale, "Zoom in").as_ref(),
                        )
                        .clicked()
                        {
                            actions.push(Action::ZoomPdfBy(1.25));
                        }
                        if theme::soft_button(
                            ui,
                            palette,
                            None,
                            &format!("{:.0}%", preview.zoom() * 100.0),
                            false,
                        )
                        .on_hover_text(crate::i18n::gettext(locale, "Fit page to window (0)"))
                        .clicked()
                        {
                            actions.push(Action::FitPdf);
                        }
                        if theme::icon_button(
                            ui,
                            Icon::Minus,
                            18.0,
                            palette.secondary,
                            palette.text,
                            crate::i18n::gettext(locale, "Zoom out").as_ref(),
                        )
                        .clicked()
                        {
                            actions.push(Action::ZoomPdfBy(0.8));
                        }
                        ui.add_space(8.0);
                        if theme::icon_button(
                            ui,
                            Icon::ChevronRight,
                            18.0,
                            palette.secondary,
                            palette.text,
                            crate::i18n::gettext(locale, "Next page").as_ref(),
                        )
                        .clicked()
                        {
                            actions.push(Action::PdfPage(1));
                        }
                        theme::text(
                            ui,
                            format!("{} / {}", preview.page() + 1, preview.page_count()),
                            theme::regular(13.0),
                            palette.secondary,
                        );
                        if theme::icon_button(
                            ui,
                            Icon::ChevronLeft,
                            18.0,
                            palette.secondary,
                            palette.text,
                            crate::i18n::gettext(locale, "Previous page").as_ref(),
                        )
                        .clicked()
                        {
                            actions.push(Action::PdfPage(-1));
                        }
                    });
                },
            );
        });
}

fn page(
    ui: &mut egui::Ui,
    preview: &PreviewState,
    stage: Rect,
    palette: &theme::Palette,
    locale: crate::i18n::Locale,
    actions: &mut Vec<Action>,
) {
    let view = stage.shrink(16.0);
    let message = match preview.status() {
        Status::Loading => Some(crate::i18n::gettext(locale, "Opening PDF…").into_owned()),
        Status::Failed(message) => Some(crate::i18n::gettext(locale, message).into_owned()),
        Status::Ready if preview.texture().is_none() => {
            Some(crate::i18n::gettext(locale, "Rendering page…").into_owned())
        }
        Status::Ready => None,
    };
    if let Some(message) = message {
        ui.scope_builder(UiBuilder::new().max_rect(view), |ui| {
            ui.vertical_centered_justified(|ui| {
                ui.add_space((view.height() * 0.35).max(16.0));
                if preview.status() == Status::Loading || preview.status() == Status::Ready {
                    ui.spinner();
                }
                theme::text(ui, message, theme::regular(14.0), palette.secondary);
                if matches!(preview.status(), Status::Failed(_))
                    && theme::soft_button(
                        ui,
                        palette,
                        Some(Icon::ExternalLink),
                        crate::i18n::gettext(locale, "Open in another app").as_ref(),
                        false,
                    )
                    .clicked()
                {
                    actions.push(Action::ClosePdfPreview);
                    actions.push(Action::OpenFile(preview.path().to_owned()));
                }
            });
        });
        return;
    }

    let Some(texture) = preview.texture() else {
        return;
    };
    let Some(size) = preview.display_size(view.size()) else {
        return;
    };
    let content_size = Vec2::new(view.width().max(size.x), view.height().max(size.y));
    ui.scope_builder(UiBuilder::new().max_rect(view), |ui| {
        egui::ScrollArea::both()
            .id_salt("pdf-preview-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                let (content, _) = ui.allocate_exact_size(content_size, Sense::hover());
                let image = Rect::from_center_size(content.center(), size);
                ui.painter().rect_filled(image, 0.0, egui::Color32::WHITE);
                ui.put(
                    image,
                    egui::Image::from_texture(texture).fit_to_exact_size(size),
                );
            });
    });
}
