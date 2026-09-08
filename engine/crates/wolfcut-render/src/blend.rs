//! The arithmetic behind [`BlendMode`].
//!
//! A line-by-line port of OpenCut's `blend.wgsl` fragment shader into plain
//! Rust, so the CPU compositor can do exactly what a GPU one will do later.
//! Where the shader and the CSS/PDF specifications disagree the shader wins:
//! the point of this file is that two backends produce the same pixels, not
//! that it reads like the specification.
//!
//! Everything here works on normalised channels in `0.0..=1.0`, not bytes.
//! Bytes are the compositor's problem.

use wolfcut_core::BlendMode;

/// The perceived brightness of a colour, on the specifications' weights.
fn lum(c: [f32; 3]) -> f32 {
    c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11
}

/// How far apart a colour's lightest and darkest channels are.
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// Pulls a colour back inside the cube by squeezing it towards its own
/// brightness, which is what keeps [`set_lum`] from simply clipping - and
/// clipping is what turns a tint into a posterised mess.
///
/// Both divisors are safe. A zero divisor needs all three channels equal
/// *and* outside `0.0..=1.0`; every caller reaches here through [`set_lum`],
/// whose output for an all-equal input is exactly the target brightness, and
/// that target always comes from a colour already inside the cube.
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;

    if n < 0.0 {
        for channel in &mut out {
            *channel = l + (*channel - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for channel in &mut out {
            *channel = l + (*channel - l) * (1.0 - l) / (x - l);
        }
    }
    out
}

/// The same colour at a different brightness.
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let shift = l - lum(c);
    clip_color([c[0] + shift, c[1] + shift, c[2] + shift])
}

/// The same colour at a different saturation. A colour with no spread at all
/// has no hue to keep, so it comes back black.
fn set_sat(c: [f32; 3], target: f32) -> [f32; 3] {
    let max_value = c[0].max(c[1]).max(c[2]);
    let min_value = c[0].min(c[1]).min(c[2]);
    if max_value <= min_value {
        return [0.0, 0.0, 0.0];
    }
    let scale = target / (max_value - min_value);
    [(c[0] - min_value) * scale, (c[1] - min_value) * scale, (c[2] - min_value) * scale]
}

/// Multiply below the halfway point, screen above it, with `layer` choosing.
///
/// Both halves are symmetric in their arguments, so swapping them swaps only
/// which colour decides - which is exactly the difference between
/// [`BlendMode::HardLight`] and [`BlendMode::Overlay`].
fn hard_light(base: [f32; 3], layer: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| {
        if layer[i] >= 0.5 {
            1.0 - 2.0 * (1.0 - base[i]) * (1.0 - layer[i])
        } else {
            2.0 * base[i] * layer[i]
        }
    })
}

/// [`BlendMode::SoftLight`] for one channel. The `d` term is the
/// specifications' cheap stand-in for a smooth curve near black.
fn soft_light_channel(base: f32, layer: f32) -> f32 {
    if layer <= 0.5 {
        return base - (1.0 - 2.0 * layer) * base * (1.0 - base);
    }
    let d = if base > 0.25 {
        base.sqrt()
    } else {
        ((16.0 * base - 12.0) * base + 4.0) * base
    };
    base + (2.0 * layer - 1.0) * (d - base)
}

/// Note the shader's edge case, kept deliberately: a fully lit layer gives
/// white even over black, where the specifications give black.
fn color_dodge(base: [f32; 3], layer: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| {
        if layer[i] >= 1.0 {
            1.0
        } else {
            (base[i] / (1.0 - layer[i]).max(0.0001)).min(1.0)
        }
    })
}

/// The mirror of [`color_dodge`], with the same deliberate edge case at the
/// far end: a fully dark layer gives black even over white.
fn color_burn(base: [f32; 3], layer: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| {
        if layer[i] <= 0.0 {
            0.0
        } else {
            1.0 - ((1.0 - base[i]) / layer[i].max(0.0001)).min(1.0)
        }
    })
}

/// Combines `layer` with the `base` beneath it, both normalised to
/// `0.0..=1.0`, and clamps the result back into that range.
///
/// This is colour only. Alpha is not touched and not consulted: the caller
/// still weighs the result against what is underneath, because how much of a
/// layer lands is a separate question from what colour it lands as.
/// [`BlendMode::Normal`] therefore returns `layer` unchanged.
pub fn blend_rgb(base: [f32; 3], layer: [f32; 3], mode: BlendMode) -> [f32; 3] {
    let blended: [f32; 3] = match mode {
        BlendMode::Normal => layer,
        BlendMode::Darken => std::array::from_fn(|i| base[i].min(layer[i])),
        BlendMode::Multiply => std::array::from_fn(|i| base[i] * layer[i]),
        BlendMode::ColorBurn => color_burn(base, layer),
        BlendMode::Lighten => std::array::from_fn(|i| base[i].max(layer[i])),
        BlendMode::Screen => std::array::from_fn(|i| 1.0 - (1.0 - base[i]) * (1.0 - layer[i])),
        BlendMode::PlusLighter => std::array::from_fn(|i| (base[i] + layer[i]).min(1.0)),
        BlendMode::ColorDodge => color_dodge(base, layer),
        // Not a typo: overlay is hard light with the roles swapped, so the
        // picture beneath decides which half of the curve each channel takes.
        BlendMode::Overlay => hard_light(layer, base),
        BlendMode::SoftLight => std::array::from_fn(|i| soft_light_channel(base[i], layer[i])),
        BlendMode::HardLight => hard_light(base, layer),
        BlendMode::Difference => std::array::from_fn(|i| (base[i] - layer[i]).abs()),
        BlendMode::Exclusion => {
            std::array::from_fn(|i| base[i] + layer[i] - 2.0 * base[i] * layer[i])
        }
        BlendMode::Hue => set_lum(set_sat(layer, sat(base)), lum(base)),
        BlendMode::Saturation => set_lum(set_sat(base, sat(layer)), lum(base)),
        BlendMode::Color => set_lum(layer, lum(base)),
        BlendMode::Luminosity => set_lum(base, lum(layer)),
    };
    blended.map(|channel| channel.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mode, so a loop cannot quietly skip the one that is broken.
    const EVERY_MODE: [BlendMode; 17] = [
        BlendMode::Normal,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::PlusLighter,
        BlendMode::ColorDodge,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() < 1e-5,
                "channel {channel}: {actual:?} is not {expected:?}"
            );
        }
    }

    #[test]
    fn normal_returns_the_layer_untouched() {
        assert_eq!(
            blend_rgb([0.2, 0.3, 0.4], [0.9, 0.1, 0.0], BlendMode::Normal),
            [0.9, 0.1, 0.0]
        );
    }

    #[test]
    fn multiplying_by_white_changes_nothing() {
        let base = [0.25, 0.5, 0.75];
        assert_eq!(blend_rgb(base, [1.0; 3], BlendMode::Multiply), base);
        assert_eq!(blend_rgb(base, [0.0; 3], BlendMode::Multiply), [0.0; 3]);
    }

    #[test]
    fn screening_black_changes_nothing() {
        let base = [0.25, 0.5, 0.75];
        assert_eq!(blend_rgb(base, [0.0; 3], BlendMode::Screen), base);
        assert_eq!(blend_rgb(base, [1.0; 3], BlendMode::Screen), [1.0; 3]);
    }

    #[test]
    fn darken_and_lighten_pick_per_channel() {
        let base = [0.2, 0.8, 0.5];
        let layer = [0.6, 0.1, 0.5];
        assert_eq!(blend_rgb(base, layer, BlendMode::Darken), [0.2, 0.1, 0.5]);
        assert_eq!(blend_rgb(base, layer, BlendMode::Lighten), [0.6, 0.8, 0.5]);
    }

    #[test]
    fn difference_of_equal_colours_is_black() {
        assert_eq!(blend_rgb([0.4; 3], [0.4; 3], BlendMode::Difference), [0.0; 3]);
    }

    #[test]
    fn the_neutral_layer_of_each_light_mode_is_a_no_op() {
        let base = [0.25, 0.5, 0.75];
        // Mid grey is soft light's null, black is dodge's, white is burn's.
        assert_eq!(blend_rgb(base, [0.5; 3], BlendMode::SoftLight), base);
        assert_eq!(blend_rgb(base, [0.0; 3], BlendMode::ColorDodge), base);
        assert_eq!(blend_rgb(base, [1.0; 3], BlendMode::ColorBurn), base);
    }

    #[test]
    fn overlay_is_hard_light_with_the_roles_swapped() {
        let base = [0.3, 0.6, 0.2];
        let layer = [0.8, 0.2, 0.9];
        let overlay = blend_rgb(base, layer, BlendMode::Overlay);
        let hard = blend_rgb(base, layer, BlendMode::HardLight);

        assert_close(overlay, [0.48, 0.36, 0.36]);
        assert_close(hard, [0.72, 0.24, 0.84]);
        assert_ne!(overlay, hard, "swapping the roles has to change the answer");
        // ...and swapping the inputs instead is the same operation.
        assert_eq!(overlay, blend_rgb(layer, base, BlendMode::HardLight));
    }

    #[test]
    fn overlay_matches_the_shader_it_was_ported_from() {
        // `blend.wgsl`, case 8u, transcribed: the picture beneath chooses.
        fn shader_overlay(base: [f32; 3], layer: [f32; 3]) -> [f32; 3] {
            std::array::from_fn(|i| {
                if base[i] >= 0.5 {
                    1.0 - 2.0 * (1.0 - base[i]) * (1.0 - layer[i])
                } else {
                    2.0 * base[i] * layer[i]
                }
            })
        }

        for base_step in 0..=8 {
            for layer_step in 0..=8 {
                let base = [base_step as f32 / 8.0, 0.5, 1.0 - base_step as f32 / 8.0];
                let layer = [layer_step as f32 / 8.0, 1.0 - layer_step as f32 / 8.0, 0.5];
                assert_eq!(
                    blend_rgb(base, layer, BlendMode::Overlay),
                    shader_overlay(base, layer),
                    "base {base:?} layer {layer:?}"
                );
            }
        }
    }

    #[test]
    fn colour_keeps_the_brightness_beneath_and_luminosity_keeps_the_colour() {
        let base = [0.2, 0.4, 0.6];
        let layer = [0.9, 0.1, 0.3];

        let tinted = blend_rgb(base, layer, BlendMode::Color);
        assert!((lum(tinted) - lum(base)).abs() < 1e-5, "tinting must not shift brightness");

        let relit = blend_rgb(base, layer, BlendMode::Luminosity);
        assert!((lum(relit) - lum(layer)).abs() < 1e-5, "relighting must take the layer's");
    }

    #[test]
    fn a_grey_layer_has_no_hue_to_lend() {
        // set_sat of a flat colour is black, lifted back to the base's
        // brightness: a neutral grey.
        let base = [0.2, 0.4, 0.6];
        assert_close(blend_rgb(base, [0.5; 3], BlendMode::Hue), [lum(base); 3]);
    }

    #[test]
    fn every_mode_stays_inside_the_cube() {
        for mode in EVERY_MODE {
            for step in 0..=10 {
                let value = step as f32 / 10.0;
                for (base, layer) in [
                    ([value; 3], [0.0, 0.5, 1.0]),
                    ([0.0, 0.5, 1.0], [value; 3]),
                    ([value, 1.0 - value, 0.0], [1.0, value, 1.0 - value]),
                ] {
                    let result = blend_rgb(base, layer, mode);
                    for channel in result {
                        assert!(
                            (0.0..=1.0).contains(&channel),
                            "{mode:?} left {result:?} outside the cube"
                        );
                    }
                }
            }
        }
    }
}
