//! Putting one finished frame through a filter chain.
//!
//! The exporter lays timeline effects on at the encoder, where FFmpeg sees the
//! whole stream and `t` means what it should. The monitor has no stream: it
//! composites one instant and asks what that looks like, so the frame goes out
//! to FFmpeg and comes straight back, filtered.
//!
//! # Why this starts a process every time
//!
//! Because it has to, and finding that out cost a freeze. A process is most
//! of the cost - 74 ms against 19 ms on a 960x540 frame through a blur - so
//! keeping one running is the obvious repair, and it does not work: FFmpeg
//! holds a frame back. Feed it k frames on a pipe that stays open and k-1
//! come out, every time, at every frame size, and no combination of
//! `-flush_packets`, `-fflags nobuffer`, `-avioflags direct`, `-probesize` or
//! `-muxdelay` changes it. A one-shot process does not notice because closing
//! stdin flushes the last frame; a long-lived one blocks forever on the first.
//!
//! The latency measures as exactly one frame, which is enough to work around
//! by pushing a pad frame after each real one. That is not a promise FFmpeg
//! makes, though - a different filter or version may hold two - and the way
//! it fails is a monitor that stops updating with nothing in any log. A
//! preview that costs 74 ms is worth more than one that is fast until it
//! silently is not.
//!
//! The weight still stays out of the graph, which is what [`mix`] is for: the
//! chain here is the plain effect a clip would use, and the ramp is applied
//! afterwards in Rust. That keeps this file honest about doing one thing.

use std::io::{Read, Write};
use std::process::Stdio;

use crate::error::{Error, Result};
use crate::process::base_command;

/// Runs `chain` over one RGBA frame and returns the result, same size.
///
/// The chain must keep the frame's dimensions - the caller reads back exactly
/// as many bytes as it wrote. Everything the catalogue builds keeps them.
pub fn filter_frame(pixels: &[u8], width: u32, height: u32, chain: &str) -> Result<Vec<u8>> {
    crate::audio::validate_chain(chain)?;
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return Err(Error::InvalidFilterChain {
            chain: chain.to_owned(),
            detail: format!("frame is {} bytes, expected {expected}", pixels.len()),
        });
    }

    let mut child = base_command(crate::binaries::ffmpeg())
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{width}x{height}")])
        .args(["-i", "pipe:0"])
        .args(["-vf", chain])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

    // Written on its own thread, and dropped there: closing stdin is what
    // flushes the frame out of FFmpeg, and a single thread doing both ends
    // would deadlock on a full pipe long before it got to the close.
    let mut stdin = child.stdin.take().expect("piped");
    let owned = pixels.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&owned));

    let mut out = Vec::with_capacity(expected);
    let read = child
        .stdout
        .take()
        .expect("piped")
        .read_to_end(&mut out)
        .map_err(|source| Error::Spawn { program: "ffmpeg", source });

    let _ = writer.join();
    let status = child.wait().map_err(|source| Error::Spawn { program: "ffmpeg", source })?;
    read?;

    if !status.success() || out.len() != expected {
        return Err(Error::InvalidFilterChain {
            chain: chain.to_owned(),
            detail: format!("ffmpeg returned {} bytes, expected {expected}", out.len()),
        });
    }
    Ok(out)
}

/// Mixes `filtered` back over `clean` at `weight`, in place on a copy.
///
/// The export says this to FFmpeg as `A*(1-w)+B*w`; here it is the same line
/// of arithmetic in Rust, which is what lets the weight change every frame
/// without the filter process knowing or caring.
pub fn mix(clean: &[u8], filtered: &[u8], weight: f32) -> Vec<u8> {
    let weight = weight.clamp(0.0, 1.0);
    if weight >= 1.0 {
        return filtered.to_vec();
    }
    if weight <= 0.0 {
        return clean.to_vec();
    }
    clean
        .iter()
        .zip(filtered)
        .map(|(&under, &over)| {
            (f32::from(under) * (1.0 - weight) + f32::from(over) * weight).round() as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_weight_is_the_filtered_frame_and_nothing_of_the_clean_one() {
        let clean = [10u8, 20, 30, 255];
        let filtered = [200u8, 210, 220, 255];
        assert_eq!(mix(&clean, &filtered, 1.0), filtered);
        assert_eq!(mix(&clean, &filtered, 2.0), filtered, "clamped, not extrapolated");
    }

    #[test]
    fn a_zero_weight_leaves_the_picture_alone() {
        let clean = [10u8, 20, 30, 255];
        let filtered = [200u8, 210, 220, 255];
        assert_eq!(mix(&clean, &filtered, 0.0), clean);
        assert_eq!(mix(&clean, &filtered, -1.0), clean);
    }

    #[test]
    fn half_way_lands_half_way() {
        assert_eq!(mix(&[0, 100, 200, 255], &[100, 200, 0, 255], 0.5), vec![50, 150, 100, 255]);
    }

    #[test]
    fn the_mix_matches_what_the_exports_blend_expression_says() {
        // `A*(1-w)+B*w`, evaluated by hand at a weight the ramp really uses.
        let weight = 0.25f32;
        let (clean, filtered) = (80u8, 240u8);
        let expected = (f32::from(clean) * 0.75 + f32::from(filtered) * 0.25).round() as u8;
        assert_eq!(mix(&[clean], &[filtered], weight), vec![expected]);
    }
}
