use crate::{
    Bounds, DevicePixels, Font, FontFeatures, FontId, FontMetrics, FontRun, FontStyle, FontWeight,
    GlyphId, LineLayout, Pixels, PlatformTextSystem, Point, RenderGlyphParams, SUBPIXEL_VARIANTS_X,
    SUBPIXEL_VARIANTS_Y, ShapedGlyph, ShapedRun, SharedString, Size, point, size,
};
use anyhow::{Context as _, Result};
use collections::HashMap;
use cosmic_text::{
    Attrs, AttrsList, CacheKey, Family, Font as CosmicTextFont, FontFeatures as CosmicFontFeatures,
    FontSystem, ShapeBuffer, ShapeLine, SwashCache,
};
use itertools::Itertools;
use parking_lot::RwLock;
use pathfinder_geometry::{
    rect::{RectF, RectI},
    vector::{Vector2F, Vector2I},
};
use smallvec::SmallVec;
use std::{borrow::Cow, path::Path, sync::Arc};

pub(crate) struct OhosTextSystem(RwLock<OhosTextSystemState>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: SharedString,
    features: FontFeatures,
    weight: FontWeight,
    style: FontStyle,
}

impl FontKey {
    fn new(
        family: SharedString,
        features: FontFeatures,
        weight: FontWeight,
        style: FontStyle,
    ) -> Self {
        Self {
            family,
            features,
            weight,
            style,
        }
    }
}

struct OhosTextSystemState {
    swash_cache: SwashCache,
    font_system: FontSystem,
    scratch: ShapeBuffer,
    /// Contains all already loaded fonts, including all faces. Indexed by `FontId`.
    loaded_fonts: Vec<LoadedFont>,
    /// Caches the `FontId`s associated with a specific family to avoid iterating the font database
    /// for every font face in a family.
    font_ids_by_family_cache: HashMap<FontKey, CachedFamilyFonts>,
    /// Monotonic version for font database mutations used to lazily invalidate cached family lookups.
    font_db_generation: u64,
    system_fonts_loaded: bool,
}

#[derive(Clone)]
struct CachedFamilyFonts {
    generation: u64,
    font_ids: SmallVec<[FontId; 4]>,
}

struct LoadedFont {
    font: Arc<CosmicTextFont>,
    features: CosmicFontFeatures,
    is_known_emoji_font: bool,
    requested_weight: cosmic_text::Weight,
    requested_style: cosmic_text::Style,
}

impl OhosTextSystem {
    pub(crate) fn new() -> Self {
        let mut font_system = FontSystem::new();

        Self(RwLock::new(OhosTextSystemState {
            font_system,
            swash_cache: SwashCache::new(),
            scratch: ShapeBuffer::default(),
            loaded_fonts: Vec::new(),
            font_ids_by_family_cache: HashMap::default(),
            font_db_generation: 0,
            system_fonts_loaded: false,
        }))
    }
}

impl Default for OhosTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformTextSystem for OhosTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.write().add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        let mut result = self
            .0
            .read()
            .font_system
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|family| family.0.clone()))
            .collect_vec();
        result.sort();
        result.dedup();
        result
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        let mut state = self.0.write();
        let key = FontKey::new(
            font.family.clone(),
            font.features.clone(),
            font.weight,
            font.style,
        );
        let generation = state.font_db_generation;
        let candidates: SmallVec<[FontId; 4]> = if let Some(cached) =
            state.font_ids_by_family_cache.get(&key)
            && cached.generation == generation
        {
            cached.font_ids.clone()
        } else {
            let font_ids =
                state.load_family(&font.family, &font.features, font.weight, font.style)?;
            state.font_ids_by_family_cache.insert(
                key.clone(),
                CachedFamilyFonts {
                    generation,
                    font_ids: font_ids.clone(),
                },
            );
            font_ids
        };

        let candidate_properties = candidates
            .iter()
            .map(|font_id| {
                let database_id = state.loaded_font(*font_id).font.id();
                let face_info = state.font_system.db().face(database_id).expect("");
                face_info_into_properties(face_info)
            })
            .collect::<SmallVec<[_; 4]>>();

        let ix = match font_kit::matching::find_best_match(
            &candidate_properties,
            &font_into_properties(font),
        ) {
            Ok(ix) => ix,
            // If style/weight matching fails, still keep the requested family by
            // falling back to its first loaded face instead of escalating to the
            // global system fallback stack.
            Err(_) => 0,
        };
        Ok(candidates[ix])
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        let metrics = self
            .0
            .read()
            .loaded_font(font_id)
            .font
            .as_swash()
            .metrics(&[]);

        FontMetrics {
            units_per_em: metrics.units_per_em as u32,
            ascent: metrics.ascent,
            descent: -metrics.descent,
            line_gap: metrics.leading,
            underline_position: metrics.underline_offset,
            underline_thickness: metrics.stroke_size,
            cap_height: metrics.cap_height,
            x_height: metrics.x_height,
            bounding_box: Bounds {
                origin: point(0.0, 0.0),
                size: size(metrics.max_width, metrics.ascent + metrics.descent),
            },
        }
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        let lock = self.0.read();
        let glyph_metrics = lock.loaded_font(font_id).font.as_swash().glyph_metrics(&[]);
        let glyph_id = glyph_id.0 as u16;
        Ok(Bounds {
            origin: point(0.0, 0.0),
            size: size(
                glyph_metrics.advance_width(glyph_id),
                glyph_metrics.advance_height(glyph_id),
            ),
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.read().advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.read().glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.write().raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.write().rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.write().layout_line(text, font_size, runs)
    }
}

impl OhosTextSystemState {
    fn normalize_family_name(name: &str) -> String {
        name.trim()
            .to_lowercase()
            .replace('_', " ")
            .replace('-', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn ensure_system_fonts_loaded(&mut self) {
        if self.system_fonts_loaded {
            return;
        }

        let db = self.font_system.db_mut();
        let initial_count = db.faces().count();

        // Common OHOS / Android font locations
        const FONT_DIRS: [&str; 5] = [
            "/system/fonts",
            "/system/font",
            "/vendor/fonts",
            "/system_ext/fonts",
            "/data/fonts",
        ];
        for dir in FONT_DIRS {
            let path = Path::new(dir);
            if path.is_dir() {
                db.load_fonts_dir(path);
            }
        }

        // Fallback to built-in fontdb search (may be empty on OHOS)
        let final_count = db.faces().count();
        if final_count == initial_count {
            db.load_system_fonts();
        }

        self.system_fonts_loaded = true;
    }

    fn loaded_font(&self, font_id: FontId) -> &LoadedFont {
        &self.loaded_fonts[font_id.0]
    }

    fn add_fonts(&mut self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let db = self.font_system.db_mut();
        for bytes in fonts {
            match bytes {
                Cow::Borrowed(embedded_font) => {
                    db.load_font_data(embedded_font.to_vec());
                }
                Cow::Owned(bytes) => {
                    db.load_font_data(bytes);
                }
            }
        }
        // Mark cached family lookups stale. We keep cache entries and lazily refresh on demand.
        self.font_db_generation = self.font_db_generation.wrapping_add(1);
        Ok(())
    }

    fn load_family(
        &mut self,
        name: &str,
        features: &FontFeatures,
        requested_weight: FontWeight,
        requested_style: FontStyle,
    ) -> Result<SmallVec<[FontId; 4]>> {
        self.ensure_system_fonts_loaded();

        let mut families = SmallVec::<[cosmic_text::fontdb::ID; 4]>::new();
        let mut seen_ids = HashMap::<cosmic_text::fontdb::ID, ()>::default();
        let system_name = "HarmonyOS Sans";
        let primary_name = crate::text_system::font_name_with_fallbacks(name, system_name);
        let mut candidates = SmallVec::<[&str; 3]>::new();
        candidates.push(primary_name);
        if name == ".SystemUIFont" {
            candidates.push("HarmonyOS_Sans");
            candidates.push("sans-serif");
        }

        for candidate in candidates {
            let normalized_candidate = Self::normalize_family_name(candidate);
            for face in self.font_system.db().faces().filter(|face| {
                face.families.iter().any(|family| {
                    candidate == family.0
                        || normalized_candidate == Self::normalize_family_name(&family.0)
                })
            }) {
                if seen_ids.insert(face.id, ()).is_none() {
                    families.push(face.id);
                }
            }
            if !families.is_empty() {
                break;
            }
        }

        if families.is_empty() {
            log::warn!(
                "OHOS text system: font family '{}' not found, falling back to '{}'",
                name,
                system_name
            );

            let normalized_system_name = Self::normalize_family_name(system_name);
            for face in self.font_system.db().faces().filter(|face| {
                face.families.iter().any(|family| {
                    system_name == family.0
                        || normalized_system_name == Self::normalize_family_name(&family.0)
                })
            }) {
                if seen_ids.insert(face.id, ()).is_none() {
                    families.push(face.id);
                }
            }
        }

        if families.is_empty() {
            anyhow::bail!(
                "OHOS text system: fallback font family '{}' is also unavailable",
                system_name
            );
        }

        let mut loaded_font_ids = SmallVec::new();
        for font_id in families {
            let postscript_name = self
                .font_system
                .db()
                .face(font_id)
                .map(|face| face.post_script_name.clone())
                .unwrap_or_default();
            let font = self
                .font_system
                .get_font(font_id)
                .context("Could not load font")?;

            let font_id = FontId(self.loaded_fonts.len());
            loaded_font_ids.push(font_id);
            self.loaded_fonts.push(LoadedFont {
                font,
                features: features.try_into()?,
                is_known_emoji_font: check_is_known_emoji_font(&postscript_name),
                requested_weight: requested_weight.into(),
                requested_style: requested_style.into(),
            });
        }

        Ok(loaded_font_ids)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        let glyph_metrics = self.loaded_font(font_id).font.as_swash().glyph_metrics(&[]);
        Ok(Size {
            width: glyph_metrics.advance_width(glyph_id.0 as u16),
            height: glyph_metrics.advance_height(glyph_id.0 as u16),
        })
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let glyph_id = self.loaded_font(font_id).font.as_swash().charmap().map(ch);
        if glyph_id == 0 {
            None
        } else {
            Some(GlyphId(glyph_id.into()))
        }
    }

    fn raster_bounds(&mut self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let font = &self.loaded_fonts[params.font_id.0].font;
        let subpixel_shift = point(
            params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
            params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
        );
        let image = self
            .swash_cache
            .get_image(
                &mut self.font_system,
                CacheKey::new(
                    font.id(),
                    params.glyph_id.0 as u16,
                    (params.font_size * params.scale_factor).into(),
                    (subpixel_shift.x, subpixel_shift.y.trunc()),
                    cosmic_text::CacheKeyFlags::empty(),
                )
                .0,
            )
            .clone()
            .with_context(|| format!("no image for {params:?} in font {font:?}"))?;
        Ok(Bounds {
            origin: point(image.placement.left.into(), (-image.placement.top).into()),
            size: size(image.placement.width.into(), image.placement.height.into()),
        })
    }

    fn rasterize_glyph(
        &mut self,
        params: &RenderGlyphParams,
        glyph_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        if glyph_bounds.size.width.0 == 0 || glyph_bounds.size.height.0 == 0 {
            anyhow::bail!("glyph bounds are empty");
        } else {
            let bitmap_size = glyph_bounds.size;
            let font = &self.loaded_fonts[params.font_id.0].font;
            let subpixel_shift = point(
                params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
                params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
            );
            let mut image = self
                .swash_cache
                .get_image(
                    &mut self.font_system,
                    CacheKey::new(
                        font.id(),
                        params.glyph_id.0 as u16,
                        (params.font_size * params.scale_factor).into(),
                        (subpixel_shift.x, subpixel_shift.y.trunc()),
                        cosmic_text::CacheKeyFlags::empty(),
                    )
                    .0,
                )
                .clone()
                .with_context(|| format!("no image for {params:?} in font {font:?}"))?;

            let synthetic_bold = self.should_apply_synthetic_bold(params.font_id);
            if synthetic_bold && !params.is_emoji {
                embolden_bitmap(
                    &mut image.data,
                    image.placement.width as usize,
                    image.placement.height as usize,
                );
            }

            if params.is_emoji {
                // Convert from RGBA to BGRA.
                for pixel in image.data.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }

            Ok((bitmap_size, image.data))
        }
    }

    fn font_id_for_cosmic_id_with_request(
        &mut self,
        id: cosmic_text::fontdb::ID,
        requested_weight: cosmic_text::Weight,
        requested_style: cosmic_text::Style,
    ) -> FontId {
        if let Some(ix) = self.loaded_fonts.iter().position(|loaded_font| {
            loaded_font.font.id() == id
                && loaded_font.requested_weight == requested_weight
                && loaded_font.requested_style == requested_style
        }) {
            FontId(ix)
        } else {
            let font = self.font_system.get_font(id).unwrap();
            let face = self.font_system.db().face(id).unwrap();

            let font_id = FontId(self.loaded_fonts.len());
            self.loaded_fonts.push(LoadedFont {
                font,
                features: CosmicFontFeatures::new(),
                is_known_emoji_font: check_is_known_emoji_font(&face.post_script_name),
                requested_weight,
                requested_style,
            });

            font_id
        }
    }

    fn should_apply_synthetic_bold(&self, font_id: FontId) -> bool {
        let loaded_font = self.loaded_font(font_id);
        if loaded_font.requested_weight.0 < 600 {
            return false;
        }
        let Some(face) = self.font_system.db().face(loaded_font.font.id()) else {
            return false;
        };
        let needs_synthetic = face.weight.0 < loaded_font.requested_weight.0;
        needs_synthetic
    }

    fn layout_line(&mut self, text: &str, font_size: Pixels, font_runs: &[FontRun]) -> LineLayout {
        let mut attrs_list = AttrsList::new(&Attrs::new());
        let mut offs = 0;
        for run in font_runs {
            let loaded_font = self.loaded_font(run.font_id);
            let font = self.font_system.db().face(loaded_font.font.id()).unwrap();

            attrs_list.add_span(
                offs..(offs + run.len),
                &Attrs::new()
                    .metadata(run.font_id.0)
                    .family(Family::Name(&font.families.first().unwrap().0))
                    .stretch(font.stretch)
                    .style(loaded_font.requested_style)
                    .weight(loaded_font.requested_weight)
                    .font_features(loaded_font.features.clone()),
            );
            offs += run.len;
        }

        let line = ShapeLine::new(
            &mut self.font_system,
            text,
            &attrs_list,
            cosmic_text::Shaping::Advanced,
            4,
        );
        let mut layout_lines = Vec::with_capacity(1);
        line.layout_to_buffer(
            &mut self.scratch,
            font_size.0,
            None,
            cosmic_text::Wrap::None,
            None,
            &mut layout_lines,
            None,
        );
        let layout = layout_lines.first().unwrap();

        let mut runs: Vec<ShapedRun> = Vec::new();
        for glyph in &layout.glyphs {
            let mut font_id = FontId(glyph.metadata);
            let mut loaded_font = self.loaded_font(font_id);
            let requested_weight = loaded_font.requested_weight;
            let requested_style = loaded_font.requested_style;
            if loaded_font.font.id() != glyph.font_id {
                font_id = self.font_id_for_cosmic_id_with_request(
                    glyph.font_id,
                    requested_weight,
                    requested_style,
                );
                loaded_font = self.loaded_font(font_id);
            }
            let is_emoji = loaded_font.is_known_emoji_font;

            if glyph.glyph_id == 3 && is_emoji {
                continue;
            }

            let shaped_glyph = ShapedGlyph {
                id: GlyphId(glyph.glyph_id as u32),
                position: point(glyph.x.into(), glyph.y.into()),
                index: glyph.start,
                is_emoji,
            };

            if let Some(last_run) = runs
                .last_mut()
                .filter(|last_run| last_run.font_id == font_id)
            {
                last_run.glyphs.push(shaped_glyph);
            } else {
                runs.push(ShapedRun {
                    font_id,
                    glyphs: vec![shaped_glyph],
                });
            }
        }

        LineLayout {
            font_size,
            width: layout.w.into(),
            ascent: layout.max_ascent.into(),
            descent: layout.max_descent.into(),
            runs,
            len: text.len(),
        }
    }
}

impl TryFrom<&FontFeatures> for CosmicFontFeatures {
    type Error = anyhow::Error;

    fn try_from(features: &FontFeatures) -> Result<Self> {
        let mut result = CosmicFontFeatures::new();
        for feature in features.0.iter() {
            let name_bytes: [u8; 4] = feature
                .0
                .as_bytes()
                .try_into()
                .context("Incorrect feature flag format")?;

            let tag = cosmic_text::FeatureTag::new(&name_bytes);

            result.set(tag, feature.1);
        }
        Ok(result)
    }
}

impl From<RectF> for Bounds<f32> {
    fn from(rect: RectF) -> Self {
        Bounds {
            origin: point(rect.origin_x(), rect.origin_y()),
            size: size(rect.width(), rect.height()),
        }
    }
}

impl From<RectI> for Bounds<DevicePixels> {
    fn from(rect: RectI) -> Self {
        Bounds {
            origin: point(DevicePixels(rect.origin_x()), DevicePixels(rect.origin_y())),
            size: size(DevicePixels(rect.width()), DevicePixels(rect.height())),
        }
    }
}

impl From<Vector2I> for Size<DevicePixels> {
    fn from(value: Vector2I) -> Self {
        size(value.x().into(), value.y().into())
    }
}

impl From<RectI> for Bounds<i32> {
    fn from(rect: RectI) -> Self {
        Bounds {
            origin: point(rect.origin_x(), rect.origin_y()),
            size: size(rect.width(), rect.height()),
        }
    }
}

impl From<Point<u32>> for Vector2I {
    fn from(size: Point<u32>) -> Self {
        Vector2I::new(size.x as i32, size.y as i32)
    }
}

impl From<Vector2F> for Size<f32> {
    fn from(vec: Vector2F) -> Self {
        size(vec.x(), vec.y())
    }
}

impl From<FontWeight> for cosmic_text::Weight {
    fn from(value: FontWeight) -> Self {
        cosmic_text::Weight(value.0 as u16)
    }
}

impl From<FontStyle> for cosmic_text::Style {
    fn from(style: FontStyle) -> Self {
        match style {
            FontStyle::Normal => cosmic_text::Style::Normal,
            FontStyle::Italic => cosmic_text::Style::Italic,
            FontStyle::Oblique => cosmic_text::Style::Oblique,
        }
    }
}

fn font_into_properties(font: &crate::Font) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match font.style {
            crate::FontStyle::Normal => font_kit::properties::Style::Normal,
            crate::FontStyle::Italic => font_kit::properties::Style::Italic,
            crate::FontStyle::Oblique => font_kit::properties::Style::Oblique,
        },
        weight: font_kit::properties::Weight(font.weight.0),
        stretch: Default::default(),
    }
}

fn face_info_into_properties(
    face_info: &cosmic_text::fontdb::FaceInfo,
) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match face_info.style {
            cosmic_text::Style::Normal => font_kit::properties::Style::Normal,
            cosmic_text::Style::Italic => font_kit::properties::Style::Italic,
            cosmic_text::Style::Oblique => font_kit::properties::Style::Oblique,
        },
        weight: font_kit::properties::Weight(face_info.weight.0.into()),
        stretch: match face_info.stretch {
            cosmic_text::Stretch::Condensed => font_kit::properties::Stretch::CONDENSED,
            cosmic_text::Stretch::Expanded => font_kit::properties::Stretch::EXPANDED,
            cosmic_text::Stretch::ExtraCondensed => font_kit::properties::Stretch::EXTRA_CONDENSED,
            cosmic_text::Stretch::ExtraExpanded => font_kit::properties::Stretch::EXTRA_EXPANDED,
            cosmic_text::Stretch::Normal => font_kit::properties::Stretch::NORMAL,
            cosmic_text::Stretch::SemiCondensed => font_kit::properties::Stretch::SEMI_CONDENSED,
            cosmic_text::Stretch::SemiExpanded => font_kit::properties::Stretch::SEMI_EXPANDED,
            cosmic_text::Stretch::UltraCondensed => font_kit::properties::Stretch::ULTRA_CONDENSED,
            cosmic_text::Stretch::UltraExpanded => font_kit::properties::Stretch::ULTRA_EXPANDED,
        },
    }
}

fn check_is_known_emoji_font(postscript_name: &str) -> bool {
    postscript_name == "NotoColorEmoji"
}

fn embolden_bitmap(data: &mut [u8], width: usize, height: usize) {
    if width == 0 || height == 0 {
        return;
    }
    let pixel_count = width.saturating_mul(height);
    if pixel_count == 0 || data.is_empty() || data.len() % pixel_count != 0 {
        return;
    }
    let channels = data.len() / pixel_count;
    if channels == 0 {
        return;
    }

    let original = data.to_vec();
    for y in 0..height {
        for x in 1..width {
            let dst_base = (y * width + x) * channels;
            let src_base = (y * width + (x - 1)) * channels;
            for c in 0..channels {
                let dst = dst_base + c;
                let src = src_base + c;
                data[dst] = data[dst].max(original[src]);
            }
        }
    }
}
