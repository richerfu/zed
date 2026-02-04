#![cfg(target_env = "ohos")]

use blade_graphics as gpu;

use crate::platform::blade::{
    GpuMonochromeSprite, GpuPolychromeSprite, GpuQuad, GpuShadow, GpuUnderline,
    PathRasterizationVertex, PathSprite,
};

/// Create a VertexLayout for Quad struct.
/// The layout matches QuadVertexInput in ohos/shaders.wgsl
pub(crate) fn quad_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) order_border_style: vec2<u32> - order + border_style at offset 0
            (
                "order_border_style",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(1) bounds_origin: vec2<f32> - bounds.origin at offset 8
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(2) bounds_size: vec2<f32> - bounds.size at offset 16
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(3) content_mask_origin: vec2<f32> - content_mask.bounds.origin at offset 24
            (
                "content_mask_origin",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(4) content_mask_size: vec2<f32> - content_mask.bounds.size at offset 32
            (
                "content_mask_size",
                gpu::VertexAttribute {
                    offset: 32,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(5) background_tag_colorspace: vec2<u32> - background.tag + color_space at offset 40
            (
                "background_tag_colorspace",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(6) background_solid: vec4<f32> - background.solid (Hsla) at offset 48
            (
                "background_solid",
                gpu::VertexAttribute {
                    offset: 48,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(7) background_angle: f32 - background.gradient_angle_or_pattern_height at offset 64
            (
                "background_angle",
                gpu::VertexAttribute {
                    offset: 64,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(8) background_color0: vec4<f32> - background.colors[0].color at offset 68
            (
                "background_color0",
                gpu::VertexAttribute {
                    offset: 68,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(9) background_stop0: f32 - background.colors[0].percentage at offset 84
            (
                "background_stop0",
                gpu::VertexAttribute {
                    offset: 84,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(10) background_color1: vec4<f32> - background.colors[1].color at offset 88
            (
                "background_color1",
                gpu::VertexAttribute {
                    offset: 88,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(11) background_stop1: f32 - background.colors[1].percentage at offset 104
            (
                "background_stop1",
                gpu::VertexAttribute {
                    offset: 104,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(12) border_color: vec4<f32> - border_color at offset 112
            (
                "border_color",
                gpu::VertexAttribute {
                    offset: 112,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(13) corner_radii: vec4<f32> - corner_radii at offset 128
            (
                "corner_radii",
                gpu::VertexAttribute {
                    offset: 128,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(14) border_widths: vec4<f32> - border_widths at offset 144
            (
                "border_widths",
                gpu::VertexAttribute {
                    offset: 144,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
        ],
        stride: std::mem::size_of::<GpuQuad>() as u32, // 160 bytes
    }
}

/// Create a VertexLayout for Shadow struct.
/// Shadow struct layout:
/// - order: u32 (offset 0)
/// - blur_radius: f32 (offset 4) - ScaledPixels wraps f32
/// - bounds: Bounds (offset 8, 16 bytes)
/// - corner_radii: Corners (offset 24, 16 bytes)
/// - content_mask: ContentMask (offset 40, 16 bytes)
/// - color: Hsla (offset 56, 16 bytes)
/// Total: 72 bytes
pub(crate) fn shadow_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) order: u32
            (
                "order",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::U32,
                },
            ),
            // @location(1) blur_radius: f32
            (
                "blur_radius",
                gpu::VertexAttribute {
                    offset: 4,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(2) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(3) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(4) corner_radii: vec4<f32>
            (
                "corner_radii",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(5) content_mask_origin: vec2<f32>
            (
                "content_mask_origin",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(6) content_mask_size: vec2<f32>
            (
                "content_mask_size",
                gpu::VertexAttribute {
                    offset: 48,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(7) color: vec4<f32>
            (
                "color",
                gpu::VertexAttribute {
                    offset: 56,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
        ],
        stride: std::mem::size_of::<GpuShadow>() as u32,
    }
}

/// Create a VertexLayout for PathRasterizationVertex struct.
pub(crate) fn path_rasterization_vertex_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) xy_position: vec2<f32>
            (
                "xy_position",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(1) st_position: vec2<f32>
            (
                "st_position",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // Background starts at offset 16
            // @location(2) background_tag_colorspace: vec2<u32>
            (
                "background_tag_colorspace",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(3) background_solid: vec4<f32> - at offset 24
            (
                "background_solid",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(4) background_angle: f32 - at offset 40
            (
                "background_angle",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(5) background_color0: vec4<f32> - at offset 44
            (
                "background_color0",
                gpu::VertexAttribute {
                    offset: 44,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(6) background_stop0: f32 - at offset 60
            (
                "background_stop0",
                gpu::VertexAttribute {
                    offset: 60,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(7) background_color1: vec4<f32> - at offset 64
            (
                "background_color1",
                gpu::VertexAttribute {
                    offset: 64,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(8) background_stop1: f32 - at offset 80
            (
                "background_stop1",
                gpu::VertexAttribute {
                    offset: 80,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // Bounds starts after Background (84 + 4 pad = 88)
            // @location(9) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 88,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(10) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 96,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
        ],
        stride: std::mem::size_of::<PathRasterizationVertex>() as u32,
    }
}

/// Create a VertexLayout for PathSprite struct.
pub(crate) fn path_sprite_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(1) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
        ],
        stride: std::mem::size_of::<PathSprite>() as u32,
    }
}

/// Create a VertexLayout for Underline struct.
pub(crate) fn underline_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) order: u32
            (
                "order",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::U32,
                },
            ),
            // @location(1) pad: u32
            (
                "pad",
                gpu::VertexAttribute {
                    offset: 4,
                    format: gpu::VertexFormat::U32,
                },
            ),
            // @location(2) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(3) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(4) content_mask_origin: vec2<f32>
            (
                "content_mask_origin",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(5) content_mask_size: vec2<f32>
            (
                "content_mask_size",
                gpu::VertexAttribute {
                    offset: 32,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(6) color: vec4<f32>
            (
                "color",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // @location(7) thickness: f32
            (
                "thickness",
                gpu::VertexAttribute {
                    offset: 56,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(8) wavy: u32
            (
                "wavy",
                gpu::VertexAttribute {
                    offset: 60,
                    format: gpu::VertexFormat::U32,
                },
            ),
        ],
        stride: std::mem::size_of::<GpuUnderline>() as u32,
    }
}

/// Create a VertexLayout for MonochromeSprite struct.
pub(crate) fn mono_sprite_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) order_pad: vec2<u32> - order + pad
            (
                "order_pad",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(1) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(2) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(3) content_mask_origin: vec2<f32>
            (
                "content_mask_origin",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(4) content_mask_size: vec2<f32>
            (
                "content_mask_size",
                gpu::VertexAttribute {
                    offset: 32,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(5) color: vec4<f32>
            (
                "color",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // AtlasTile starts at offset 56
            // @location(6) tile_texture_id: vec2<u32> - texture_id.index + texture_id.kind
            (
                "tile_texture_id",
                gpu::VertexAttribute {
                    offset: 56,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(7) tile_id_padding: vec2<u32> - tile_id + padding
            (
                "tile_id_padding",
                gpu::VertexAttribute {
                    offset: 64,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(8) tile_bounds_origin: vec2<i32>
            (
                "tile_bounds_origin",
                gpu::VertexAttribute {
                    offset: 72,
                    format: gpu::VertexFormat::I32Vec2,
                },
            ),
            // @location(9) tile_bounds_size: vec2<i32>
            (
                "tile_bounds_size",
                gpu::VertexAttribute {
                    offset: 80,
                    format: gpu::VertexFormat::I32Vec2,
                },
            ),
            // TransformationMatrix starts at offset 88
            // @location(10) transform_row0: vec2<f32> - rotation_scale[0]
            (
                "transform_row0",
                gpu::VertexAttribute {
                    offset: 88,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(11) transform_row1: vec2<f32> - rotation_scale[1]
            (
                "transform_row1",
                gpu::VertexAttribute {
                    offset: 96,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(12) transform_translation: vec2<f32>
            (
                "transform_translation",
                gpu::VertexAttribute {
                    offset: 104,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
        ],
        stride: std::mem::size_of::<GpuMonochromeSprite>() as u32,
    }
}

/// Create a VertexLayout for PolychromeSprite struct.
pub(crate) fn poly_sprite_layout() -> gpu::VertexLayout {
    gpu::VertexLayout {
        attributes: vec![
            // @location(0) order_pad: vec2<u32> - order + pad
            (
                "order_pad",
                gpu::VertexAttribute {
                    offset: 0,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(1) grayscale: u32
            (
                "grayscale",
                gpu::VertexAttribute {
                    offset: 8,
                    format: gpu::VertexFormat::U32,
                },
            ),
            // @location(2) opacity: f32
            (
                "opacity",
                gpu::VertexAttribute {
                    offset: 12,
                    format: gpu::VertexFormat::F32,
                },
            ),
            // @location(3) bounds_origin: vec2<f32>
            (
                "bounds_origin",
                gpu::VertexAttribute {
                    offset: 16,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(4) bounds_size: vec2<f32>
            (
                "bounds_size",
                gpu::VertexAttribute {
                    offset: 24,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(5) content_mask_origin: vec2<f32>
            (
                "content_mask_origin",
                gpu::VertexAttribute {
                    offset: 32,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(6) content_mask_size: vec2<f32>
            (
                "content_mask_size",
                gpu::VertexAttribute {
                    offset: 40,
                    format: gpu::VertexFormat::F32Vec2,
                },
            ),
            // @location(7) corner_radii: vec4<f32>
            (
                "corner_radii",
                gpu::VertexAttribute {
                    offset: 48,
                    format: gpu::VertexFormat::F32Vec4,
                },
            ),
            // AtlasTile starts at offset 64
            // @location(8) tile_texture_id: vec2<u32>
            (
                "tile_texture_id",
                gpu::VertexAttribute {
                    offset: 64,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(9) tile_id_padding: vec2<u32>
            (
                "tile_id_padding",
                gpu::VertexAttribute {
                    offset: 72,
                    format: gpu::VertexFormat::U32Vec2,
                },
            ),
            // @location(10) tile_bounds_origin: vec2<i32>
            (
                "tile_bounds_origin",
                gpu::VertexAttribute {
                    offset: 80,
                    format: gpu::VertexFormat::I32Vec2,
                },
            ),
            // @location(11) tile_bounds_size: vec2<i32>
            (
                "tile_bounds_size",
                gpu::VertexAttribute {
                    offset: 88,
                    format: gpu::VertexFormat::I32Vec2,
                },
            ),
        ],
        stride: std::mem::size_of::<GpuPolychromeSprite>() as u32,
    }
}
