//! Bundled Twemoji: an inline atlas and bounded scalable artwork decoded by the media worker.
use egui::{Context, Image, TextureHandle};
use image::ImageDecoder;
use std::sync::{Mutex, OnceLock, mpsc};

const ATLAS: &[u8] = include_bytes!("../../../assets/twemoji/atlas.png");
const INDEX: &str = include_str!("../../../assets/twemoji/index.tsv");
const VECTORS: &[u8] = include_bytes!("../../../assets/twemoji/vectors.bin");
const VECTOR_COUNT: usize = 4009;
const VECTOR_HEADER: usize = (VECTOR_COUNT + 1) * 4;

/// Parse only bounded local artwork keys. No URL or filesystem input is accepted.
pub fn bundled_svg(key: &str) -> Option<(&'static [u8], u32)> {
	if key.len() > 64 {
		return None;
	}
	let (cell, edge) = key.strip_prefix("emoji-unicode-")?.split_once('-')?;
	let cell = cell.parse::<usize>().ok()?;
	let edge = edge.parse::<u32>().ok()?;
	if cell >= VECTOR_COUNT || !matches!(edge, 64 | 128 | 256) {
		return None;
	}
	let offset = |index| {
		Some(u32::from_le_bytes(VECTORS.get(index * 4..index * 4 + 4)?.try_into().ok()?) as usize)
	};
	let start = offset(cell)?;
	let end = offset(cell + 1)?;
	if start >= end || end - start > 65536 {
		return None;
	}
	Some((
		VECTORS.get(VECTOR_HEADER + start..VECTOR_HEADER + end)?,
		edge,
	))
}

/// Call outside rendering. Individual trusted SVGs have a 64 KiB expansion/window limit.
pub fn decode_bundled_svg(source: &[u8]) -> Option<Vec<u8>> {
	use std::io::Read;
	if source.is_empty() || source.len() > 65536 {
		return None;
	}
	let decoder =
		ruzstd::decoding::StreamingDecoder::new_with_max_window_size(source, 65536).ok()?;
	let mut svg = Vec::new();
	decoder.take(65537).read_to_end(&mut svg).ok()?;
	(svg.len() <= 65536).then_some(svg)
}

static ENTRIES: OnceLock<Vec<(&'static str, usize)>> = OnceLock::new();

fn entries() -> &'static [(&'static str, usize)] {
	ENTRIES.get_or_init(|| {
		INDEX
			.lines()
			.map(|line| {
				let (text, index) = line.split_once('\t').expect("bundled emoji index");
				(text, index.parse().expect("bundled atlas cell"))
			})
			.collect()
	})
}

// The bundled RGBA8 PNG decodes directly into its final pixel allocation. Color32's
// safe byte view is converted from straight alpha before the image reaches egui.
fn decode_atlas() -> Result<egui::ColorImage, image::ImageError> {
	let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(ATLAS))?;
	assert_eq!(
		decoder.color_type(),
		image::ColorType::Rgba8,
		"bundled atlas format"
	);
	let (width, height) = decoder.dimensions();
	assert!(u64::from(width) * u64::from(height) * 4 <= 16 * 1024 * 1024);
	let mut image = egui::ColorImage::filled(
		[width as usize, height as usize],
		egui::Color32::TRANSPARENT,
	);
	decoder.read_image(image.as_raw_mut())?;
	for pixel in image.pixels_mut() {
		let [r, g, b, a] = pixel.to_array();
		*pixel = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
	}
	Ok(image)
}

/// The bundled artwork belongs to this graphics context, outside resettable account UI memory.
#[derive(Default)]
enum Atlas {
	#[default]
	Idle,
	Decoding(Mutex<mpsc::Receiver<egui::ColorImage>>),
	Ready(TextureHandle),
	Failed,
}

impl egui::Plugin for Atlas {
	fn debug_name(&self) -> &'static str {
		"Twemoji atlas"
	}
}

/// Let visible Unicode artwork start the one atlas decode for this desktop context.
pub fn load_on_demand(ctx: &Context) {
	ctx.add_plugin(Atlas::default());
}

/// Install synchronously for fixtures that need the artwork on their first pass.
pub fn install(ctx: &Context) -> Result<(), image::ImageError> {
	let atlas = ctx.plugin_or_default::<Atlas>();
	let mut atlas = atlas.lock();
	if !matches!(*atlas, Atlas::Ready(_)) {
		*atlas = Atlas::Ready(upload(ctx, decode_atlas()?));
	}
	entries();
	Ok(())
}

fn upload(ctx: &Context, image: egui::ColorImage) -> TextureHandle {
	ctx.load_texture("Twemoji 17.0.3", image, egui::TextureOptions::LINEAR)
}

impl Atlas {
	fn visible(&mut self, ctx: &Context) -> Option<egui::load::SizedTexture> {
		let image = match self {
			Self::Ready(texture) => return Some((&*texture).into()),
			Self::Failed => return None,
			Self::Idle => {
				let (send, receive) = mpsc::sync_channel(1);
				let wake = ctx.clone();
				let worker = std::thread::Builder::new()
					.name("serein-emoji-atlas".into())
					.spawn(move || {
						if let Ok(image) = decode_atlas() {
							let _ = send.send(image);
						}
						drop(send);
						wake.request_repaint();
					});
				*self = if worker.is_ok() {
					Self::Decoding(Mutex::new(receive))
				} else {
					Self::Failed
				};
				return None;
			}
			Self::Decoding(receive) => {
				match receive.get_mut().expect("atlas receiver").try_recv() {
					Ok(image) => image,
					Err(mpsc::TryRecvError::Empty) => return None,
					Err(mpsc::TryRecvError::Disconnected) => {
						*self = Self::Failed;
						return None;
					}
				}
			}
		};
		// Completion wakes the app, but only a visible consumer queues the GPU upload.
		let texture = upload(ctx, image);
		let sized = (&texture).into();
		*self = Self::Ready(texture);
		Some(sized)
	}
}

pub(crate) fn inline_size(ui: &egui::Ui) -> f32 {
	egui::TextStyle::Body.resolve(ui.style()).size * 1.6
}

/// A blank glyph with real advance width keeps inline objects intact across row breaks.
pub(crate) fn inline_format(ui: &egui::Ui, width: f32, height: f32) -> egui::TextFormat {
	let mut font_id = egui::FontId::monospace(height);
	let space = ui.fonts_mut(|fonts| fonts.glyph_width(&font_id, ' '));
	font_id.size *= width / space.max(f32::EPSILON);
	egui::TextFormat {
		font_id,
		color: egui::Color32::TRANSPARENT,
		line_height: Some(height),
		..Default::default()
	}
}

pub(crate) fn lookup(text: &str) -> Option<usize> {
	// Explicit text presentation must stay text. Do not partially match unknown sequences.
	if text.is_ascii() || text.contains('\u{fe0e}') || text.len() > 128 {
		return None;
	}
	let mut normalized = [0; 128];
	let key = if text.contains('\u{fe0f}') {
		let mut len = 0;
		for c in text.chars().filter(|c| *c != '\u{fe0f}') {
			len += c.encode_utf8(&mut normalized[len..]).len();
		}
		std::str::from_utf8(&normalized[..len]).expect("normalized UTF-8")
	} else {
		text
	};
	entries()
		.binary_search_by_key(&key, |(name, _)| *name)
		.ok()
		.map(|index| entries()[index].1)
}

/// Mark text-default symbols that Twemoji draws as emoji (✌, ❤) with U+FE0F, so font fallback
/// colours them in plain-text names as messages do. Emoji-default and explicit text forms stay.
pub(crate) fn presentation(text: &str) -> std::borrow::Cow<'_, str> {
	use unicode_segmentation::UnicodeSegmentation as _;
	let bare = |grapheme: &str| {
		grapheme.chars().nth(1).is_none()
			&& !egui::epaint::text::has_emoji_presentation(grapheme)
			&& lookup(grapheme).is_some()
	};
	if text.is_ascii() || !text.graphemes(true).any(bare) {
		return std::borrow::Cow::Borrowed(text);
	}
	let mut marked = String::with_capacity(text.len() + 3);
	for grapheme in text.graphemes(true) {
		marked.push_str(grapheme);
		if bare(grapheme) {
			marked.push('\u{fe0f}');
		}
	}
	std::borrow::Cow::Owned(marked)
}

pub(crate) fn image(
	ui: &egui::Ui,
	text: &str,
	cell: usize,
	rect: egui::Rect,
) -> Option<Image<'static>> {
	if !artwork_is_visible(ui, rect) {
		return None;
	}
	Some(image_cell(
		visible_atlas(ui.ctx())?,
		text,
		cell,
		rect.width(),
	))
}

/// Request the atlas only for a recognized Unicode cell about to be drawn.
pub(crate) fn visible_atlas(ctx: &Context) -> Option<egui::load::SizedTexture> {
	ctx.with_plugin(|atlas: &mut Atlas| atlas.visible(ctx))?
}

pub(crate) fn artwork_is_visible(ui: &egui::Ui, rect: egui::Rect) -> bool {
	!ui.is_sizing_pass() && ui.is_rect_visible(rect) && rect.intersect(ui.clip_rect()).is_positive()
}

/// Keep the artwork slot stable while the atlas is unavailable without consulting system emoji fonts.
pub(crate) fn paint_placeholder(ui: &egui::Ui, rect: egui::Rect) {
	if artwork_is_visible(ui, rect) {
		let size = rect.width().min(rect.height()) * 0.55;
		ui.painter().rect_filled(
			egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(size)),
			size * 0.25,
			ui.visuals().weak_text_color().gamma_multiply(0.25),
		);
	}
}

pub(crate) fn paint_unicode(ui: &egui::Ui, text: &str, cell: usize, rect: egui::Rect) {
	if let Some(image) = image(ui, text, cell, rect) {
		image.paint_at(ui, rect);
	} else {
		paint_placeholder(ui, rect);
	}
}

pub(crate) fn atom(ui: &egui::Ui, text: String, cell: usize, size: f32) -> egui::Atom<'static> {
	// Match the font-sized artwork used by egui's image buttons.
	let font = egui::FontSelection::default().resolve(ui.style());
	let size = size.min(ui.fonts_mut(|fonts| fonts.row_height(&font)));
	egui::Atom::paint(egui::Vec2::splat(size), move |ui, args| {
		paint_unicode(ui, &text, cell, args.rect);
	})
}

pub(crate) fn image_cell(
	atlas: egui::load::SizedTexture,
	text: &str,
	cell: usize,
	size: f32,
) -> Image<'static> {
	let x = (cell % 64) as f32 * 32.0;
	let y = (cell / 64) as f32 * 32.0;
	let uv = egui::Rect::from_min_max(
		egui::pos2(x / atlas.size.x, y / atlas.size.y),
		egui::pos2((x + 32.0) / atlas.size.x, (y + 32.0) / atlas.size.y),
	);
	Image::new((atlas.id, egui::Vec2::splat(size)))
		.uv(uv)
		.alt_text(text)
}

pub(crate) fn blank(ctx: &Context, size: f32) -> Image<'static> {
	let id = egui::Id::unique("emoji-blank");
	let texture = ctx.data(|data| data.get_temp::<TextureHandle>(id));
	let texture = texture.unwrap_or_else(|| {
		let texture = ctx.load_texture(
			"emoji-blank",
			egui::ColorImage::filled([1, 1], egui::Color32::TRANSPARENT),
			egui::TextureOptions::LINEAR,
		);
		ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
		texture
	});
	Image::new(&texture).fit_to_exact_size(egui::Vec2::splat(size))
}

/// Whether custom emoji markup is the animated `<a:name:id>` form.
pub(crate) fn custom_animated(text: &str) -> bool {
	text.starts_with("<a:") && custom_prefix(text).is_some()
}

pub(crate) fn custom_prefix(text: &str) -> Option<(model::Id, usize)> {
	let body = text
		.strip_prefix("<:")
		.or_else(|| text.strip_prefix("<a:"))?;
	let end = body.as_bytes().iter().take(54).position(|b| *b == b'>')?;
	let (name, id) = body[..end].split_once(':')?;
	if !(2..=32).contains(&name.len())
		|| !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
	{
		return None;
	}
	Some((id.parse().ok()?, text.len() - body.len() + end + 1))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_ready_atlas_waits_for_visible_artwork_and_survives_account_ui_reset() {
		let ctx = Context::default();
		let (send, receive) = mpsc::sync_channel(1);
		ctx.add_plugin(Atlas::Decoding(Mutex::new(receive)));
		send.send(egui::ColorImage::filled([1, 1], egui::Color32::WHITE))
			.unwrap();
		let draw_plain_preview = |ui: &mut egui::Ui| {
			let galley = ui.painter().layout_no_wrap(
				"No emoji".into(),
				egui::FontId::default(),
				egui::Color32::WHITE,
			);
			crate::markdown::Formatted::paint_inline_preview_emojis(
				ui,
				ui.min_rect().min,
				&galley,
				&[],
			);
			let clipped = ui
				.clip_rect()
				.translate(egui::vec2(ui.clip_rect().width() + 1.0, 0.0));
			assert!(image(ui, "🙂", lookup("🙂").unwrap(), clipped).is_none());
			let sizing = ui.new_child(egui::UiBuilder::new().sizing_pass());
			let inside =
				egui::Rect::from_center_size(sizing.clip_rect().center(), egui::vec2(18.0, 18.0));
			assert!(image(&sizing, "🙂", lookup("🙂").unwrap(), inside).is_none());
		};
		ctx.run_ui(egui::RawInput::default(), draw_plain_preview)
			.drop_without_applying_deltas();
		assert_eq!(
			ctx.with_plugin(|atlas: &mut Atlas| matches!(atlas, Atlas::Decoding(_))),
			Some(true),
		);
		let draw_forum_tag = |ui: &mut egui::Ui| {
			crate::forum::tag_pill(
				ui,
				(&mut crate::avatars::Avatars::default(), true),
				&model::forum::Tag {
					id: model::Id(1),
					name: "Forum tag".into(),
					moderated: false,
					emoji_id: None,
					emoji_name: Some("🙂".into()),
				},
				false,
				24.0,
				egui::Sense::hover(),
			);
		};
		let ready = ctx.run_ui(egui::RawInput::default(), draw_forum_tag);
		let atlas_id = ctx
			.with_plugin(|atlas: &mut Atlas| match atlas {
				Atlas::Ready(texture) => texture.id(),
				_ => panic!("visible forum emoji did not publish the atlas"),
			})
			.unwrap();
		assert!(
			ready
				.textures_delta
				.set
				.iter()
				.any(|(id, _)| *id == atlas_id)
		);
		ready.drop_without_applying_deltas();
		ctx.memory_mut(|memory| *memory = egui::Memory::default());
		let reopened = ctx.run_ui(egui::RawInput::default(), draw_forum_tag);
		assert!(
			!reopened
				.textures_delta
				.set
				.iter()
				.any(|(id, _)| *id == atlas_id)
		);
		reopened.drop_without_applying_deltas();
		assert_eq!(
			ctx.with_plugin(
				|atlas: &mut Atlas| matches!(atlas, Atlas::Ready(texture) if texture.id() == atlas_id)
			),
			Some(true),
		);
	}

	/// Plain-text names colour ✌ like Discord does, without touching emoji or text forms.
	#[test]
	fn names_mark_text_default_twemoji_for_colour() {
		assert_eq!(presentation("✌ oznámení"), "✌\u{fe0f} oznámení");
		for unchanged in ["🛒-bazar", "✌\u{fe0f}", "✌\u{fe0e}", "✌🏻", "plain", "→ x"] {
			assert_eq!(presentation(unchanged), unchanged);
		}
	}
	#[test]
	fn animated_custom_markup_is_recognized() {
		assert!(custom_animated("<a:wave_animated:1169784511555452928>"));
		assert!(!custom_animated("<:wave:1169784511555452928>"));
		assert!(!custom_animated("<a:x:1>"), "names need two characters");
		assert!(!custom_animated("<a:wave:not-an-id>"));
		assert!(!custom_animated("\u{1f44b}"));
	}
	#[test]
	fn vector_artwork_keys_and_decompression_are_bounded() {
		for edge in [64, 128, 256] {
			let (source, parsed) = bundled_svg(&format!("emoji-unicode-0-{edge}")).unwrap();
			assert_eq!(parsed, edge);
			let svg = decode_bundled_svg(source).unwrap();
			assert!(svg.starts_with(b"<svg"));
			assert!(svg.len() <= 65536);
		}
		for key in [
			"emoji-unicode-4009-64",
			"emoji-unicode-0-4096",
			"emoji-unicode-../0-64",
			"emoji-unicode-0-64?url=x",
			"emoji-9001",
		] {
			assert!(bundled_svg(key).is_none(), "{key}");
		}
		assert!(decode_bundled_svg(b"not compressed svg").is_none());
		for cell in 0..VECTOR_COUNT {
			let (source, _) = bundled_svg(&format!("emoji-unicode-{cell}-64")).unwrap();
			assert!(decode_bundled_svg(source).is_some(), "cell {cell}");
		}
	}
	#[test]
	fn custom_markup_is_bounded_and_never_an_arbitrary_url() {
		for token in ["<:serein_wave:9001>", "<a:serein_party:9001>"] {
			assert_eq!(
				custom_prefix(&format!("{token} trailing")),
				Some((model::Id(9001), token.len()))
			);
		}
		for token in [
			"<:x:1>",
			"<:hello:0>",
			"<:hello:-1>",
			"<:hello:18446744073709551616>",
			"<:../x:1>",
			"<:hello:1/2>",
			"<a:hello:1",
			"<:hello:https://example.com>",
		] {
			assert!(custom_prefix(token).is_none(), "{token}");
		}
	}
	#[test]
	fn atlas_is_bounded_and_matches_complete_sequences() {
		let image = image::load_from_memory(ATLAS).unwrap();
		assert!(image.width() as usize * image.height() as usize * 4 <= 16 * 1024 * 1024);
		assert!(ATLAS.len() <= 8 * 1024 * 1024);
		assert!(entries().len() <= 4096);
		assert!(entries().windows(2).all(|pair| pair[0].0 < pair[1].0));
		assert!(
			entries()
				.iter()
				.all(|(_, cell)| *cell < (image.width() / 32 * (image.height() / 32)) as usize)
		);
		for text in ["👍", "👍🏽", "❤️", "👩🏽‍💻", "🇨🇿", "1️⃣", "🏳️‍🌈", "🏳‍🌈", "👩‍⚕", "👨‍👩‍👧‍👦"]
		{
			assert!(lookup(text).is_some(), "missing {text}");
		}
		for text in ["hello", "1", "©\u{fe0e}", "👩\u{200d}🦀", "<:custom:123>"] {
			assert!(lookup(text).is_none(), "unexpected {text}");
		}
	}
}
