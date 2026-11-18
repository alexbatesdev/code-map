use crate::{evidence::Snapshot, relationships::SourceFile};
use std::{fs, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub enum EvidenceInput {
    #[default]
    Static,
    File(PathBuf),
    Rails,
}

pub struct EvidenceResult {
    pub snapshot: Option<Snapshot>,
    pub status: String,
}

pub fn load(root: &Path, sources: &[SourceFile], input: &EvidenceInput) -> EvidenceResult {
    let result = match input {
        EvidenceInput::Static => return EvidenceResult { snapshot: None, status: "Static analysis; optional app data is off".into() },
        EvidenceInput::File(path) => read(&if path.is_absolute() { path.clone() } else { root.join(path) }).and_then(|text| Snapshot::parse(&text, sources)),
        EvidenceInput::Rails => capture(root, Duration::from_secs(45)).and_then(|text| Snapshot::parse(&text, sources)),
    };
    match result {
        Ok(snapshot) => {
            let warnings = snapshot.warnings.len();
            let status = format!("Loaded evidence: {}{}", snapshot.label, if warnings == 0 { String::new() } else {
                format!(" · {warnings} warnings/ignored entries: {}", snapshot.warnings.iter().take(2).cloned().collect::<Vec<_>>().join("; "))
            });
            EvidenceResult { snapshot: Some(snapshot), status }
        }
        Err(error) => EvidenceResult { snapshot: None, status: format!("Optional evidence unavailable: {error}. Using static analysis") },
    }
}

fn read(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|_| "snapshot file could not be opened".to_owned())?;
    let mut bytes = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "snapshot file could not be read".to_owned())?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES { return Err("snapshot exceeds 64 MiB".into()); }
    String::from_utf8(bytes).map_err(|_| "snapshot must be UTF-8 JSON".into())
}

struct CaptureDir(PathBuf);
impl Drop for CaptureDir {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn capture(root: &Path, timeout: Duration) -> Result<String, String> {
    if !root.join("bin/rails").is_file() { return Err("this repository has no bin/rails".into()); }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = std::env::temp_dir().join(format!("spamton-megamap-evidence-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).map_err(|_| "could not create capture directory".to_owned())?;
    let dir = CaptureDir(path);
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).map_err(|_| "could not secure capture directory".to_owned())?;
    }
    let script = dir.0.join("runtime_evidence.rb");
    let output = dir.0.join("snapshot.json");
    fs::write(&script, include_str!("../tools/runtime_evidence.rb")).map_err(|_| "could not prepare capture".to_owned())?;
    let mut command = Command::new(root.join("bin/rails"));
    command.current_dir(root).args(["runner", script.to_str().ok_or("capture path must be UTF-8")?])
        .env("RAILS_ENV", "development").env("DISABLE_SPRING", "1")
        .env("MEGAMAP_EVIDENCE_MODE", "snapshot")
        .env("MEGAMAP_EVIDENCE_LABEL", "Rails development metadata")
        .env("MEGAMAP_EVIDENCE_OUT", &output)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| "Rails could not be started".to_owned())?;
    wait_capture(&mut child, timeout)?;
    read(&output)
}

fn wait_capture(child: &mut std::process::Child, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return if status.success() { Ok(()) } else { Err(format!("Rails capture exited with {status}")) },
            Err(_) => { stop(child); return Err("Rails capture status could not be read".into()); }
            _ => {}
        }
        if started.elapsed() >= timeout {
            stop(child);
            return Err("Rails capture timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn stop(child: &mut std::process::Child) {
    #[cfg(unix)] unsafe {
        extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
        kill(-(child.id() as i32), 9);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_optional_file_or_app_keeps_static_analysis_available() {
        let root = std::env::temp_dir().join(format!("spamton-missing-evidence-{}", std::process::id()));
        for input in [EvidenceInput::Static, EvidenceInput::File(root.join("missing.json")), EvidenceInput::Rails] {
            let result = load(&root, &[], &input);
            assert!(result.snapshot.is_none());
            assert!(result.status.to_lowercase().contains("static analysis"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn stalled_app_capture_is_bounded_and_leaves_no_child() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("/bin/sleep").arg("60").process_group(0).spawn().unwrap();
        let pid = child.id() as i32;
        let started = Instant::now();
        assert!(wait_capture(&mut child, Duration::from_millis(60)).unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(child.try_wait().unwrap().is_some());
        unsafe {
            extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
            assert_eq!(kill(pid, 0), -1, "capture child remained alive after timeout");
        }
    }
}
