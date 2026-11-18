use crate::platform::{self, Event};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{atomic::{AtomicBool, Ordering}, mpsc::{self, Receiver, SyncSender}, Arc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const REQUEST_LIMIT: usize = 8192;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub enum Command {
    Help,
    Snapshot { query: Option<String>, all: bool },
    State,
    Stats,
    Input(Event),
    Click { x: f64, y: f64, button: u32, clicks: u32, modifiers: u32 },
    Grab { quit: bool },
    Quit,
    Noop,
}

pub struct Envelope {
    pub command: Command,
    pub reply: SyncSender<Value>,
}

pub struct Remote {
    pub address: SocketAddr,
    pub rx: Receiver<Envelope>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Remote {
    pub fn start(port: Option<u16>) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port.unwrap_or(0)))
            .map_err(|e| format!("Cannot start remote control: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::sync_channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new().name("megamap-remote".into()).spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &tx, &worker_stop),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(POLL),
                    Err(_) => break,
                }
            }
        }).map_err(|e| e.to_string())?;
        Ok(Self { address, rx, stop, worker: Some(worker) })
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[derive(Debug)]
struct Failure(u16, String);

fn bad(message: &str) -> Failure { Failure(400, message.into()) }

fn serve(mut stream: TcpStream, tx: &SyncSender<Envelope>, stop: &AtomicBool) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(POLL));
    let _ = stream.set_write_timeout(Some(POLL));
    let result = read_request(&mut stream).and_then(|request| parse_request(&request));
    let command = match result {
        Ok(command) => command,
        Err(error) => { respond(&mut stream, stop, error.0, &json!({"err": error.1})); return; }
    };
    if matches!(command, Command::Noop) {
        respond(&mut stream, stop, 200, &json!({"ok": true}));
        return;
    }
    let (reply, rx) = mpsc::sync_channel(1);
    if tx.try_send(Envelope { command, reply }).is_err() {
        respond(&mut stream, stop, 503, &json!({"err": "UI request queue unavailable"}));
        return;
    }
    let started = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            respond(&mut stream, stop, 503, &json!({"err": "Remote control stopped"}));
            return;
        }
        match rx.recv_timeout(POLL) {
            Ok(value) => { respond(&mut stream, stop, 200, &value); return; }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                respond(&mut stream, stop, 503, &json!({"err": "UI request cancelled"})); return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() >= REPLY_TIMEOUT => {
                respond(&mut stream, stop, 504, &json!({"err": "UI response timed out"})); return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn read_request(reader: &mut impl Read) -> Result<Vec<u8>, Failure> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    let started = Instant::now();
    loop {
        if started.elapsed() >= IO_TIMEOUT { return Err(Failure(408, "Request read timed out".into())); }
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
            Err(_) => return Err(bad("Request read failed")),
        };
        if count == 0 { return Err(bad("Incomplete HTTP request")); }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > REQUEST_LIMIT { return Err(Failure(413, "Request exceeds 8 KiB".into())); }
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") { return Ok(request); }
        if started.elapsed() >= IO_TIMEOUT { return Err(Failure(408, "Request read timed out".into())); }
    }
}

fn respond(stream: &mut TcpStream, stop: &AtomicBool, status: u16, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{\"err\":\"JSON serialization failed\"}".to_vec());
    let reason = match status {
        200 => "OK", 400 => "Bad Request", 404 => "Not Found", 405 => "Method Not Allowed",
        408 => "Request Timeout", 413 => "Content Too Large", 503 => "Service Unavailable",
        504 => "Gateway Timeout", _ => "Error",
    };
    let header = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", body.len());
    let started = Instant::now();
    for bytes in [header.as_bytes(), body.as_slice()] {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            if (stop.load(Ordering::Acquire) && started.elapsed() >= POLL) || started.elapsed() >= IO_TIMEOUT { return; }
            match stream.write(remaining) {
                Ok(0) => return,
                Ok(count) => remaining = &remaining[count..],
                Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {},
                Err(_) => return,
            }
        }
    }
}

fn parse_request(request: &[u8]) -> Result<Command, Failure> {
    if request.len() > REQUEST_LIMIT { return Err(Failure(413, "Request exceeds 8 KiB".into())); }
    let request = std::str::from_utf8(request).map_err(|_| bad("Request is not UTF-8"))?;
    let (headers, body) = request.split_once("\r\n\r\n").ok_or_else(|| bad("Incomplete HTTP request"))?;
    if !body.is_empty() { return Err(bad("Request body is unsupported")); }
    let mut lines = headers.split("\r\n");
    let parts: Vec<_> = lines.next().unwrap_or("").split(' ').collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") { return Err(bad("Invalid HTTP request line")); }
    if parts[0] != "GET" { return Err(Failure(405, "Only GET is supported".into())); }
    for header in lines {
        let (name, value) = header.split_once(':').ok_or_else(|| bad("Invalid HTTP header"))?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(bad("Invalid HTTP header"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0") {
            return Err(bad("Request body is unsupported"));
        }
    }
    let target = parts[1];
    if !target.starts_with('/') || target.starts_with("//") || target.contains('#') || target.chars().any(char::is_control) { return Err(bad("Invalid request target")); }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params = Params::new(query)?;
    let command = match path {
        "/" | "/help" => Command::Help,
        "/snap" => Command::Snapshot { query: params.take("q"), all: params.flag("all")? },
        "/s" => Command::State,
        "/stats" => Command::Stats,
        "/g" => Command::Grab { quit: false },
        "/gq" => Command::Grab { quit: true },
        "/quit" => Command::Quit,
        "/click" => pointer(&mut params, "click")?,
        "/m" => {
            let kind = params.take("k").unwrap_or_else(|| "move".into());
            pointer(&mut params, &kind)?
        }
        "/k" => keyboard(&mut params)?,
        "/t" => text(params.take("t").ok_or_else(|| bad("Missing text"))?),
        _ => return Err(Failure(404, "Unknown route".into())),
    };
    params.flag("wait")?;
    if !params.0.is_empty() { return Err(bad("Unknown query parameter")); }
    Ok(command)
}

struct Params(HashMap<String, String>);

impl Params {
    fn new(query: &str) -> Result<Self, Failure> {
        let mut values = HashMap::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode(key)?;
            let value = decode(value)?;
            if key.is_empty() || values.insert(key, value).is_some() { return Err(bad("Empty or duplicate query parameter")); }
        }
        Ok(Self(values))
    }
    fn take(&mut self, key: &str) -> Option<String> { self.0.remove(key) }
    fn flag(&mut self, key: &str) -> Result<bool, Failure> {
        match self.take(key).as_deref() { None | Some("0") => Ok(false), Some("1") => Ok(true), _ => Err(bad("Flag must be 0 or 1")) }
    }
    fn coord(&mut self, key: &str) -> Result<f64, Failure> {
        let value = self.take(key).unwrap_or_else(|| "0".into()).parse::<f64>().map_err(|_| bad("Invalid coordinate"))?;
        if !value.is_finite() || value.abs() > 1_000_000.0 { return Err(bad("Coordinate outside supported range")); }
        Ok(value)
    }
    fn integer(&mut self, key: &str, default: u32, max: u32) -> Result<u32, Failure> {
        let value = match self.take(key) { Some(value) => value.parse::<u32>().map_err(|_| bad("Invalid integer"))?, None => default };
        if value > max { return Err(bad("Integer outside supported range")); }
        Ok(value)
    }
    fn modifiers(&mut self) -> Result<u32, Failure> {
        let mut value = 0;
        for (key, bit) in [("shift", platform::SHIFT), ("ctrl", platform::CONTROL), ("alt", platform::ALT), ("cmd", platform::COMMAND)] {
            if self.flag(key)? { value |= bit; }
        }
        Ok(value)
    }
}

fn decode(value: &str) -> Result<String, Failure> {
    let mut output = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => output.push(b' '),
            b'%' => {
                let high = bytes.next().and_then(|b| (b as char).to_digit(16)).ok_or_else(|| bad("Invalid percent encoding"))?;
                let low = bytes.next().and_then(|b| (b as char).to_digit(16)).ok_or_else(|| bad("Invalid percent encoding"))?;
                output.push((high * 16 + low) as u8);
            }
            byte => output.push(byte),
        }
    }
    String::from_utf8(output).map_err(|_| bad("Query is not UTF-8"))
}

fn text(text: String) -> Command { Command::Input(Event::Text { text, replace_start: usize::MAX, replace_len: 0 }) }

fn pointer(params: &mut Params, kind: &str) -> Result<Command, Failure> {
    let x = params.coord("x")?;
    let y = params.coord("y")?;
    let modifiers = params.modifiers()?;
    let button = params.integer("b", 0, 2)?;
    let clicks = params.integer("clicks", 1, 3)?;
    if clicks == 0 { return Err(bad("Click count must be positive")); }
    Ok(match kind {
        "move" => Command::Input(Event::PointerMove { x, y, modifiers }),
        "down" => Command::Input(Event::PointerDown { x, y, modifiers, button, clicks }),
        "up" => Command::Input(Event::PointerUp { x, y, modifiers, button, clicks }),
        "click" => Command::Click { x, y, modifiers, button, clicks },
        "scroll" => Command::Input(Event::Scroll { x, y, dx: params.coord("dx")?, dy: -params.coord("dy")?, modifiers }),
        _ => return Err(bad("Unknown pointer action")),
    })
}

fn keyboard(params: &mut Params) -> Result<Command, Failure> {
    let kind = params.take("k").unwrap_or_else(|| "press".into());
    if !matches!(kind.as_str(), "press" | "down" | "up") { return Err(bad("Unknown keyboard action")); }
    let modifiers = params.modifiers()?;
    let character = params.take("c");
    let committed = params.take("t");
    if character.is_some() && committed.is_some() { return Err(bad("Choose key or text")); }
    let command = if let Some(value) = committed { text(value) } else {
        let character = character.ok_or_else(|| bad("Missing key"))?;
        let code = match character.as_str() {
            "ReturnKey" | "Return" | "Enter" => platform::KEY_RETURN,
            "Escape" => platform::KEY_ESCAPE,
            "Backspace" => platform::KEY_BACKSPACE,
            "Delete" => platform::KEY_DELETE,
            "Tab" => platform::KEY_TAB,
            "ArrowLeft" | "LeftArrow" => platform::KEY_LEFT,
            "ArrowRight" | "RightArrow" => platform::KEY_RIGHT,
            "ArrowUp" | "UpArrow" => platform::KEY_UP,
            "ArrowDown" | "DownArrow" => platform::KEY_DOWN,
            "Home" => platform::KEY_HOME,
            "End" => platform::KEY_END,
            "Space" => b' ' as u32,
            value if value.len() == 4 && value.starts_with("Key") && value.as_bytes()[3].is_ascii_uppercase() => value.as_bytes()[3].to_ascii_lowercase() as u32,
            value if value.len() == 6 && value.starts_with("Digit") && value.as_bytes()[5].is_ascii_digit() => value.as_bytes()[5] as u32,
            _ => return Err(bad("Unsupported key")),
        };
        let code = if modifiers & platform::COMMAND != 0 {
            match code { 97 => platform::KEY_SELECT_ALL, 99 => platform::KEY_COPY, 120 => platform::KEY_CUT, 118 => platform::KEY_PASTE, 122 => platform::KEY_UNDO, value => value }
        } else { code };
        if modifiers & (platform::COMMAND | platform::CONTROL | platform::ALT) == 0 && (32..=126).contains(&code) {
            let mut value = char::from_u32(code).unwrap();
            if modifiers & platform::SHIFT != 0 { value = value.to_ascii_uppercase(); }
            text(value.to_string())
        } else { Command::Input(Event::Key { code, modifiers }) }
    };
    Ok(if kind == "up" { Command::Noop } else { command })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route(target: &str) -> Result<Command, Failure> { parse_request(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()) }

    #[test]
    fn unicode_text_and_snapshot_query_survive_decoding() {
        match route("/t?t=Hello+%F0%9F%8C%8D%20%22%5C").unwrap() {
            Command::Input(Event::Text { text, replace_start, replace_len }) => {
                assert_eq!(text, "Hello 🌍 \"\\"); assert_eq!(replace_start, usize::MAX); assert_eq!(replace_len, 0);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(route("/snap?q=%E6%97%A5%E6%9C%AC&all=1").unwrap(), Command::Snapshot { query: Some(query), all: true } if query == "日本"));
    }

    #[test]
    fn click_and_scroll_preserve_input_conventions() {
        assert!(matches!(route("/click?x=3.5&y=-7&b=1&clicks=2&cmd=1&shift=1&ctrl=1&alt=1&wait=1").unwrap(), Command::Click { x:3.5, y:-7.0, button:1, clicks:2, modifiers:15 }));
        assert!(matches!(route("/m?k=scroll&x=4&dx=2&dy=3").unwrap(), Command::Input(Event::Scroll { x:4.0, y:0.0, dx:2.0, dy:-3.0, modifiers:0 })));
        assert!(matches!(route("/m?k=down&b=2").unwrap(), Command::Input(Event::PointerDown { button:2, .. })));
        assert!(matches!(route("/m?k=up").unwrap(), Command::Input(Event::PointerUp { .. })));
        assert!(matches!(route("/m").unwrap(), Command::Input(Event::PointerMove { .. })));
    }

    #[test]
    fn key_commands_and_key_up_match_editor_events() {
        for (key, code) in [("KeyA", platform::KEY_SELECT_ALL), ("KeyC", platform::KEY_COPY), ("KeyX", platform::KEY_CUT), ("KeyV", platform::KEY_PASTE), ("KeyZ", platform::KEY_UNDO)] {
            assert!(matches!(route(&format!("/k?c={key}&cmd=1")).unwrap(), Command::Input(Event::Key { code: found, modifiers: platform::COMMAND }) if found == code));
        }
        assert!(matches!(route("/k?k=up&c=KeyA&cmd=1").unwrap(), Command::Noop));
        assert!(matches!(route("/k?k=down&c=ReturnKey").unwrap(), Command::Input(Event::Key { code:platform::KEY_RETURN, modifiers:0 })));
        assert!(matches!(route("/k?c=KeyA&shift=1").unwrap(), Command::Input(Event::Text { text, .. }) if text == "A"));
        assert!(matches!(route("/k?t=%E2%9C%93").unwrap(), Command::Input(Event::Text { text, .. }) if text == "✓"));
    }

    #[test]
    fn malformed_queries_and_numeric_bounds_are_rejected() {
        for target in ["/click?x=NaN", "/click?x=inf", "/click?y=-1000001", "/click?b=3", "/click?clicks=0", "/click?clicks=-1", "/click?cmd=true", "/click?x=1&x=2", "/click?x=1&%78=2", "/t?t=%", "/t?t=%0x", "/t?t=%FF", "/t?t=a&t=b", "/g?path=/tmp/file", "/k?c=KeyA&t=a", "/m?k=foo", "/k?c=Unknown", "/t"] {
            assert!(route(target).is_err(), "Accepted {target}");
        }
        assert!(route("/click?x=1000000&y=-1000000").is_ok());
    }

    #[test]
    fn http_bounds_and_routes_are_enforced() {
        assert!(matches!(route("/").unwrap(), Command::Help));
        assert!(matches!(route("/help").unwrap(), Command::Help));
        assert!(matches!(route("/s").unwrap(), Command::State));
        assert!(matches!(route("/stats").unwrap(), Command::Stats));
        assert!(matches!(route("/g").unwrap(), Command::Grab { quit: false }));
        assert!(matches!(route("/gq").unwrap(), Command::Grab { quit: true }));
        assert!(matches!(route("/quit").unwrap(), Command::Quit));
        assert_eq!(route("/unknown").unwrap_err().0, 404);
        assert_eq!(parse_request(b"POST /s HTTP/1.1\r\n\r\n").unwrap_err().0, 405);
        for request in ["GET /s HTTP/2\r\n\r\n", "GET http://localhost/s HTTP/1.1\r\n\r\n", "GET /s HTTP/1.1\r\nBad\r\n\r\n", "GET /s HTTP/1.1\r\nContent-Length: 1\r\n\r\n", "GET /s HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", "GET /s HTTP/1.1\r\n\r\nx"] { assert!(parse_request(request.as_bytes()).is_err()); }
        assert_eq!(route(&format!("/t?t={}", "x".repeat(REQUEST_LIMIT))).unwrap_err().0, 413);
        assert_eq!(read_request(&mut std::io::Cursor::new(vec![b'x'; REQUEST_LIMIT+1])).unwrap_err().0, 413);
        assert!(read_request(&mut std::io::Cursor::new(b"GET /s HTTP/1.1\r\n\r\n")).is_ok());
        assert!(read_request(&mut std::io::Cursor::new(b"GET /s")).is_err());
    }

    #[test]
    #[ignore = "Requires permission to bind a loopback TCP listener"]
    fn loopback_http_replies_are_json_and_drop_cancels_pending_work() {
        let remote = Remote::start(None).unwrap();
        assert_eq!(remote.address.ip(), Ipv4Addr::LOCALHOST);
        let address = remote.address;
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(b"GET /snap?q=%E6%97%A5%E6%9C%AC HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let envelope = remote.rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(envelope.command, Command::Snapshot { query: Some(ref q), all: false } if q == "日本"));
        envelope.reply.send(json!({"text": "日本\"\\\n"})).unwrap();
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.contains(&format!("Content-Length: {}", body.len())));
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), json!({"text": "日本\"\\\n"}));

        let reader = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(b"GET /snap HTTP/1.1\r\n\r\n").unwrap();
            let mut response = Vec::new();
            let mut chunk = [0; 16384];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 { break; }
                response.extend_from_slice(&chunk[..count]);
                thread::sleep(Duration::from_millis(15));
            }
            response
        });
        let envelope = remote.rx.recv_timeout(Duration::from_secs(2)).unwrap();
        envelope.reply.send(json!({"s": "x".repeat(512 * 1024)})).unwrap();
        let bytes = reader.join().unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.contains(&format!("Content-Length: {}", body.len())));
        assert_eq!(serde_json::from_str::<Value>(body).unwrap()["s"].as_str().unwrap().len(), 512 * 1024);

        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            stream.write_all(b"GET /s HTTP/1.1\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let pending = remote.rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        drop(remote);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(client.join().unwrap().starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        drop(pending);
        assert!(TcpStream::connect(address).is_err());
    }
}
