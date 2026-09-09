//! Putting finished frames through a filter chain, one process for many.
//!
//! The exporter lays timeline effects on at the encoder, where FFmpeg sees the
//! whole stream and `t` means what it should. The monitor has no stream: it
//! composites one instant and asks what that looks like.
//!
//! The obvious way to answer - hand FFmpeg the frame, take it back filtered -
//! costs a process each time, and a process is most of the cost. Measured on
//! a 960x540 frame through a blur: 74 ms the obvious way, 19 ms when the
//! process is already running. Starting it is three quarters of the work.
//!
//! So the process stays. What made that hard is that a ramped effect wants a
//! different weight every frame, and a graph is fixed when the process
//! starts - so the weight comes out of the graph entirely. FFmpeg runs the
//! plain effect, the same chain a clip would use, and the ramp is applied
//! afterwards by mixing the filtered frame back over the clean one in Rust,
//! where a weight is just a number. Same arithmetic as the export's `blend`,
//! and nothing has to be respawned to change it.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::mpsc::{SyncSender, sync_channel};

use crate::error::{Error, Result};
use crate::process::base_command;

/// One running FFmpeg with a fixed chain, fed frames and read back.
pub struct FrameFilter {
    child: Child,
    stdout: ChildStdout,
    /// Frames go to a writer thread, because filling the child's stdin and
    /// draining its stdout from one thread deadlocks the moment either pipe
    /// fills - and a frame is far bigger than a pipe.
    to_child: SyncSender<Vec<u8>>,
    frame_bytes: usize,
}

impl FrameFilter {
    /// Starts FFmpeg for `chain` at one frame size.
    ///
    /// The chain must keep the frame's dimensions: every read expects exactly
    /// as many bytes as the write that prompted it. Everything the catalogue
    /// builds keeps them.
    pub fn open(width: u32, height: u32, chain: &str) -> Result<Self> {
        crate::audio::validate_chain(chain)?;
        let mut child = base_command(crate::binaries::ffmpeg())
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-i", "pipe:0"])
            .args(["-vf", chain])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            // Without this FFmpeg may hold a frame back waiting for company,
            // and the reader would block on a frame that is already made.
            .args(["-flush_packets", "1"])
            .arg("pipe:1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

        let mut stdin = child.stdin.take().expect("piped");
        // One frame in flight: a queue here would only let the caller run
        // ahead of a filter it is about to wait for anyway.
        let (to_child, from_caller) = sync_channel::<Vec<u8>>(1);
        std::thread::spawn(move || {
            for frame in from_caller {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            stdout: child.stdout.take().expect("piped"),
            child,
            to_child,
            frame_bytes: width as usize * height as usize * 4,
        })
    }

    /// Filters one frame. The result is the same size as the input.
    pub fn apply(&mut self, pixels: &[u8]) -> Result<Vec<u8>> {
        if pixels.len() != self.frame_bytes {
            return Err(Error::InvalidFilterChain {
                chain: String::new(),
                detail: format!("frame is {} bytes, expected {}", pixels.len(), self.frame_bytes),
            });
        }
        self.to_child.send(pixels.to_vec()).map_err(|_| Error::InvalidFilterChain {
            chain: String::new(),
            detail: "the filter process has gone".to_owned(),
        })?;

        let mut out = vec![0u8; self.frame_bytes];
        self.stdout.read_exact(&mut out).map_err(|source| Error::Spawn {
            program: "ffmpeg",
            source,
        })?;
        Ok(out)
    }
}

impl Drop for FrameFilter {
    fn drop(&mut self) {
        // Closing stdin ends the writer thread and lets FFmpeg finish; the
        // kill is for a child that ignores that.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Running filters, kept by what they do and how big they do it.
///
/// A monitor scrubbing back and forth over the same effect asks for the same
/// chain again and again; keeping the process is the whole point.
#[derive(Default)]
pub struct FilterPool {
    running: HashMap<(String, u32, u32), FrameFilter>,
}

impl FilterPool {
    /// An empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filters one frame through `chain`, starting a process for it the first
    /// time and reusing it after.
    ///
    /// A filter that has died is dropped and started again rather than
    /// poisoning every later frame - the monitor should recover from one bad
    /// frame, not stop showing pictures.
    pub fn apply(
        &mut self,
        chain: &str,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<Vec<u8>> {
        let key = (chain.to_owned(), width, height);
        if !self.running.contains_key(&key) {
            self.running.insert(key.clone(), FrameFilter::open(width, height, chain)?);
        }
        match self.running.get_mut(&key).expect("just inserted").apply(pixels) {
            Ok(filtered) => Ok(filtered),
            Err(error) => {
                self.running.remove(&key);
                Err(error)
            }
        }
    }

    /// Drops every filter whose chain is not in `keep`.
    ///
    /// Called with what the edit still uses, so retuning an effect does not
    /// leave the old chain's process running for the rest of the session.
    pub fn retain(&mut self, keep: &[String]) {
        self.running.retain(|(chain, _, _), _| keep.iter().any(|wanted| wanted == chain));
    }
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
