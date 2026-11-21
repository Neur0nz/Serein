//! Placeholder workspace while credentials or Gateway READY are pending.

use crate::{design, i18n, notifications, skeleton};
use egui::{Align, CornerRadius, Frame, Layout, Margin, Sense, Stroke, vec2};

/// Renders the startup shell and returns whether the owner chose to switch accounts.
pub fn show(
	ui: &mut egui::Ui,
	stage: &str,
	sidebar_width: u16,
	hide_title_bar: bool,
	traffic_light_inset: f32,
) -> bool {
	let mut switch_account = false;
	let p = design::palette(ui);
	let background = design::window_palette(ui);
	let rail_width = notifications::RAIL_WIDTH;
	let sidebar_width = f32::from(sidebar_width).clamp(190.0, 360.0);
	let status_text = format!("{stage}…");
	let switch_label = i18n::translate("main-restoring-screen-use-a-different-account");

	if !hide_title_bar {
		egui::Panel::top("startup-title-bar")
			.exact_size(design::TITLE_BAR_HEIGHT)
			.show_separator_line(false)
			.frame(Frame::new().fill(background.base).inner_margin(0))
			.show(ui, |ui| {
				design::window_drag(ui, ui.max_rect());
				ui.horizontal_centered(|ui| {
					ui.add_space(12.0 + traffic_light_inset);
					ui.label(
						design::semibold(ui, i18n::translate("main-restoring-screen-serein"), 13.0)
							.color(p.text_strong),
					);
					ui.add_space(10.0);
					connecting_pill(ui, &status_text);
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						design::window_controls(ui);
						ui.add_space(8.0);
						switch_account = switch_account_button(ui, &switch_label);
					});
				});
			});
	}

	egui::Panel::left("startup-navigation")
		.exact_size(rail_width + sidebar_width)
		.show_separator_line(true)
		.frame(Frame::new().fill(background.base).inner_margin(0))
		.show(ui, |ui| {
			egui::Panel::bottom("startup-account-footer")
				.show_separator_line(false)
				.frame(
					Frame::new()
						.fill(egui::Color32::TRANSPARENT)
						.inner_margin(Margin::same(8)),
				)
				.show(ui, |ui| {
					account_footer(ui);
				});

			egui::Panel::left("startup-rail")
				.exact_size(rail_width)
				.show_separator_line(false)
				.frame(
					Frame::new()
						.fill(egui::Color32::TRANSPARENT)
						.inner_margin(0),
				)
				.show(ui, |ui| {
					server_rail(ui);
				});

			let lists = ui.available_rect_before_wrap();
			ui.painter().rect_filled(
				lists,
				CornerRadius {
					nw: 12,
					ne: 0,
					sw: 0,
					se: 0,
				},
				background.sidebar,
			);
			Frame::new()
				.fill(egui::Color32::TRANSPARENT)
				.inner_margin(Margin::symmetric(8, 10))
				.show(ui, |ui| {
					channel_sidebar(ui);
				});
		});

	egui::CentralPanel::default()
		.frame(Frame::new().fill(background.canvas).inner_margin(0))
		.show(ui, |ui| {
			egui::Panel::top("startup-channel-header")
				.exact_size(48.0)
				.show_separator_line(true)
				.frame(
					Frame::new()
						.fill(background.canvas)
						.inner_margin(Margin::symmetric(16, 0)),
				)
				.show(ui, |ui| {
					ui.horizontal_centered(|ui| {
						let _ = skeleton::custom(
							ui,
							"Loading channel header",
							vec2(160.0, 14.0),
							|painter, rect, fill| {
								skeleton::bar(painter, fill, rect.min, rect.size());
							},
						);
						if hide_title_bar {
							ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
								switch_account = switch_account_button(ui, &switch_label);
								ui.add_space(10.0);
								connecting_pill(ui, &status_text);
							});
						}
					});
				});

			egui::Panel::bottom("startup-composer")
				.exact_size(60.0)
				.show_separator_line(false)
				.frame(
					Frame::new()
						.fill(background.canvas)
						.inner_margin(Margin::symmetric(16, 10)),
				)
				.show(ui, |ui| {
					let rect = ui.available_rect_before_wrap();
					ui.painter().rect_filled(rect, 10.0, p.raised);
					ui.painter().rect_stroke(
						rect,
						10.0,
						Stroke::new(1.0, p.border),
						egui::StrokeKind::Inside,
					);
					let width = (rect.width() - 28.0).clamp(0.0, 148.0);
					skeleton::bar(
						ui.painter(),
						skeleton::fill(ui),
						egui::pos2(rect.left() + 14.0, rect.center().y - 6.0),
						vec2(width, 12.0),
					);
				});

			Frame::new()
				.fill(egui::Color32::TRANSPARENT)
				.inner_margin(Margin::symmetric(16, 12))
				.show(ui, |ui| {
					timeline(ui);
				});
		});

	switch_account
}

fn connecting_pill(ui: &mut egui::Ui, text: &str) {
	let p = design::palette(ui);
	Frame::new()
		.fill(p.raised)
		.stroke(Stroke::new(1.0, p.border))
		.corner_radius(10.0)
		.inner_margin(Margin::symmetric(8, 2))
		.show(ui, |ui| {
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 6.0;
				let (dot, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
				ui.painter().circle_filled(dot.center(), 3.0, p.accent);
				ui.label(egui::RichText::new(text).size(11.5).color(p.muted));
			});
		});
}

fn switch_account_button(ui: &mut egui::Ui, label: &str) -> bool {
	let p = design::palette(ui);
	ui.add(egui::Button::new(egui::RichText::new(label).size(12.0).color(p.muted)).frame(false))
		.clicked()
}

fn account_footer(ui: &mut egui::Ui) {
	let p = design::palette(ui);
	Frame::new()
		.fill(p.raised)
		.stroke(Stroke::new(1.0, p.border))
		.corner_radius(10.0)
		.inner_margin(Margin::symmetric(10, 8))
		.show(ui, |ui| {
			ui.set_min_height(38.0);
			let _ = skeleton::rows(ui, "Loading account", skeleton::Row::PERSON, 1);
		});
}

fn server_rail(ui: &mut egui::Ui) {
	ui.add_space(8.0);
	ui.vertical_centered(|ui| {
		ui.spacing_mut().item_spacing.y = 8.0;
		let count = skeleton::visible_count(ui, 52.0, 8);
		for _ in 0..count {
			let _ = skeleton::custom(
				ui,
				"Loading servers",
				vec2(44.0, 44.0),
				|painter, rect, fill| {
					skeleton::block(painter, fill, rect, 14.0);
				},
			);
		}
	});
}

fn channel_sidebar(ui: &mut egui::Ui) {
	let _ = skeleton::custom(
		ui,
		"Loading server name",
		vec2(ui.available_width() * 0.6, 16.0),
		|painter, rect, fill| {
			skeleton::bar(painter, fill, rect.min, rect.size());
		},
	);
	ui.add_space(12.0);
	let row = skeleton::Row {
		height: 30.0,
		avatar: 0.0,
		square: false,
		lines: 1,
		inset: 6.0,
	};
	let count = skeleton::visible_count(ui, 34.0, 14);
	let _ = skeleton::rows_spaced(ui, "Loading channels", row, count, 4.0);
}

fn timeline(ui: &mut egui::Ui) {
	let row = skeleton::Row {
		height: 54.0,
		avatar: 36.0,
		square: false,
		lines: 2,
		inset: 4.0,
	};
	let count = skeleton::fitting_count(ui.available_height(), 64.0, 10.0).min(10);
	let _ = skeleton::rows_spaced(ui, "Loading messages", row, count, 10.0);
}
