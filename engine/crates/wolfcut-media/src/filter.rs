//! Putting one finished frame through a filtergraph.
//!
//! The exporter lays timeline effects on at the encoder, where FFmpeg sees
//! every frame in order and `t` means what it should. The paused monitor has
//! no such stream: it composites one instant and asks what it looks like. So
//! the frame goes out to FFmpeg and comes straight back, filtered.
//!
//! A process per frame is not a thing to do while playing, and this is not
//! for that - it is for the frame the monitor rests on. That frame is already
//! fetched a beat after the playhead settles, and one FFmpeg round trip fits
//! inside the same beat.

use std::io::{Read, Write};
use std::process::Stdio;

use crate::error::{Error, Result};
use crate::process::base_command;

/// Runs `graph` over one RGBA frame and returns the result, same size.
///
/// The graph must keep the frame's dimensions - the caller reads back exactly
/// as many bytes as it wrote, and a graph that scales would leave the reader
/// short or long. Everything this crate builds keeps them.
pub fn filter_frame(pixels: &[u8], width: u32, height: u32, graph: &str) -> Result<Vec<u8>> {
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return Err(Error::InvalidFilterChain {
            chain: graph.to_owned(),
            detail: format!("frame is {} bytes, expected {expected}", pixels.len()),
        });
    }

    let mut child = base_command(crate::binaries::ffmpeg())
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{width}x{height}")])
        .args(["-i", "pipe:0"])
        .args(["-vf", graph])
        .args(["-frames:v", "1"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Spawn { program: "ffmpeg", source })?;

    // Written on its own thread: FFmpeg starts producing before it has taken
    // the whole input, and a single thread doing both would deadlock on a
    // full pipe with the other end waiting to be read.
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
            chain: graph.to_owned(),
            detail: format!("ffmpeg returned {} bytes, expected {expected}", out.len()),
        });
    }
    Ok(out)
}
