//! Blending layers into one frame.
//!
//! [`CpuCompositor`] is the reference implementation: obvious, dependency-free,
//! and slow enough that it will eventually only be used for tests and for
//! checking the GPU path's output. The `Compositor` trait is the seam that
//! `wgpu` will slot into.

use wolfcut_core::BlendMode;
use wolfcut_core::frame::{BYTES_PER_PIXEL, Frame};

use crate::blend::blend_rgb;

/// A layer's placement beyond its base position, in output pixels.
///
/// Applied about the layer's centre: scale first, then rotation, then the
/// translation. Pixel-space on purpose - the resolution-independent form
/// lives in `wolfcut-core::timeline::Transform`, and whoever builds layers
/// converts exactly once.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    /// Multiplier over the layer's own size.
    pub scale: f32,
    /// Clockwise rotation about the layer's centre, in radians.
    pub rotation: f32,
    /// Horizontal offset of the layer's centre from its base position, pixels.
    pub translate_x: f32,
    /// Vertical offset of the layer's centre from its base position, pixels.
    pub translate_y: f32,
}

impl Placement {
    /// Unscaled, unrotated, unmoved.
    pub const IDENTITY: Placement =
        Placement { scale: 1.0, rotation: 0.0, translate_x: 0.0, translate_y: 0.0 };

    /// True when applying this placement would change nothing.
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// One source image, positioned and weighted.
#[derive(Clone, Copy, Debug)]
pub struct Layer<'a> {
    /// The pixels to draw.
    pub frame: &'a Frame,
    /// Blend strength over what is beneath, in `0.0..=1.0`.
    pub opacity: f32,
    /// Horizontal offset of the layer's top-left corner. May be negative.
    pub x: i32,
    /// Vertical offset of the layer's top-left corner. May be negative.
    pub y: i32,
    /// Scale, rotation and translation about the layer's centre.
    pub placement: Placement,
    /// How this layer's colour combines with what is beneath it. Orthogonal
    /// to `opacity`: the mode decides what colour lands, `opacity` decides
    /// how much of it does.
    pub blend_mode: BlendMode,
}

impl<'a> Layer<'a> {
    /// A layer drawn at the origin, fully opaque.
    pub fn new(frame: &'a Frame) -> Self {
        Self {
            frame,
            opacity: 1.0,
            x: 0,
            y: 0,
            placement: Placement::IDENTITY,
            blend_mode: BlendMode::Normal,
        }
    }

    /// Sets the blend strength.
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// Sets the top-left corner.
    pub fn at(mut self, x: i32, y: i32) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    /// Sets the scale/rotation/translation.
    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Sets how the layer's colour combines with what is beneath it.
    pub fn with_blend_mode(mut self, blend_mode: BlendMode) -> Self {
        self.blend_mode = blend_mode;
        self
    }
}

/// Blends layers into a single output frame.
pub trait Compositor {
    /// Draws `layers` bottom-most first over an opaque black background.
    ///
    /// Layers may hang off any edge; anything outside the output is clipped.
    /// The result is always fully opaque - it is what goes to screen or to an
    /// encoder, and neither has anything to show through.
    ///
    /// Every implementation must honour each layer's
    /// [`BlendMode`](wolfcut_core::BlendMode), taking a slower path rather
    /// than ignoring one: what an export looks like must not depend on which
    /// backend the machine could offer.
    ///
    /// The background is opaque black, so a darkening mode on the bottom-most
    /// layer really does blend against black. That is what every other editor
    /// does too, and it is why such a mode belongs on an upper track.
    fn composite(&mut self, width: u32, height: u32, layers: &[Layer<'_>]) -> Frame;
}

/// A straightforward CPU compositor.
#[derive(Clone, Copy, Default, Debug)]
pub struct CpuCompositor;

impl Compositor for CpuCompositor {
    fn composite(&mut self, width: u32, height: u32, layers: &[Layer<'_>]) -> Frame {
        let mut output = Frame::black(width, height);

        for layer in layers {
            let opacity = layer.opacity.clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }

            let source = layer.frame;

            if !layer.placement.is_identity() {
                blend_transformed(&mut output, layer, opacity);
                continue;
            }

            let Some((dst_x, src_x, columns)) = overlap(layer.x, source.width(), width) else {
                continue;
            };
            let Some((dst_y, src_y, rows)) = overlap(layer.y, source.height(), height) else {
                continue;
            };

            blend_region(
                &mut output,
                source,
                (dst_x, dst_y),
                (src_x, src_y),
                (columns, rows),
                opacity,
                layer.blend_mode,
            );
        }

        output
    }
}

/// Draws one layer through its placement: inverse-mapped, bilinearly sampled.
///
/// Each covered output pixel is carried backwards through the placement into
/// source coordinates and sampled there. Inverse mapping is what makes the
/// result hole-free at any scale or angle; bilinear is the cheapest filter
/// that does not shimmer on motion. Only the transformed bounding box is
/// visited, so a small layer stays cheap on a large frame.
fn blend_transformed(output: &mut Frame, layer: &Layer<'_>, opacity: f32) {
    let source = layer.frame;
    let placement = layer.placement;
    let blend_mode = layer.blend_mode;
    let scale = placement.scale.max(1e-6);
    let (sin, cos) = placement.rotation.sin_cos();

    let src_w = source.width() as f32;
    let src_h = source.height() as f32;
    // The layer's centre in output space, after translation.
    let centre_x = layer.x as f32 + src_w / 2.0 + placement.translate_x;
    let centre_y = layer.y as f32 + src_h / 2.0 + placement.translate_y;

    // Bounding box of the transformed rectangle, clamped to the output.
    let half_w = src_w * scale / 2.0;
    let half_h = src_h * scale / 2.0;
    let reach_x = (half_w * cos.abs()) + (half_h * sin.abs());
    let reach_y = (half_w * sin.abs()) + (half_h * cos.abs());

    let x_from = ((centre_x - reach_x).floor().max(0.0)) as u32;
    let y_from = ((centre_y - reach_y).floor().max(0.0)) as u32;
    let x_to = ((centre_x + reach_x).ceil().min(output.width() as f32)) as u32;
    let y_to = ((centre_y + reach_y).ceil().min(output.height() as f32)) as u32;
    if x_from >= x_to || y_from >= y_to {
        return;
    }

    let dst_stride = output.width() as usize * BYTES_PER_PIXEL;
    let src_stride = source.width() as usize * BYTES_PER_PIXEL;
    let src_pixels = source.pixels();
    let dst_pixels = output.pixels_mut();

    for y in y_from..y_to {
        for x in x_from..x_to {
            // Sample at the pixel centre, mapped back into source space:
            // untranslate, unrotate, unscale, then re-origin at the corner.
            let dx = (x as f32 + 0.5) - centre_x;
            let dy = (y as f32 + 0.5) - centre_y;
            let sx = (dx * cos + dy * sin) / scale + src_w / 2.0 - 0.5;
            let sy = (-dx * sin + dy * cos) / scale + src_h / 2.0 - 0.5;

            if sx < -0.5 || sy < -0.5 || sx > src_w - 0.5 || sy > src_h - 0.5 {
                continue;
            }

            let x0 = sx.floor().max(0.0) as usize;
            let y0 = sy.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(source.width() as usize - 1);
            let y1 = (y0 + 1).min(source.height() as usize - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);
            let fy = (sy - y0 as f32).clamp(0.0, 1.0);

            let mut sample = [0.0f32; 4];
            for (corner_x, corner_y, weight) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                (x1, y0, fx * (1.0 - fy)),
                (x0, y1, (1.0 - fx) * fy),
                (x1, y1, fx * fy),
            ] {
                let at = corner_y * src_stride + corner_x * BYTES_PER_PIXEL;
                for channel in 0..4 {
                    sample[channel] += f32::from(src_pixels[at + channel]) * weight;
                }
            }

            let alpha = (sample[3] / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }

            let at = y as usize * dst_stride + x as usize * BYTES_PER_PIXEL;
            let colour = blended_source(
                [sample[0], sample[1], sample[2]],
                [dst_pixels[at], dst_pixels[at + 1], dst_pixels[at + 2]],
                blend_mode,
            );
            for channel in 0..3 {
                let over = colour[channel] * alpha;
                let under = f32::from(dst_pixels[at + channel]) * (1.0 - alpha);
                dst_pixels[at + channel] = (over + under).round().clamp(0.0, 255.0) as u8;
            }
            dst_pixels[at + 3] = 255;
        }
    }
}

/// Works out the visible span along one axis when a layer of `source` pixels is
/// placed at `offset` inside a destination of `destination` pixels.
///
/// Returns `(destination_start, source_start, count)`, or `None` when the layer
/// falls entirely outside.
fn overlap(offset: i32, source: u32, destination: u32) -> Option<(u32, u32, u32)> {
    let source = i64::from(source);
    let destination = i64::from(destination);
    let offset = i64::from(offset);

    let dst_start = offset.max(0);
    let src_start = (-offset).max(0);
    let count = (source - src_start).min(destination - dst_start);

    (count > 0).then_some((dst_start as u32, src_start as u32, count as u32))
}

/// The colour a layer contributes at one pixel, in `0..=255`, once `mode` has
/// combined it with what is already beneath it.
///
/// All three destination channels are read before any of them is written: a
/// blend mode looks at the colour underneath as a whole, and mixing channel by
/// channel against a half-updated pixel would be a quiet, ugly bug.
///
/// [`BlendMode::Normal`] is the overwhelmingly common case and skips the trip
/// through normalised floats entirely. This runs once per pixel per layer, so
/// the fast path is worth having.
fn blended_source(source: [f32; 3], destination: [u8; 3], mode: BlendMode) -> [f32; 3] {
    if mode == BlendMode::Normal {
        return source;
    }
    let base = destination.map(|channel| f32::from(channel) / 255.0);
    let layer = source.map(|channel| (channel / 255.0).clamp(0.0, 1.0));
    blend_rgb(base, layer, mode).map(|channel| channel * 255.0)
}

/// Source-over alpha blend of an aligned rectangle.
///
/// The shader this ports from weighs the blended colour by the destination's
/// alpha as well as the source's. Here the destination is always opaque - the
/// output starts as [`Frame::black`] and every pixel written ends fully
/// opaque - which collapses that to the plain source-over below. Break that
/// invariant and this arithmetic silently becomes wrong.
fn blend_region(
    output: &mut Frame,
    source: &Frame,
    (dst_x, dst_y): (u32, u32),
    (src_x, src_y): (u32, u32),
    (columns, rows): (u32, u32),
    opacity: f32,
    blend_mode: BlendMode,
) {
    let src_stride = source.width() as usize * BYTES_PER_PIXEL;
    let dst_stride = output.width() as usize * BYTES_PER_PIXEL;
    let span = columns as usize * BYTES_PER_PIXEL;

    let src_pixels = source.pixels();
    let dst_pixels = output.pixels_mut();

    for row in 0..rows as usize {
        let src_offset = (src_y as usize + row) * src_stride + src_x as usize * BYTES_PER_PIXEL;
        let dst_offset = (dst_y as usize + row) * dst_stride + dst_x as usize * BYTES_PER_PIXEL;

        let src_row = &src_pixels[src_offset..src_offset + span];
        let dst_row = &mut dst_pixels[dst_offset..dst_offset + span];

        for (src_pixel, dst_pixel) in
            src_row.chunks_exact(BYTES_PER_PIXEL).zip(dst_row.chunks_exact_mut(BYTES_PER_PIXEL))
        {
            let alpha = (f32::from(src_pixel[3]) / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }
            let colour = blended_source(
                [f32::from(src_pixel[0]), f32::from(src_pixel[1]), f32::from(src_pixel[2])],
                [dst_pixel[0], dst_pixel[1], dst_pixel[2]],
                blend_mode,
            );
            for channel in 0..3 {
                let over = colour[channel] * alpha;
                let under = f32::from(dst_pixel[channel]) * (1.0 - alpha);
                dst_pixel[channel] = (over + under).round().clamp(0.0, 255.0) as u8;
            }
            dst_pixel[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Frame {
        let mut frame = Frame::transparent(width, height);
        frame.fill(rgba);
        frame
    }

    #[test]
    fn no_layers_gives_opaque_black() {
        let frame = CpuCompositor.composite(2, 2, &[]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn an_opaque_layer_replaces_the_background() {
        let red = solid(2, 2, [255, 0, 0, 255]);
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&red)]);
        assert_eq!(frame.pixel(1, 1), Some([255, 0, 0, 255]));
    }

    #[test]
    fn half_opacity_lands_halfway() {
        let white = solid(1, 1, [255, 255, 255, 255]);
        let frame = CpuCompositor.composite(1, 1, &[Layer::new(&white).with_opacity(0.5)]);
        assert_eq!(frame.pixel(0, 0), Some([128, 128, 128, 255]));
    }

    #[test]
    fn source_alpha_and_layer_opacity_multiply() {
        let half_alpha = solid(1, 1, [255, 255, 255, 128]);
        let frame = CpuCompositor.composite(1, 1, &[Layer::new(&half_alpha).with_opacity(0.5)]);
        // 128/255 * 0.5 ~= 0.251
        assert_eq!(frame.pixel(0, 0), Some([64, 64, 64, 255]));
    }

    #[test]
    fn later_layers_draw_on_top() {
        let red = solid(1, 1, [255, 0, 0, 255]);
        let blue = solid(1, 1, [0, 0, 255, 255]);
        let frame = CpuCompositor.composite(1, 1, &[Layer::new(&red), Layer::new(&blue)]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 255, 255]));
    }

    #[test]
    fn a_transparent_layer_changes_nothing() {
        let clear = Frame::transparent(2, 2);
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&clear)]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn offset_layers_are_clipped_not_wrapped() {
        let red = solid(2, 2, [255, 0, 0, 255]);
        // Placed so only its bottom-right pixel lands on the output's top-left.
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&red).at(-1, -1)]);
        assert_eq!(frame.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(frame.pixel(1, 1), Some([0, 0, 0, 255]), "must not wrap around");
    }

    #[test]
    fn a_layer_entirely_off_screen_is_skipped() {
        let red = solid(2, 2, [255, 0, 0, 255]);
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&red).at(50, 50)]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn a_layer_larger_than_the_output_is_cropped() {
        let red = solid(8, 8, [255, 0, 0, 255]);
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&red)]);
        assert_eq!(frame.width(), 2);
        assert_eq!(frame.pixel(1, 1), Some([255, 0, 0, 255]));
    }

    #[test]
    fn scaling_doubles_coverage() {
        let red = solid(2, 2, [255, 0, 0, 255]);
        let placement = Placement { scale: 2.0, ..Placement::IDENTITY };
        let frame =
            CpuCompositor.composite(4, 4, &[Layer::new(&red).at(1, 1).with_placement(placement)]);
        // A 2x2 layer based at (1,1) scaled x2 about its centre covers the
        // whole 4x4 output.
        assert_eq!(frame.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(frame.pixel(3, 3), Some([255, 0, 0, 255]));
    }

    #[test]
    fn translation_moves_the_layer() {
        let red = solid(1, 1, [255, 0, 0, 255]);
        let placement = Placement { translate_x: 2.0, ..Placement::IDENTITY };
        let frame = CpuCompositor.composite(3, 1, &[Layer::new(&red).with_placement(placement)]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(frame.pixel(2, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn a_half_turn_swaps_the_ends() {
        let mut strip = Frame::transparent(2, 1);
        strip.set_pixel(0, 0, [255, 0, 0, 255]);
        strip.set_pixel(1, 0, [0, 0, 255, 255]);
        let placement = Placement { rotation: std::f32::consts::PI, ..Placement::IDENTITY };
        let frame = CpuCompositor.composite(2, 1, &[Layer::new(&strip).with_placement(placement)]);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 255, 255]));
        assert_eq!(frame.pixel(1, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn a_transformed_layer_is_clipped_at_the_frame_edge() {
        let red = solid(2, 2, [255, 0, 0, 255]);
        let placement = Placement { scale: 100.0, ..Placement::IDENTITY };
        let frame = CpuCompositor.composite(2, 2, &[Layer::new(&red).with_placement(placement)]);
        assert_eq!(frame.width(), 2);
        assert_eq!(frame.pixel(1, 1), Some([255, 0, 0, 255]));
    }

    #[test]
    fn multiply_darkens_against_the_layer_beneath() {
        let grey = solid(1, 1, [200, 200, 200, 255]);
        let half = solid(1, 1, [128, 128, 128, 255]);
        let frame = CpuCompositor.composite(
            1,
            1,
            &[Layer::new(&grey), Layer::new(&half).with_blend_mode(BlendMode::Multiply)],
        );
        // 200/255 * 128/255, back in bytes, is 100.4.
        assert_eq!(frame.pixel(0, 0), Some([100, 100, 100, 255]));
    }

    #[test]
    fn a_white_multiply_layer_changes_nothing() {
        let red = solid(1, 1, [255, 0, 0, 255]);
        let white = solid(1, 1, [255, 255, 255, 255]);
        let frame = CpuCompositor.composite(
            1,
            1,
            &[Layer::new(&red), Layer::new(&white).with_blend_mode(BlendMode::Multiply)],
        );
        assert_eq!(frame.pixel(0, 0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn the_bottom_most_layer_blends_against_the_black_background() {
        let grey = solid(1, 1, [200, 200, 200, 255]);
        let layers = [Layer::new(&grey).with_blend_mode(BlendMode::Multiply)];
        let frame = CpuCompositor.composite(1, 1, &layers);
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]), "there is only black beneath it");
    }

    #[test]
    fn difference_cancels_a_layer_against_itself() {
        let colour = solid(1, 1, [90, 140, 210, 255]);
        let frame = CpuCompositor.composite(
            1,
            1,
            &[Layer::new(&colour), Layer::new(&colour).with_blend_mode(BlendMode::Difference)],
        );
        assert_eq!(frame.pixel(0, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn overlay_and_hard_light_are_not_each_other() {
        let grey = solid(1, 1, [200, 200, 200, 255]);
        let dark = solid(1, 1, [64, 64, 64, 255]);
        let through = |mode: BlendMode| {
            CpuCompositor
                .composite(1, 1, &[Layer::new(&grey), Layer::new(&dark).with_blend_mode(mode)])
                .pixel(0, 0)
        };
        // The light picture beneath decides for overlay, the dark layer for
        // hard light, so the same pair of colours lands far apart.
        assert_eq!(through(BlendMode::Overlay), Some([173, 173, 173, 255]));
        assert_eq!(through(BlendMode::HardLight), Some([100, 100, 100, 255]));
    }

    #[test]
    fn opacity_still_weighs_a_blended_layer() {
        let white = solid(1, 1, [255, 255, 255, 255]);
        let black = solid(1, 1, [0, 0, 0, 255]);
        // Multiplying by black gives black; at half strength that lands half
        // way down from the white beneath.
        let frame = CpuCompositor.composite(
            1,
            1,
            &[
                Layer::new(&white),
                Layer::new(&black).with_opacity(0.5).with_blend_mode(BlendMode::Multiply),
            ],
        );
        assert_eq!(frame.pixel(0, 0), Some([128, 128, 128, 255]));
    }

    #[test]
    fn a_transformed_layer_blends_too() {
        let grey = solid(2, 2, [200, 200, 200, 255]);
        let half = solid(2, 2, [128, 128, 128, 255]);
        let placement = Placement { scale: 2.0, ..Placement::IDENTITY };
        let frame = CpuCompositor.composite(
            2,
            2,
            &[
                Layer::new(&grey),
                Layer::new(&half).with_placement(placement).with_blend_mode(BlendMode::Multiply),
            ],
        );
        assert_eq!(frame.pixel(0, 0), Some([100, 100, 100, 255]));
        assert_eq!(frame.pixel(1, 1), Some([100, 100, 100, 255]));
    }

    #[test]
    fn overlap_spans_are_correct() {
        assert_eq!(overlap(0, 4, 4), Some((0, 0, 4)));
        assert_eq!(overlap(2, 4, 4), Some((2, 0, 2)));
        assert_eq!(overlap(-2, 4, 4), Some((0, 2, 2)));
        assert_eq!(overlap(4, 4, 4), None);
        assert_eq!(overlap(-4, 4, 4), None);
    }
}
