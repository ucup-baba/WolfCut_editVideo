//! A loopback HTTP server for the monitor's `<video>` and `<img>` sources.
//!
//! The webview cannot read media through the app's own URL scheme on every
//! platform. WebKitGTK keeps a protocol allowlist for its media player that
//! custom schemes are not on, so `<video src="asset://...">` fails with a
//! FormatError before a decoder is touched - and `file://` is unreachable
//! from the page's own origin. HTTP is on that allowlist everywhere, so the
//! smooth element preview needs an HTTP origin to exist at all.
//!
//! Written on `std::net` rather than a web framework, for the reason the
//! whisper downloader gives for using a blocking client: an async HTTP stack
//! would be a second runtime's worth of dependencies, here for a read-only
//! GET of files the user already imported.
//!
//! What keeps this from being a hole in the app:
//!
//! - It binds `127.0.0.1` on a port the OS picks. Nothing off the machine
//!   can reach it, and nothing can guess the port from one run to the next.
//! - Every URL carries a random per-run token. A local process that has not
//!   seen the token gets 404 for everything, including paths it knows.
//! - It serves *only* paths already admitted to the asset scope, matched
//!   whole against that set. There is no directory to walk out of and no
//!   prefix to escape: a path either is one the user imported or it is not.
//! - `GET` and `HEAD` only, and it never writes anything.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// How much of a file to move per write. Large enough that a 4K frame is a
/// couple of writes, small enough that a seek abandons little work.
const CHUNK: usize = 256 * 1024;

/// Refuse absurd request lines rather than growing a buffer for them.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// The running server: where to reach it, and what it is allowed to serve.
#[derive(Clone)]
pub struct MediaServer {
    origin: String,
    token: String,
    allowed: Arc<Mutex<HashSet<String>>>,
}

impl MediaServer {
    /// Binds a loopback port and starts serving. `None` if the port cannot be
    /// bound - the app still runs, the monitor just falls back to the engine's
    /// own frames, which is what it does on a machine with no working element
    /// preview anyway.
    pub fn start() -> Option<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .inspect_err(|error| eprintln!("wolfcut: media server: {error}"))
            .ok()?;
        let port = listener.local_addr().ok()?.port();

        let server = MediaServer {
            origin: format!("http://127.0.0.1:{port}"),
            token: token(),
            allowed: Arc::new(Mutex::new(HashSet::new())),
        };

        let worker = server.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let handler = worker.clone();
                // A thread per connection: the webview opens a handful, and
                // a media element holds one open for as long as it plays.
                std::thread::spawn(move || handler.serve(stream));
            }
        });

        Some(server)
    }

    /// Admits one file. Called from the same place the asset protocol scope
    /// grows, so the two can never disagree about what the webview may read.
    pub fn allow(&self, path: &str) {
        if let Ok(mut allowed) = self.allowed.lock() {
            allowed.insert(path.to_owned());
        }
    }

    /// The URL the webview should use for `path`, or `None` when the file was
    /// never admitted - a caller that gets `None` has a bug, not a permission
    /// problem, because admission happens at import and at project open.
    pub fn url_for(&self, path: &str) -> Option<String> {
        let allowed = self.allowed.lock().ok()?;
        allowed
            .contains(path)
            .then(|| format!("{}/{}/{}", self.origin, self.token, encode(path)))
    }

    fn serve(&self, stream: TcpStream) {
        let Ok(request) = read_request(&stream) else { return };
        let Some(path) = self.resolve(&request.target) else {
            let _ = respond_empty(&stream, "404 Not Found");
            return;
        };
        let _ = self.send(stream, Path::new(&path), request.range, request.head_only);
    }

    /// Turns a request target into a file this server may serve.
    ///
    /// Every failure is the same 404: a wrong token must not be told apart
    /// from a path that was never imported, or the reply becomes an oracle
    /// for what is on the disk.
    fn resolve(&self, target: &str) -> Option<String> {
        let rest = target.strip_prefix('/')?;
        let (token, encoded) = rest.split_once('/')?;
        if !constant_time_eq(token.as_bytes(), self.token.as_bytes()) {
            return None;
        }
        let path = decode(encoded)?;
        let allowed = self.allowed.lock().ok()?;
        allowed.contains(&path).then_some(path)
    }

    fn send(
        &self,
        mut stream: TcpStream,
        path: &Path,
        range: Option<(u64, Option<u64>)>,
        head_only: bool,
    ) -> std::io::Result<()> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mime = mime_of(path);

        // Ranges are not a nicety here: a media element seeks by asking for
        // one, and without them scrubbing re-downloads from the top.
        let (status, start, count) = match range {
            Some((start, _)) if start >= length => {
                let headers = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\n\
                     Content-Range: bytes */{length}\r\n\
                     Content-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                );
                return stream.write_all(headers.as_bytes());
            }
            Some((start, end)) => {
                let last = end.unwrap_or(length - 1).min(length - 1);
                ("206 Partial Content", start, last + 1 - start)
            }
            None => ("200 OK", 0, length),
        };

        let mut headers = format!(
            "HTTP/1.1 {status}\r\n\
             Content-Type: {mime}\r\n\
             Content-Length: {count}\r\n\
             Accept-Ranges: bytes\r\n\
             Cache-Control: no-store\r\n\
             Connection: close\r\n"
        );
        if status.starts_with("206") {
            let last = start + count - 1;
            headers.push_str(&format!("Content-Range: bytes {start}-{last}/{length}\r\n"));
        }
        headers.push_str("\r\n");
        stream.write_all(headers.as_bytes())?;
        if head_only {
            return Ok(());
        }

        file.seek(SeekFrom::Start(start))?;
        let mut left = count;
        let mut buffer = vec![0u8; CHUNK];
        while left > 0 {
            let want = CHUNK.min(usize::try_from(left).unwrap_or(CHUNK));
            let read = file.read(&mut buffer[..want])?;
            if read == 0 {
                break;
            }
            // A closed connection is ordinary: the element seeked, or the
            // clip left the playhead. Stop, do not complain.
            if stream.write_all(&buffer[..read]).is_err() {
                break;
            }
            left -= read as u64;
        }
        Ok(())
    }
}

/// One request, reduced to the three things this server acts on.
struct Request {
    target: String,
    range: Option<(u64, Option<u64>)>,
    head_only: bool,
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut read = reader.read_line(&mut line)?;
    let mut total = read;

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();

    let mut range = None;
    loop {
        line.clear();
        read = reader.read_line(&mut line)?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" || total > MAX_REQUEST_BYTES {
            break;
        }
        if let Some(value) = header(&line, "range") {
            range = parse_range(value);
        }
    }

    Ok(Request {
        target,
        range,
        head_only: method.eq_ignore_ascii_case("HEAD"),
    })
}

fn header<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
}

/// `bytes=start-` or `bytes=start-end`. Multi-range is not supported and not
/// needed: media elements ask for one span at a time.
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() { None } else { Some(end.parse().ok()?) };
    Some((start, end))
}

fn mime_of(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        // Deliberately not a guess: an unknown type the element cannot play
        // should fail as itself, not as a mislabelled mp4.
        _ => "application/octet-stream",
    }
}

/// Percent-encodes everything that is not unreserved, the separator included.
/// The whole path is one URL segment, so a slash in it is data, not structure.
fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn decode(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = encoded.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Compares without returning early, so a wrong token cannot be found one
/// character at a time by watching how long the reply takes.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(right).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// 128 bits of per-run secret, from the OS.
fn token() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut source) = File::open("/dev/urandom") {
        if source.read_exact(&mut bytes).is_ok() {
            return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        }
    }
    // No /dev/urandom (Windows): the address of a fresh allocation and the
    // clock are not a CSPRNG, but the token is a second lock behind a port
    // only this machine can reach, and the alternative is no server at all.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let here = Box::into_raw(Box::new(0u8)) as usize;
    format!("{now:032x}{here:016x}")
}

fn respond_empty(mut stream: &TcpStream, status: &str) -> std::io::Result<()> {
    stream.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_round_trips_through_the_encoding() {
        for path in [
            "/home/user/Videos/clip.mp4",
            "/home/user/WhatsApp Video 2026-08-01 at 19.38.21.mp4",
            "/home/user/percent%20already/#hash?query.mp4",
            "/home/user/ドキュメント/映像.mp4",
        ] {
            assert_eq!(decode(&encode(path)).as_deref(), Some(path));
        }
    }

    #[test]
    fn the_separator_is_encoded_so_a_path_stays_one_segment() {
        let encoded = encode("/a/b.mp4");
        assert!(!encoded.contains('/'), "{encoded} would split into segments");
    }

    #[test]
    fn a_truncated_escape_is_refused_rather_than_guessed() {
        assert_eq!(decode("%2"), None);
        assert_eq!(decode("%zz"), None);
    }

    #[test]
    fn ranges_parse_both_forms_and_reject_the_rest() {
        assert_eq!(parse_range("bytes=0-499"), Some((0, Some(499))));
        assert_eq!(parse_range("bytes=500-"), Some((500, None)));
        assert_eq!(parse_range(" bytes=1-2 "), Some((1, Some(2))));
        assert_eq!(parse_range("items=0-1"), None);
        assert_eq!(parse_range("bytes=abc"), None);
    }

    #[test]
    fn only_admitted_paths_resolve_and_only_with_the_token() {
        let server = MediaServer {
            origin: "http://127.0.0.1:1".to_owned(),
            token: "abc123".to_owned(),
            allowed: Arc::new(Mutex::new(HashSet::new())),
        };
        server.allow("/home/user/clip.mp4");
        let encoded = encode("/home/user/clip.mp4");

        assert_eq!(
            server.resolve(&format!("/abc123/{encoded}")).as_deref(),
            Some("/home/user/clip.mp4")
        );
        assert_eq!(server.resolve(&format!("/wrong/{encoded}")), None, "bad token");
        assert_eq!(
            server.resolve(&format!("/abc123/{}", encode("/etc/passwd"))),
            None,
            "never imported"
        );
        assert_eq!(
            server.resolve(&format!("/abc123/{}", encode("/home/user/../../etc/passwd"))),
            None,
            "no prefix to escape: the whole path is matched"
        );
    }

    #[test]
    fn a_url_is_only_minted_for_an_admitted_file() {
        let server = MediaServer {
            origin: "http://127.0.0.1:1".to_owned(),
            token: "t".to_owned(),
            allowed: Arc::new(Mutex::new(HashSet::new())),
        };
        assert_eq!(server.url_for("/home/user/clip.mp4"), None);
        server.allow("/home/user/clip.mp4");
        assert_eq!(
            server.url_for("/home/user/clip.mp4").as_deref(),
            Some("http://127.0.0.1:1/t/%2Fhome%2Fuser%2Fclip.mp4")
        );
    }

    #[test]
    fn tokens_differ_between_runs() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 32);
    }

    #[test]
    fn mime_types_come_from_the_extension_and_never_guess() {
        assert_eq!(mime_of(Path::new("/a/b.MP4")), "video/mp4");
        assert_eq!(mime_of(Path::new("/a/b.mov")), "video/quicktime");
        assert_eq!(mime_of(Path::new("/a/b.exe")), "application/octet-stream");
        assert_eq!(mime_of(Path::new("/a/b")), "application/octet-stream");
    }
}
