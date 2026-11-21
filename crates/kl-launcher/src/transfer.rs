//! A one-page file receiver, tied to the lifetime of the launcher's File transfer page.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

const MAX_HEADER: usize = 8192;
const MAX_FILE: u64 = 64 * 1024 * 1024;
static UPLOAD_ID: AtomicUsize = AtomicUsize::new(0);
const PAGE: &str = include_str!("transfer.html");

pub struct Session {
    pub address: String,
    pub code: String,
    stop: Arc<AtomicBool>,
}

impl Session {
    pub fn start(ip: Ipv4Addr, documents: PathBuf) -> io::Result<Self> {
        let listener = TcpListener::bind((ip, 0))?;
        listener.set_nonblocking(true)?;
        let address = format!("http://{}", listener.local_addr()?);
        let mut bytes = [0u8; 4];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let code = format!("{:06}", u32::from_ne_bytes(bytes) % 1_000_000);
        let stop = Arc::new(AtomicBool::new(false));
        let session_stop = stop.clone();
        let key = code.clone();
        thread::Builder::new()
            .name("file-transfer".into())
            .spawn(move || {
                while !session_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut connection, _)) => {
                            let _ = handle(&mut connection, &documents, &key, &session_stop);
                            drain(&mut connection);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            address,
            code,
            stop,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len())
}

/// After the answer, read and discard what the client still sends (up to two seconds), then close. A close with
/// unread request bytes sends a reset, and the client can lose the answer before it reads it (a 403 for a wrong
/// code, for one): the reset came first on Linux and failed the transfer test there.
fn drain(stream: &mut TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut sink = [0u8; 4096];
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn handle(stream: &mut TcpStream, root: &Path, code: &str, stop: &AtomicBool) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream);
    let header_deadline = Instant::now() + Duration::from_secs(8);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= MAX_HEADER
            || stop.load(Ordering::Acquire)
            || Instant::now() >= header_deadline
        {
            return Ok(());
        }
        let remaining = MAX_HEADER - header.len();
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let mut consumed = 0;
        for &byte in available.iter().take(remaining) {
            consumed += 1;
            header.push(byte);
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        reader.consume(consumed);
    }
    header.truncate(header.len() - 4);
    let Ok(head) = std::str::from_utf8(&header) else {
        return Ok(());
    };
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default();
    if request == "GET / HTTP/1.1" || request == "GET / HTTP/1.0" {
        return respond(reader.get_mut(), "200 OK", "text/html", PAGE);
    }
    if request != "POST /upload HTTP/1.1" && request != "POST /upload HTTP/1.0" {
        return respond(
            reader.get_mut(),
            "404 Not Found",
            "text/plain",
            "Page not found.",
        );
    }

    let mut named = None;
    let mut length = None;
    let mut supplied = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(());
        };
        let entry = match name.to_ascii_lowercase().as_str() {
            "x-filename" => &mut named,
            "content-length" => &mut length,
            "x-transfer-code" => &mut supplied,
            "transfer-encoding" => {
                return respond(
                    reader.get_mut(),
                    "400 Bad Request",
                    "text/plain",
                    "Use a fixed-size file.",
                )
            }
            _ => continue,
        };
        if entry.replace(value.trim()).is_some() {
            return respond(
                reader.get_mut(),
                "400 Bad Request",
                "text/plain",
                "Duplicate file header.",
            );
        }
    }
    if supplied != Some(code) {
        thread::sleep(Duration::from_millis(250));
        return respond(
            reader.get_mut(),
            "403 Forbidden",
            "text/plain",
            "Check the code shown on your Kindle.",
        );
    }
    let Some(filename) = named.and_then(safe_name) else {
        return respond(
            reader.get_mut(),
            "400 Bad Request",
            "text/plain",
            "Choose a document or another non-executable file.",
        );
    };
    let Some(size) = length.and_then(|size| size.parse::<u64>().ok()) else {
        return respond(
            reader.get_mut(),
            "411 Length Required",
            "text/plain",
            "Could not read the file size.",
        );
    };
    if size == 0 || size > MAX_FILE {
        return respond(
            reader.get_mut(),
            "413 Content Too Large",
            "text/plain",
            "Choose a file between 1 byte and 64 MB.",
        );
    }
    let result = write_upload(&mut reader, root, &filename, size, stop);
    match result {
        Ok(()) => respond(
            reader.get_mut(),
            "201 Created",
            "text/plain",
            "File received.",
        ),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => respond(
            reader.get_mut(),
            "409 Conflict",
            "text/plain",
            "A file with this name already exists.",
        ),
        Err(_) => respond(
            reader.get_mut(),
            "500 Internal Server Error",
            "text/plain",
            "The file could not be saved. Check Kindle storage.",
        ),
    }
}

fn safe_name(encoded: &str) -> Option<String> {
    let mut bytes = encoded.bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let a = (bytes.next()? as char).to_digit(16)?;
            let b = (bytes.next()? as char).to_digit(16)?;
            ((a << 4) | b) as u8
        } else {
            byte
        });
    }
    let name = String::from_utf8(decoded).ok()?;
    let disallowed = name.starts_with('.')
        || name.trim() != name
        || name.ends_with('.')
        || name.len() > 200
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c));
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if disallowed
        || matches!(
            ext.as_str(),
            "sh" | "py" | "pl" | "lua" | "js" | "desktop" | "bin" | "exe"
        )
    {
        return None;
    }
    Some(name)
}

fn write_upload(
    source: &mut impl Read,
    root: &Path,
    filename: &str,
    size: u64,
    stop: &AtomicBool,
) -> io::Result<()> {
    let destination = root.join(filename);
    if destination.try_exists()? {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let staged = root.join(format!(
        ".primitive-upload-{}-{}.part",
        std::process::id(),
        UPLOAD_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut remaining = size;
        let mut buffer = [0; 16 * 1024];
        while remaining != 0 {
            if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "file transfer closed",
                ));
            }
            let available = remaining.min(buffer.len() as u64) as usize;
            let n = source.read(&mut buffer[..available])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete upload",
                ));
            }
            file.write_all(&buffer[..n])?;
            remaining -= n as u64;
        }
        if stop.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file transfer closed",
            ));
        }
        file.sync_all()
    })();
    drop(file);
    let result = result.and_then(|()| publish(&staged, &destination));
    let _ = fs::remove_file(staged);
    result
}

fn publish(staged: &Path, destination: &Path) -> io::Result<()> {
    match fs::hard_link(staged, destination) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(_) => {}
    }
    // The Kindle's FAT-backed storage can refuse hard links. Reserve the name
    // exclusively before replacing our reservation with the complete file.
    let reservation = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    drop(reservation);
    if let Err(error) = fs::rename(staged, destination) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_normal_filenames_and_rejects_paths_and_scriptlets() {
        assert_eq!(
            safe_name("Reading%20notes%20%C3%A9.txt").as_deref(),
            Some("Reading notes é.txt")
        );
        for name in [
            "../book.epub",
            "%2e%2e%2fbook.epub",
            ".hidden.pdf",
            "%00.epub",
            "app.SH",
            "folder%5cbook.epub",
            "%FF.txt",
        ] {
            assert_eq!(safe_name(name), None);
        }
    }

    #[test]
    fn only_accepts_the_session_code_and_keeps_existing_documents() {
        let temp =
            std::env::temp_dir().join(format!("kl-file-transfer-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let session = Session::start(Ipv4Addr::LOCALHOST, temp.clone()).unwrap();
        let endpoint = session.address.strip_prefix("http://").unwrap();
        let send = |code, filename, size, data| {
            let mut stream = TcpStream::connect(endpoint).unwrap();
            write!(stream, "POST /upload HTTP/1.1\r\nX-Transfer-Code: {code}\r\nX-Filename: {filename}\r\nContent-Length: {size}\r\n\r\n{data}").unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        // A wrong code writes nothing, so it can be asked again if the connection ended before an answer arrived (the
        // race that made this test fail now and then on a loaded machine). The answer must still be the 403.
        let mut refused = String::new();
        for _ in 0..3 {
            refused = send("wrong", "other.epub", 3, "bad");
            if !refused.is_empty() {
                break;
            }
        }
        assert!(refused.starts_with("HTTP/1.1 403"), "a wrong code is refused: {refused:?}");
        assert!(send(&session.code, "book.epub", 4, "text").starts_with("HTTP/1.1 201"));
        assert!(send(&session.code, "book.epub", 3, "new").starts_with("HTTP/1.1 409"));
        assert!(send(&session.code, "partial.epub", 4, "bad").starts_with("HTTP/1.1 500"));
        assert_eq!(fs::read(temp.join("book.epub")).unwrap(), b"text");
        assert!(!temp.join("partial.epub").exists());
        drop(session);
        fs::remove_dir_all(temp).unwrap();
    }
}
