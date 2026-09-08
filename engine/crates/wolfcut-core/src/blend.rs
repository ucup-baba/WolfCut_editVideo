//! How a layer's colour combines with the pixels beneath it.
//!
//! The names and the arithmetic are the ones CSS calls `mix-blend-mode` and
//! PDF calls blend modes, which is also what every other editor means by these
//! words. Agreeing with them is the whole point: a cut graded in one tool
//! should look the same here.
//!
//! This is vocabulary only - a name for an intent. The arithmetic itself lives
//! in `wolfcut-render`, next to the compositor that applies it, because it is
//! about pixels and this crate is not.

/// How a layer's colour combines with the pixels beneath it.
///
/// Split, as the specifications split them, into modes that work on each
/// channel independently (everything up to [`Exclusion`](Self::Exclusion)) and
/// four that take a colour apart into hue, saturation and luminosity first.
/// The separable ones are cheap; the last four are not.
///
/// The order of these variants is the order the compositor's `switch` is
/// numbered, so a variant's position is part of its meaning. Append; never
/// insert.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub enum BlendMode {
    /// The layer simply covers what is beneath it, weighted by its alpha.
    /// The default, and what every clip does until someone says otherwise.
    #[default]
    Normal,
    /// Keeps whichever of the two is darker, per channel.
    Darken,
    /// Multiplies the two together. Never lightens; white leaves the picture
    /// beneath untouched. The workhorse for shadows and ink.
    Multiply,
    /// Darkens what is beneath in proportion to how dark the layer is.
    /// Harsher than [`Multiply`](Self::Multiply), and it crushes to black.
    ColorBurn,
    /// Keeps whichever of the two is lighter, per channel.
    Lighten,
    /// Multiplies the inverses. Never darkens; black leaves the picture
    /// beneath untouched. The way to drop a flare or a light leak on top.
    Screen,
    /// Plain addition, clipped at white. Brighter than
    /// [`Screen`](Self::Screen) and it clips rather than rolls off.
    PlusLighter,
    /// Brightens what is beneath in proportion to how light the layer is.
    /// The mirror of [`ColorBurn`](Self::ColorBurn), and it blows out to white.
    ColorDodge,
    /// Multiplies the dark parts and screens the light parts of *what is
    /// beneath*, so the picture underneath decides. Raises contrast while
    /// keeping highlights and shadows.
    Overlay,
    /// [`Overlay`](Self::Overlay)'s gentler cousin: the same idea with a
    /// curve that never clips, as if the layer were a diffuse light.
    SoftLight,
    /// [`Overlay`](Self::Overlay) with the roles swapped, so the *layer*
    /// decides. Harsh, and useful for exactly that reason.
    HardLight,
    /// The absolute difference between the two. Identical pictures give
    /// black, which makes this the mode you reach for to line two takes up.
    Difference,
    /// [`Difference`](Self::Difference) with a softer curve and less contrast.
    Exclusion,
    /// The layer's hue, wearing the saturation and brightness of what is
    /// beneath.
    Hue,
    /// The layer's saturation, wearing the hue and brightness of what is
    /// beneath.
    Saturation,
    /// The layer's hue and saturation over the brightness of what is beneath -
    /// the mode for tinting footage without flattening it.
    Color,
    /// The layer's brightness over the colour of what is beneath. The exact
    /// inverse of [`Color`](Self::Color).
    Luminosity,
}
