use crate::relationships::{SourceFile, SymbolRef};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EvidenceKind {
    #[default]
    Source,
    Inferred,
    Loaded,
    Observed,
}

impl EvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Source-resolved",
            Self::Inferred => "Inferred",
            Self::Loaded => "Loaded app",
            Self::Observed => "Observed execution",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidenceFilter {
    pub source: bool,
    pub inferred: bool,
    pub loaded: bool,
    pub observed: bool,
}

impl Default for EvidenceFilter {
    fn default() -> Self { Self { source: true, inferred: true, loaded: true, observed: true } }
}

impl EvidenceFilter {
    pub fn for_mode(mode: usize) -> Self {
        Self { source: mode <= 2, inferred: mode == 0 || mode == 2, loaded: mode == 0 || mode == 3, observed: mode == 0 || mode == 4 }
    }

    pub fn allows(self, kind: EvidenceKind) -> bool {
        match kind { EvidenceKind::Source => self.source, EvidenceKind::Inferred => self.inferred, EvidenceKind::Loaded => self.loaded, EvidenceKind::Observed => self.observed }
    }
}

#[derive(Clone, Debug)]
pub struct LoadedMethod {
    pub name: String,
    pub singleton: bool,
    pub definition: SymbolRef,
}

#[derive(Clone, Debug)]
pub struct LoadedAssociation {
    pub name: String,
    pub target: String,
    pub collection: bool,
}

#[derive(Clone, Debug)]
pub struct LoadedClass {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub active_record: bool,
    pub attributes: Vec<String>,
    pub associations: Vec<LoadedAssociation>,
    pub methods: Vec<LoadedMethod>,
}

#[derive(Clone, Debug)]
pub struct ObservedCall {
    pub caller: SymbolRef,
    pub callee: SymbolRef,
    pub line: usize,
    pub method: String,
    pub count: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub label: String,
    pub warnings: Vec<String>,
    pub classes: Vec<LoadedClass>,
    pub calls: Vec<ObservedCall>,
}

/// A content freshness check, not an authentication or cryptographic hash.
pub fn fingerprint(text: &str) -> String {
    let value = text.as_bytes().iter().fold(14695981039346656037u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(1099511628211));
    format!("{value:016x}")
}

impl Snapshot {
    pub fn parse(text: &str, sources: &[SourceFile]) -> Result<Self, String> {
        if text.len() > 64 * 1024 * 1024 { return Err("Evidence exceeds the 64 MiB limit".into()); }
        let value: Value = serde_json::from_str(text).map_err(|e| format!("Invalid evidence JSON: {e}"))?;
        if value.get("version").and_then(Value::as_u64) != Some(1) { return Err("Unsupported evidence version (expected 1)".into()); }
        let files = value.get("files").and_then(Value::as_object).ok_or("Evidence needs a files fingerprint map")?;
        if files.len() > 200_000 { return Err("Too many evidence files".into()); }
        let mut source_map: HashMap<&str, Option<&str>> = HashMap::new();
        for source in sources {
            source_map.entry(&source.path).and_modify(|old| *old = None).or_insert(Some(&source.text));
        }
        let label: String = value.get("label").and_then(Value::as_str).unwrap_or("Imported capture").chars().take(256).map(|c| if c.is_control() { ' ' } else { c }).collect();
        let mut result = Self { label, ..Self::default() };
        if let Some(warnings) = value.get("warnings") {
            if let Some(warnings) = warnings.as_array() {
                for warning in warnings.iter().take(16) {
                    if let Some(text) = warning.as_str() {
                        let text: String = text.chars().take(256).map(|c| if c.is_control() { ' ' } else { c }).collect();
                        result.warn(format!("Capture warning: {text}"));
                    }
                }
                if warnings.len() > 16 { result.warn("Capture warning: further warnings omitted".into()); }
            } else { result.warn("Invalid capture warnings were ignored".into()); }
        }
        let mut validated = HashMap::new();
        for (path, expected) in files {
            let current = source_map.get(path.as_str()).and_then(|value| *value);
            if relative_path(path) && current.is_some_and(|text| expected.as_str() == Some(fingerprint(text).as_str())) {
                validated.insert(path.as_str(), current.unwrap().lines().count().max(1));
            } else { result.warn(format!("Missing, stale or invalid evidence file: {path}")); }
        }
        let context = value.get("context_files").and_then(Value::as_array).ok_or("Evidence needs a project Ruby source context; recapture it")?;
        if context.len() > 200_000 { return Err("Too many evidence context files".into()); }
        let mut captured_context = HashSet::new();
        for path in context {
            let path = path.as_str().filter(|path| relative_path(path) && path.ends_with(".rb")).ok_or("Invalid evidence Ruby source context")?;
            if !captured_context.insert(path) { return Err("Duplicate evidence Ruby source context file".into()); }
            if !validated.contains_key(path) { return Err("Evidence Ruby source context is missing or stale; recapture it".into()); }
        }
        let current_context: HashSet<_> = sources.iter().filter(|source| source.path.ends_with(".rb")).map(|source| source.path.as_str()).collect();
        if captured_context != current_context { return Err("Evidence Ruby source files changed; recapture it".into()); }
        let classes = array(&value, "classes")?;
        let mut names = HashMap::new();
        for class in classes {
            if let Some(name) = class.get("name").and_then(Value::as_str) { *names.entry(name).or_insert(0usize) += 1; }
        }
        for class in classes {
            let Some(parsed) = parse_class(class, &validated) else { result.warn("Skipped invalid or stale loaded class".into()); continue };
            if names.get(parsed.name.as_str()) != Some(&1) { result.warn(format!("Conflicting loaded class: {}", parsed.name)); continue; }
            result.classes.push(parsed);
        }
        for call in array(&value, "calls")? {
            let Some(parsed) = parse_call(call, &validated) else { result.warn("Skipped invalid or stale observed call".into()); continue };
            result.calls.push(parsed);
        }
        Ok(result)
    }

    fn warn(&mut self, warning: String) {
        if self.warnings.len() < 32 {
            self.warnings.push(warning.chars().take(320).map(|c| if c.is_control() { ' ' } else { c }).collect());
        }
    }
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    let Some(value) = value.get(key) else { return Ok(&[]) };
    let values = value.as_array().ok_or_else(|| format!("Evidence {key} must be an array"))?;
    if values.len() > 200_000 { return Err(format!("Too many evidence {key}")); }
    Ok(values)
}

fn relative_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.starts_with('/') && !path.contains('\\') && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn name(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?;
    (!text.is_empty() && text.len() <= 512 && !text.chars().any(|c| c.is_control() || c.is_whitespace())).then(|| text.into())
}

fn constant(text: &str) -> bool {
    !text.is_empty() && text.split("::").all(|part| {
        let mut chars = part.chars();
        chars.next().is_some_and(char::is_uppercase) && chars.all(|c| c.is_alphanumeric() || c == '_')
    })
}

fn method_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256 && !text.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '#' | '.' | ':'))
}

fn symbol_name(text: &str) -> bool {
    if text == "<module>" { return true; }
    match text.rsplit_once('#').or_else(|| text.rsplit_once('.')) {
        Some((owner, method)) => constant(owner) && method_name(method),
        None => method_name(text),
    }
}

fn location(value: &Value, files: &HashMap<&str, usize>) -> Option<(String, usize)> {
    let file = value.get("file")?.as_str()?;
    let line = usize::try_from(value.get("line")?.as_u64()?).ok()?;
    (line > 0 && line <= *files.get(file)?).then(|| (file.into(), line))
}

fn symbol(value: &Value, files: &HashMap<&str, usize>) -> Option<SymbolRef> {
    let name = name(value, "name").filter(|name| symbol_name(name))?;
    let (file, line) = location(value, files)?;
    Some(SymbolRef { name, file, line })
}

fn parse_class(value: &Value, files: &HashMap<&str, usize>) -> Option<LoadedClass> {
    let class_name = name(value, "name").filter(|name| constant(name))?;
    let (file, line) = location(value, files)?;
    let ancestors: Vec<String> = array(value, "ancestors").ok()?.iter().map(|v| v.as_str().filter(|s| constant(s)).map(str::to_owned)).collect::<Option<_>>()?;
    if ancestors.len() > 256 { return None; }
    let active_record = value.get("active_record").and_then(Value::as_bool).unwrap_or(false);
    if active_record && !ancestors.iter().any(|a| a == "ActiveRecord::Base") { return None; }
    let attributes = array(value, "attributes").ok()?.iter().map(|v| v.as_str().filter(|s| !s.is_empty() && s.len() <= 256 && s.chars().all(|c| c.is_alphanumeric() || c == '_')).map(str::to_owned)).collect::<Option<Vec<_>>>()?;
    let mut associations = Vec::new();
    let mut association_names = HashSet::new();
    for association in array(value, "associations").ok()? {
        let association_name = name(association, "name").filter(|name| method_name(name))?;
        if !association_names.insert(association_name.clone()) { return None; }
        let target = name(association, "target").filter(|target| constant(target))?;
        associations.push(LoadedAssociation { name: association_name, target, collection: association.get("collection")?.as_bool()? });
    }
    let mut methods = Vec::new();
    let mut method_names = HashSet::new();
    for method in array(value, "methods").ok()? {
        let method_name = name(method, "name").filter(|name| method_name(name))?;
        let owner = name(method, "owner").filter(|owner| constant(owner))?;
        let singleton = method.get("singleton")?.as_bool()?;
        if !method_names.insert((singleton, method_name.clone())) { return None; }
        let (file, line) = location(method, files)?;
        let definition_name = match method.get("definition_name") {
            Some(_) => name(method, "definition_name").filter(|name| symbol_name(name))?,
            None => format!("{owner}{}{method_name}", if singleton { "." } else { "#" }),
        };
        if !definition_name.starts_with(&format!("{owner}#")) && !definition_name.starts_with(&format!("{owner}.")) { return None; }
        methods.push(LoadedMethod { name: method_name, singleton, definition: SymbolRef { name: definition_name, file, line } });
    }
    Some(LoadedClass { name: class_name, file, line, active_record, attributes, associations, methods })
}

fn parse_call(value: &Value, files: &HashMap<&str, usize>) -> Option<ObservedCall> {
    let caller = symbol(value.get("caller")?, files)?;
    let callee = symbol(value.get("callee")?, files)?;
    let line = usize::try_from(value.get("line")?.as_u64()?).ok()?;
    if line == 0 || line > *files.get(caller.file.as_str())? { return None; }
    let method = name(value, "method").filter(|name| method_name(name))?;
    let count = value.get("count")?.as_u64()?;
    if count == 0 { return None; }
    Some(ObservedCall { caller, callee, line, method, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_modes_select_the_same_evidence_kinds() {
        let kinds = [EvidenceKind::Source,EvidenceKind::Inferred,EvidenceKind::Loaded,EvidenceKind::Observed];
        let expected = [[true,true,true,true],[true,false,false,false],[true,true,false,false],[false,false,true,false],[false,false,false,true]];
        for (mode, expected) in expected.iter().enumerate() {
            let filter = EvidenceFilter::for_mode(mode);
            assert_eq!(kinds.map(|kind| filter.allows(kind)), *expected);
        }
    }

    #[test]
    fn fingerprints_match_the_export_protocol() {
        assert_eq!(fingerprint(""), "cbf29ce484222325");
        assert_eq!(fingerprint("hello"), "a430d84680aabd0b");
    }

    #[test]
    fn malformed_versions_stale_files_and_outside_paths_cannot_supply_evidence() {
        let sources = [SourceFile { path: "a.rb".into(), text: "class A; end".into() }];
        assert!(Snapshot::parse("{", &sources).is_err());
        assert!(Snapshot::parse(r#"{"version":2,"files":{}}"#, &sources).is_err());
        let text = serde_json::json!({"version":1,"context_files":["a.rb"],"files":{"a.rb":"wrong","../a.rb":fingerprint(&sources[0].text)},"classes":[{"name":"A","file":"a.rb","line":1}]});
        assert!(Snapshot::parse(&text.to_string(), &sources).is_err());
    }

    #[test]
    fn bad_locations_and_conflicting_classes_do_not_discard_independent_valid_calls() {
        let sources = [SourceFile { path: "a.rb".into(), text: "class A; def go; end; end".into() }];
        let class = serde_json::json!({"name":"A","file":"a.rb","line":1});
        let call = serde_json::json!({"caller":{"name":"A#go","file":"a.rb","line":1},"callee":{"name":"A#go","file":"a.rb","line":1},"line":1,"method":"go","count":1});
        let mut bad_line = call.clone(); bad_line["line"] = serde_json::json!(2);
        let mut bad_name = call.clone(); bad_name["callee"]["name"] = serde_json::json!("A#");
        let json = serde_json::json!({"version":1,"context_files":["a.rb"],"files":{"a.rb":fingerprint(&sources[0].text)},"classes":[class.clone(),class],"calls":[call,bad_line,bad_name]});
        let snapshot = Snapshot::parse(&json.to_string(), &sources).unwrap();
        assert_eq!(snapshot.calls.len(), 1);
        assert!(snapshot.classes.is_empty());
        assert!(!snapshot.warnings.is_empty());
    }

    #[test]
    fn collector_incompleteness_warnings_survive_import_with_bounded_safe_text() {
        let warnings: Vec<_> = (0..40).map(|_| format!("Trace event limit reached\n{}", "x".repeat(1000))).collect();
        let json = serde_json::json!({"version":1,"context_files":[],"files":{},"warnings":warnings});
        let snapshot = Snapshot::parse(&json.to_string(), &[]).unwrap();
        assert!(snapshot.warnings[0].starts_with("Capture warning: Trace event limit reached"));
        assert!(snapshot.warnings.len() <= 32);
        assert!(snapshot.warnings.iter().all(|warning| warning.chars().count() <= 320 && !warning.chars().any(char::is_control)));
    }

    #[test]
    fn context_changes_and_missing_context_reject_the_entire_capture() {
        let sources = [SourceFile { path: "a.rb".into(), text: "class A; end".into() }, SourceFile { path: "configure.rb".into(), text: "A.include M".into() }];
        let mut json = serde_json::json!({"version":1,"files":{"a.rb":fingerprint(&sources[0].text),"configure.rb":fingerprint(&sources[1].text)},"context_files":["a.rb","configure.rb"]});
        assert!(Snapshot::parse(&json.to_string(), &sources).is_ok());
        let changed = [sources[0].clone(), SourceFile { path: "configure.rb".into(), text: "# removed include".into() }];
        assert!(Snapshot::parse(&json.to_string(), &changed).is_err());
        assert!(Snapshot::parse(&json.to_string(), &sources[..1]).is_err());
        let mut added = sources.to_vec(); added.push(SourceFile { path: "another.rb".into(), text: "class A; end".into() });
        assert!(Snapshot::parse(&json.to_string(), &added).is_err());
        json.as_object_mut().unwrap().remove("context_files");
        assert!(Snapshot::parse(&json.to_string(), &sources).is_err());
    }
}
