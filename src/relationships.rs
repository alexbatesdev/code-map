//! Conservative source-level call relationships. Imports are bindings, never edges.

use std::collections::{HashMap, HashSet, VecDeque};
use tree_sitter::{Node, Parser};

pub const MAX_DEPTH: usize = 8;

pub fn supports_path(path: &str) -> bool {
    parser_language(path).is_some()
}

fn parser_language(path: &str) -> Option<tree_sitter::Language> {
    if path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts") { return None; }
    Some(match path.rsplit_once('.')?.1 {
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRef {
    pub name: String,
    pub file: String,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallEdge {
    pub caller: SymbolRef,
    pub callee: SymbolRef,
    pub line: usize,
    /// Zero-based rendered Unicode scalar columns; end is exclusive and each tab occupies four.
    pub start_column: usize,
    pub end_column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Callees,
    Callers,
}

#[derive(Clone, Debug, Default)]
pub struct Reachability {
    pub paths: HashSet<String>,
    pub direct: HashSet<String>,
    pub indirect: HashSet<String>,
    pub witnesses: HashMap<String, Vec<CallEdge>>,
    pub call_sites: Vec<CallEdge>,
    pub reached_symbols: usize,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub analyzed_files: usize,
    /// Number of supported files rejected because their AST contains errors.
    pub parse_errors: usize,
    pub unresolved_sites: usize,
    pub symbols: Vec<SymbolRef>,
    pub edges: Vec<CallEdge>,
    outgoing: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
    endpoints: Vec<(usize, usize)>,
    files: HashMap<String, Vec<usize>>,
}

impl Graph {
    /// Paths and base_url are project-relative, with '/' separators.
    /// Only explicit baseUrl roots participate in non-relative import resolution.
    pub fn build(sources: &[SourceFile], base_url: Option<&str>) -> Self {
        let mut builder = Builder::new(sources, base_url);
        let mut parser = Parser::new();
        for (file, source) in sources.iter().enumerate() {
            let Some(language) = parser_language(&source.path) else { continue };
            builder.graph.analyzed_files += 1;
            if parser.set_language(&language).is_err() {
                builder.graph.parse_errors += 1;
                continue;
            }
            let Some(tree) = parser.parse(&source.text, None) else {
                builder.graph.parse_errors += 1;
                continue;
            };
            if tree.root_node().has_error() {
                builder.graph.parse_errors += 1;
                continue;
            }
            let module = builder.symbol(file, "<module>".into(), 1);
            let scope = builder.scope(None, true);
            builder.file_scopes[file] = Some(scope);
            if source.path.ends_with(".rb") {
                let context = RubyContext { caller: module, scope, class: None, nesting: Vec::new(), singleton: true, definitions_singleton: false };
                builder.ruby(tree.root_node(), file, &context);
            } else {
                builder.js(tree.root_node(), file, scope, module, None);
            }
        }
        builder.finish()
    }

    /// Traverses symbols first; only then groups shortest witnesses by file.
    pub fn related(&self, file: &str, direction: Direction, depth: usize) -> Reachability {
        let mut result = Reachability::default();
        result.paths.insert(file.to_owned());
        let Some(roots) = self.files.get(file) else { return result };
        let depth = depth.min(MAX_DEPTH);
        if depth == 0 { return result; }
        let mut visited = vec![false; self.symbols.len()];
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();
        for &root in roots {
            visited[root] = true;
            queue.push_back((root, Vec::<usize>::new()));
        }
        while let Some((at, chain)) = queue.pop_front() {
            if chain.len() == depth { continue; }
            let edges = match direction { Direction::Callees => &self.outgoing[at], Direction::Callers => &self.incoming[at] };
            for &edge in edges {
                if visited_edges.insert(edge) { result.call_sites.push(self.edges[edge].clone()); }
                let (caller, callee) = self.endpoints[edge];
                let next = match direction { Direction::Callees => callee, Direction::Callers => caller };
                if visited[next] { continue; }
                visited[next] = true;
                result.reached_symbols += 1;
                let mut witness = chain.clone();
                match direction {
                    Direction::Callees => witness.push(edge),
                    Direction::Callers => witness.insert(0, edge),
                }
                let path = &self.symbols[next].file;
                if path != file && !result.paths.contains(path) {
                    result.paths.insert(path.clone());
                    if witness.len() == 1 { result.direct.insert(path.clone()); }
                    else { result.indirect.insert(path.clone()); }
                    result.witnesses.insert(path.clone(), witness.iter().map(|&i| self.edges[i].clone()).collect());
                }
                queue.push_back((next, witness));
            }
        }
        result
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Callable(usize),
    Class(usize),
    Import { file: usize, module: String, name: String },
    Namespace { file: usize, module: String },
    Unknown,
}

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    function: bool,
    bindings: HashMap<String, Binding>,
}

#[derive(Default)]
struct JsClass {
    methods: HashMap<(bool, String), Option<usize>>,
    dynamic_members: HashSet<bool>,
    unknown_instance: bool,
}

#[derive(Clone)]
enum Export {
    Local(String),
    Value(Binding),
    From(String, String),
}

#[derive(Clone)]
enum Expr {
    Name(String),
    Member(Box<Expr>, String),
    New(Box<Expr>),
    This,
    Inline(usize, usize),
    Unknown,
}

struct JsCall {
    caller: usize,
    scope: usize,
    class: Option<(usize, bool)>,
    target: Expr,
    constructor: bool,
    site: CallSite,
}

#[derive(Clone, Copy)]
struct CallSite {
    line: usize,
    start_column: usize,
    end_column: usize,
}

#[derive(Clone)]
struct RubyContext {
    caller: usize,
    scope: usize,
    class: Option<String>,
    nesting: Vec<String>,
    singleton: bool,
    definitions_singleton: bool,
}

#[derive(Clone)]
enum RubyReceiver {
    Implicit,
    SelfValue,
    Constant(String),
    Instance(String),
    Unknown,
}

struct RubyCall {
    file: usize,
    context: RubyContext,
    receiver: RubyReceiver,
    method: String,
    bare: bool,
    site: CallSite,
}

struct Builder<'a> {
    sources: &'a [SourceFile],
    base_url: Option<String>,
    graph: Graph,
    scopes: Vec<Scope>,
    file_scopes: Vec<Option<usize>>,
    module_paths: HashMap<String, Vec<usize>>,
    classes: Vec<JsClass>,
    node_values: HashMap<(usize, usize), Binding>,
    exports: Vec<HashMap<String, Vec<Export>>>,
    js_calls: Vec<JsCall>,
    assignments: Vec<(usize, String)>,
    member_assignments: Vec<(usize, Option<(usize, bool)>, Expr, String)>,
    ruby_calls: Vec<RubyCall>,
    ruby_classes: HashSet<String>,
    ruby_constants: HashSet<String>,
    ruby_assigned_constants: HashSet<String>,
    ruby_qualified_assignments: Vec<(String, Vec<String>)>,
    ruby_inherited_classes: HashSet<String>,
    ruby_methods: HashMap<(String, bool, String), Vec<usize>>,
    ruby_top_methods: HashMap<(usize, String), Vec<usize>>,
}

impl<'a> Builder<'a> {
    fn new(sources: &'a [SourceFile], base_url: Option<&str>) -> Self {
        let mut module_paths: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, source) in sources.iter().enumerate() {
            module_paths.entry(normalize(&source.path)).or_default().push(i);
        }
        Self {
            sources, base_url: base_url.map(normalize), graph: Graph::default(), scopes: Vec::new(),
            file_scopes: vec![None; sources.len()], module_paths, classes: Vec::new(),
            node_values: HashMap::new(), exports: vec![HashMap::new(); sources.len()],
            js_calls: Vec::new(), assignments: Vec::new(), member_assignments: Vec::new(),
            ruby_calls: Vec::new(), ruby_classes: HashSet::new(), ruby_constants: HashSet::new(), ruby_assigned_constants: HashSet::new(), ruby_qualified_assignments: Vec::new(), ruby_inherited_classes: HashSet::new(), ruby_methods: HashMap::new(),
            ruby_top_methods: HashMap::new(),
        }
    }

    fn text(&self, file: usize, node: Node<'_>) -> &str {
        &self.sources[file].text[node.byte_range()]
    }

    fn call_site(&self, file: usize, node: Node<'_>) -> CallSite {
        let position = node.start_position();
        let prefix = &self.sources[file].text[node.start_byte() - position.column..node.start_byte()];
        let columns = |text: &str| text.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>();
        let start_column = columns(prefix);
        let name = self.text(file, node).split(['\r', '\n']).next().unwrap_or("");
        CallSite { line: position.row + 1, start_column, end_column: start_column + columns(name) }
    }

    fn symbol(&mut self, file: usize, name: String, line: usize) -> usize {
        let id = self.graph.symbols.len();
        let path = self.sources[file].path.clone();
        self.graph.files.entry(path.clone()).or_default().push(id);
        self.graph.symbols.push(SymbolRef { name, file: path, line });
        id
    }

    fn scope(&mut self, parent: Option<usize>, function: bool) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope { parent, function, bindings: HashMap::new() });
        id
    }

    fn bind(&mut self, scope: usize, name: String, value: Binding) {
        self.scopes[scope].bindings.entry(name).and_modify(|existing| {
            if *existing != value { *existing = Binding::Unknown; }
        }).or_insert(value);
    }

    fn lookup(&self, mut scope: usize, name: &str) -> Binding {
        loop {
            if let Some(value) = self.scopes[scope].bindings.get(name) { return value.clone(); }
            let Some(parent) = self.scopes[scope].parent else { return Binding::Unknown };
            scope = parent;
        }
    }

    fn pattern(&mut self, node: Node<'_>, file: usize, scope: usize) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                self.bind(scope, self.text(file, node).into(), Binding::Unknown);
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(pattern) = node.child_by_field_name("pattern").or_else(|| node.child_by_field_name("name")) {
                    self.pattern(pattern, file, scope);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(left) = node.child_by_field_name("left") { self.pattern(left, file, scope); }
            }
            "pair_pattern" => {
                if let Some(value) = node.child_by_field_name("value") { self.pattern(value, file, scope); }
            }
            "type_annotation" | "type_identifier" => {}
            _ => for child in named(node) { self.pattern(child, file, scope); },
        }
    }

    fn export(&mut self, file: usize, name: String, value: Export) {
        self.exports[file].entry(name).or_default().push(value);
    }

    fn js(&mut self, node: Node<'_>, file: usize, scope: usize, caller: usize, class: Option<(usize, bool)>) -> Option<Binding> {
        let kind = node.kind();
        match kind {
            "import_statement" => {
                self.js_import(node, file, scope);
                return None;
            }
            "export_statement" => {
                self.js_export(node, file, scope, caller, class);
                return None;
            }
            "function_declaration" | "generator_function_declaration" | "function_expression" | "generator_function" | "arrow_function" => {
                let name = node.child_by_field_name("name").map(|n| self.text(file, n).to_owned())
                    .or_else(|| node.parent().filter(|p| p.kind() == "variable_declarator").and_then(|p| p.child_by_field_name("name")).filter(|n| n.kind() == "identifier").map(|n| self.text(file, n).to_owned()))
                    .unwrap_or_else(|| format!("<function@{}>", node.start_position().row + 1));
                let id = self.symbol(file, name.clone(), node.start_position().row + 1);
                let value = Binding::Callable(id);
                self.node_values.insert((file, node.start_byte()), value.clone());
                if matches!(kind, "function_declaration" | "generator_function_declaration") {
                    self.bind(scope, name.clone(), value.clone());
                }
                let inner = self.scope(Some(scope), true);
                if node.child_by_field_name("name").is_some() { self.bind(inner, name, value.clone()); }
                for field in ["parameters", "parameter"] {
                    if let Some(parameters) = node.child_by_field_name(field) {
                        self.pattern(parameters, file, inner);
                        self.js_parameter_values(parameters, file, inner, id, None);
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.js(body, file, inner, id, if kind == "arrow_function" { class } else { None });
                }
                return Some(value);
            }
            "class_declaration" | "class" | "abstract_class_declaration" => {
                let name = node.child_by_field_name("name").map(|n| self.text(file, n).to_owned());
                let class_id = self.classes.len();
                self.classes.push(JsClass { unknown_instance: named(node).iter().any(|n| n.kind() == "class_heritage"), ..Default::default() });
                let value = Binding::Class(class_id);
                self.node_values.insert((file, node.start_byte()), value.clone());
                if kind != "class" {
                    if let Some(name) = &name { self.bind(scope, name.clone(), value.clone()); }
                }
                let inner = self.scope(Some(scope), false);
                if let Some(name) = &name { self.bind(inner, name.clone(), value.clone()); }
                if let Some(body) = node.child_by_field_name("body") {
                    for child in named(body) { self.js(child, file, inner, caller, Some((class_id, false))); }
                }
                return Some(value);
            }
            "method_definition" => {
                let name_node = node.child_by_field_name("name");
                let name = name_node.map(|n| self.property_name(n, file).unwrap_or_else(|| self.text(file, n).to_owned())).unwrap_or_else(|| "<method>".into());
                let singleton = token(node, "static");
                let id = self.symbol(file, name.clone(), node.start_position().row + 1);
                let method_class = class.filter(|_| node.parent().is_some_and(|p| p.kind() == "class_body")).map(|(id, _)| (id, singleton));
                if let Some((class_id, _)) = method_class {
                    if let Some(name) = name_node.and_then(|n| self.property_name(n, file)) {
                        let value = if token(node, "get") || token(node, "set") || (name == "constructor" && name_node.is_some_and(|n| n.kind() == "computed_property_name")) { None } else { Some(id) };
                        self.classes[class_id].methods.entry((singleton, name)).and_modify(|old| *old = None).or_insert(value);
                    } else {
                        self.classes[class_id].dynamic_members.insert(singleton);
                    }
                }
                let inner = self.scope(Some(scope), true);
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.pattern(params, file, inner);
                    self.js_parameter_values(params, file, inner, id, method_class);
                }
                if let Some(body) = node.child_by_field_name("body") { self.js(body, file, inner, id, method_class); }
                return Some(Binding::Callable(id));
            }
            "variable_declarator" => {
                let value = node.child_by_field_name("value").and_then(|n| self.js(n, file, scope, caller, class)).unwrap_or(Binding::Unknown);
                if let Some(name) = node.child_by_field_name("name") {
                    let mut target_scope = scope;
                    if node.parent().is_some_and(|p| p.kind() == "variable_declaration") {
                        while !self.scopes[target_scope].function {
                            let Some(parent) = self.scopes[target_scope].parent else { break };
                            target_scope = parent;
                        }
                    }
                    if name.kind() == "identifier" { self.bind(target_scope, self.text(file, name).into(), value.clone()); }
                    else { self.pattern(name, file, target_scope); }
                }
                return Some(value);
            }
            "field_definition" | "public_field_definition" | "class_static_block" => {
                let id = self.symbol(file, format!("<initializer@{}>", node.start_position().row + 1), node.start_position().row + 1);
                let context = class.map(|(id, _)| (id, token(node, "static")));
                if let Some((class_id, singleton)) = context {
                    if let Some(name) = node.child_by_field_name("name").or_else(|| node.child_by_field_name("property")) {
                        if let Some(name) = self.property_name(name, file) {
                            self.classes[class_id].methods.insert((singleton, name), None);
                        } else {
                            self.classes[class_id].dynamic_members.insert(singleton);
                        }
                    }
                }
                if let Some(value) = node.child_by_field_name("value").or_else(|| node.child_by_field_name("body")) {
                    self.js(value, file, scope, id, context);
                }
                return None;
            }
            "statement_block" | "catch_clause" => {
                let inner = self.scope(Some(scope), false);
                if let Some(parameter) = node.child_by_field_name("parameter") { self.pattern(parameter, file, inner); }
                for child in named(node) {
                    if node.child_by_field_name("parameter") != Some(child) { self.js(child, file, inner, caller, class); }
                }
                return None;
            }
            "for_statement" | "for_in_statement" => {
                let inner = self.scope(Some(scope), false);
                let left = if kind == "for_in_statement" { node.child_by_field_name("left") } else { None };
                if let Some(left) = left {
                    if let Some(declaration) = node.child_by_field_name("kind") {
                        let mut target_scope = inner;
                        if declaration.kind() == "var" {
                            while !self.scopes[target_scope].function {
                                let Some(parent) = self.scopes[target_scope].parent else { break };
                                target_scope = parent;
                            }
                        }
                        self.pattern(left, file, target_scope);
                    } else {
                        self.js_assignment(left, file, inner, class);
                    }
                }
                for child in named(node) {
                    if Some(child) != left { self.js(child, file, inner, caller, class); }
                }
                return None;
            }
            "assignment_expression" | "augmented_assignment_expression" | "update_expression" => {
                if let Some(left) = node.child_by_field_name("left").or_else(|| node.child_by_field_name("argument")) {
                    self.js_assignment(left, file, scope, class);
                }
            }
            "call_expression" | "new_expression" => {
                let constructor = kind == "new_expression";
                let function = node.child_by_field_name(if constructor { "constructor" } else { "function" });
                let target = function.map(|n| Self::expression(n, file, &self.sources[file].text)).unwrap_or(Expr::Unknown);
                let site = function.and_then(Self::js_call_name).map(|name| self.call_site(file, name)).unwrap_or_else(|| {
                    let mut site = self.call_site(file, node);
                    site.end_column = site.start_column;
                    site
                });
                self.js_calls.push(JsCall { caller, scope, class, target, constructor, site });
            }
            "return_statement" => {
                if let Some((class_id, false)) = class {
                    if self.graph.symbols[caller].name == "constructor" && named(node).iter().any(|n| n.kind() != "this") {
                        self.classes[class_id].unknown_instance = true;
                    }
                }
            }
            "function_signature" | "interface_declaration" | "type_alias_declaration" | "ambient_declaration" | "method_signature" => return None,
            _ => {}
        }
        for child in named(node) { self.js(child, file, scope, caller, class); }
        None
    }

    fn js_parameter_values(&mut self, node: Node<'_>, file: usize, scope: usize, caller: usize, class: Option<(usize, bool)>) {
        let value = node.child_by_field_name("value").or_else(|| node.child_by_field_name("right"));
        if let Some(value) = value {
            self.js(value, file, scope, caller, class);
        }
        for child in named(node) {
            if Some(child) != value && !matches!(child.kind(), "type_annotation" | "type_identifier") { self.js_parameter_values(child, file, scope, caller, class); }
        }
    }

    fn property_name(&self, node: Node<'_>, file: usize) -> Option<String> {
        match node.kind() {
            "property_identifier" | "private_property_identifier" | "identifier" | "number" => Some(self.text(file, node).into()),
            "string" => literal(self.text(file, node)),
            "computed_property_name" => named(node).first().filter(|n| matches!(n.kind(), "string" | "number")).and_then(|&n| self.property_name(n, file)),
            _ => None,
        }
    }

    fn js_assignment(&mut self, node: Node<'_>, file: usize, scope: usize, class: Option<(usize, bool)>) {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => self.assignments.push((scope, self.text(file, node).into())),
            "member_expression" => if let Expr::Member(object, name) = Self::expression(node, file, &self.sources[file].text) {
                self.member_assignments.push((scope, class, *object, name));
            },
            "pair_pattern" => if let Some(value) = node.child_by_field_name("value") { self.js_assignment(value, file, scope, class); },
            "assignment_pattern" | "object_assignment_pattern" => if let Some(left) = node.child_by_field_name("left") { self.js_assignment(left, file, scope, class); },
            "object_pattern" | "array_pattern" | "rest_pattern" | "parenthesized_expression" | "non_null_expression" => {
                for child in named(node) { self.js_assignment(child, file, scope, class); }
            }
            _ => {}
        }
    }

    fn js_import(&mut self, node: Node<'_>, file: usize, scope: usize) {
        let Some(source) = node.child_by_field_name("source") else { return };
        let Some(module) = literal(self.text(file, source)) else { return };
        let type_only = token(node, "type");
        for clause in named(node).into_iter().filter(|n| n.kind() == "import_clause") {
            for child in named(clause) {
                match child.kind() {
                    "identifier" => self.bind(scope, self.text(file, child).into(), if type_only { Binding::Unknown } else { Binding::Import { file, module: module.clone(), name: "default".into() } }),
                    "namespace_import" => {
                        if let Some(name) = named(child).into_iter().find(|n| n.kind() == "identifier") {
                            self.bind(scope, self.text(file, name).into(), if type_only { Binding::Unknown } else { Binding::Namespace { file, module: module.clone() } });
                        }
                    }
                    "named_imports" => for specifier in named(child) {
                        if specifier.kind() != "import_specifier" { continue; }
                        let Some(name) = specifier.child_by_field_name("name") else { continue };
                        let local = specifier.child_by_field_name("alias").unwrap_or(name);
                        self.bind(scope, self.text(file, local).into(), if type_only || token(specifier, "type") { Binding::Unknown } else {
                            Binding::Import { file, module: module.clone(), name: self.text(file, name).into() }
                        });
                    },
                    _ => {}
                }
            }
        }
    }

    fn js_export(&mut self, node: Node<'_>, file: usize, scope: usize, caller: usize, class: Option<(usize, bool)>) {
        if token(node, "type") { return; }
        let default = token(node, "default");
        if let Some(declaration) = node.child_by_field_name("declaration") {
            let value = self.js(declaration, file, scope, caller, class);
            if default {
                self.export(file, "default".into(), Export::Value(value.unwrap_or(Binding::Unknown)));
            } else if let Some(name) = declaration.child_by_field_name("name") {
                let name = self.text(file, name).to_owned();
                self.export(file, name.clone(), Export::Local(name));
            } else {
                for declarator in named(declaration).into_iter().filter(|n| n.kind() == "variable_declarator") {
                    if let Some(name) = declarator.child_by_field_name("name").filter(|n| n.kind() == "identifier") {
                        let name = self.text(file, name).to_owned();
                        self.export(file, name.clone(), Export::Local(name));
                    }
                }
            }
            return;
        }
        if let Some(value) = node.child_by_field_name("value") {
            let binding = self.js(value, file, scope, caller, class);
            if value.kind() == "identifier" {
                self.export(file, "default".into(), Export::Local(self.text(file, value).into()));
            } else {
                self.export(file, "default".into(), Export::Value(binding.unwrap_or(Binding::Unknown)));
            }
            return;
        }
        let source = node.child_by_field_name("source").and_then(|n| literal(self.text(file, n)));
        for clause in named(node).into_iter().filter(|n| n.kind() == "export_clause") {
            for specifier in named(clause) {
                if token(specifier, "type") { continue; }
                if let Some(name) = specifier.child_by_field_name("name") {
                    let alias = specifier.child_by_field_name("alias").unwrap_or(name);
                    let original = self.text(file, name).to_owned();
                    let exported = self.text(file, alias).to_owned();
                    self.export(file, exported, match &source {
                        Some(source) => Export::From(source.clone(), original), None => Export::Local(original)
                    });
                }
            }
        }
    }

    fn expression(node: Node<'_>, file: usize, text: &str) -> Expr {
        match node.kind() {
            "identifier" => Expr::Name(text[node.byte_range()].into()),
            "this" => Expr::This,
            "member_expression" => {
                let Some(object) = node.child_by_field_name("object") else { return Expr::Unknown };
                let Some(property) = node.child_by_field_name("property") else { return Expr::Unknown };
                if !matches!(property.kind(), "property_identifier" | "private_property_identifier") { return Expr::Unknown; }
                Expr::Member(Box::new(Self::expression(object, file, text)), text[property.byte_range()].into())
            }
            "new_expression" => node.child_by_field_name("constructor").map(|n| Expr::New(Box::new(Self::expression(n, file, text)))).unwrap_or(Expr::Unknown),
            "parenthesized_expression" | "non_null_expression" | "as_expression" | "satisfies_expression" | "instantiation_expression" => named(node).first().map(|&n| Self::expression(n, file, text)).unwrap_or(Expr::Unknown),
            "function_expression" | "generator_function" | "arrow_function" | "class" => Expr::Inline(file, node.start_byte()),
            _ => Expr::Unknown,
        }
    }

    fn js_call_name(node: Node<'_>) -> Option<Node<'_>> {
        match node.kind() {
            "identifier" | "property_identifier" | "private_property_identifier" => Some(node),
            "member_expression" => node.child_by_field_name("property").and_then(Self::js_call_name),
            "parenthesized_expression" | "non_null_expression" | "as_expression" | "satisfies_expression" | "instantiation_expression" => named(node).first().copied().and_then(Self::js_call_name),
            _ => None,
        }
    }

    fn module(&self, file: usize, specifier: &str) -> Option<usize> {
        let base = if specifier.starts_with("./") || specifier.starts_with("../") {
            let directory = self.sources[file].path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
            normalize(&format!("{directory}/{specifier}"))
        } else {
            let base_url = self.base_url.as_ref()?;
            if specifier.starts_with('/') { return None; }
            normalize(&format!("{base_url}/{specifier}"))
        };
        if let Some(files) = self.module_paths.get(&base) {
            if files.len() == 1 && self.file_scopes[files[0]].is_some() { return Some(files[0]); }
            return None;
        }
        let mut candidates = Vec::new();
        for ext in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
            for candidate in [format!("{base}.{ext}"), format!("{base}/index.{ext}")] {
                if let Some(files) = self.module_paths.get(&candidate) {
                    candidates.extend(files.iter().copied().filter(|&i| self.file_scopes[i].is_some()));
                }
            }
        }
        if candidates.len() == 1 { Some(candidates[0]) } else { None }
    }

    fn resolve_binding(&self, binding: Binding, seen: &mut HashSet<(usize, String)>) -> Binding {
        match binding {
            Binding::Import { file, module, name } => {
                let Some(target) = self.module(file, &module) else { return Binding::Unknown };
                self.resolve_export(target, &name, seen)
            }
            other => other,
        }
    }

    fn resolve_export(&self, file: usize, name: &str, seen: &mut HashSet<(usize, String)>) -> Binding {
        if !seen.insert((file, name.into())) { return Binding::Unknown; }
        let Some(exports) = self.exports[file].get(name).filter(|exports| exports.len() == 1) else { return Binding::Unknown };
        let value = match &exports[0] {
            Export::Local(name) => self.file_scopes[file].map(|scope| self.lookup(scope, name)).unwrap_or(Binding::Unknown),
            Export::Value(value) => value.clone(),
            Export::From(module, name) => Binding::Import { file, module: module.clone(), name: name.clone() },
        };
        self.resolve_binding(value, seen)
    }

    fn js_value(&self, expr: &Expr, scope: usize, class: Option<(usize, bool)>) -> Binding {
        let value = match expr {
            Expr::Name(name) => self.lookup(scope, name),
            Expr::Inline(file, start) => self.node_values.get(&(*file, *start)).cloned().unwrap_or(Binding::Unknown),
            Expr::This => class.filter(|(_, singleton)| *singleton).map(|(id, _)| Binding::Class(id)).unwrap_or(Binding::Unknown),
            Expr::Member(object, name) => match self.js_value(object, scope, class) {
                Binding::Namespace { file, module } => self.module(file, &module).map(|target| self.resolve_export(target, name, &mut HashSet::new())).unwrap_or(Binding::Unknown),
                Binding::Class(id) => self.js_method(id, true, name).map(Binding::Callable).unwrap_or(Binding::Unknown),
                _ => {
                    let instance = match object.as_ref() {
                        Expr::New(constructor) => match self.js_value(constructor, scope, class) { Binding::Class(id) if !self.classes[id].unknown_instance => Some(id), _ => None },
                        Expr::This => class.filter(|(_, singleton)| !*singleton).map(|(id, _)| id),
                        _ => None,
                    };
                    instance.and_then(|id| self.js_method(id, false, name)).map(Binding::Callable).unwrap_or(Binding::Unknown)
                }
            },
            _ => Binding::Unknown,
        };
        self.resolve_binding(value, &mut HashSet::new())
    }

    fn js_method(&self, class: usize, singleton: bool, name: &str) -> Option<usize> {
        if self.classes[class].dynamic_members.contains(&singleton) { return None; }
        self.classes[class].methods.get(&(singleton, name.to_owned())).copied().flatten()
    }

    fn ruby(&mut self, node: Node<'_>, file: usize, context: &RubyContext) {
        match node.kind() {
            "class" | "module" => {
                let Some(name) = node.child_by_field_name("name") else { return };
                let raw = self.text(file, name).to_owned();
                if !raw.starts_with("::") && raw.contains("::") && !context.nesting.is_empty() { return; }
                let qualified = if raw.starts_with("::") { raw.trim_start_matches("::").to_owned() }
                    else if let Some(parent) = context.nesting.first() { format!("{parent}::{raw}") } else { raw };
                self.ruby_classes.insert(qualified.clone());
                self.ruby_constants.insert(qualified.clone());
                if node.child_by_field_name("superclass").is_some() { self.ruby_inherited_classes.insert(qualified.clone()); }
                let scope = self.scope(None, true);
                let body_symbol = self.symbol(file, format!("{qualified}.<body>"), node.start_position().row + 1);
                let mut inner = context.clone();
                inner.caller = body_symbol;
                inner.scope = scope;
                inner.class = Some(qualified.clone());
                inner.nesting.insert(0, qualified);
                inner.singleton = true;
                inner.definitions_singleton = false;
                if let Some(body) = node.child_by_field_name("body") { self.ruby(body, file, &inner); }
                return;
            }
            "singleton_class" => {
                let Some(value) = node.child_by_field_name("value") else { return };
                let mut inner = context.clone();
                if value.kind() != "self" { inner.class = None; }
                inner.singleton = true;
                inner.definitions_singleton = true;
                if let Some(body) = node.child_by_field_name("body") { self.ruby(body, file, &inner); }
                return;
            }
            "method" | "singleton_method" => {
                let Some(name) = node.child_by_field_name("name") else { return };
                let name = self.text(file, name).to_owned();
                let singleton_method = node.kind() == "singleton_method";
                let singleton = singleton_method || context.definitions_singleton;
                let mut owner = context.class.clone();
                if singleton_method {
                    if let Some(object) = node.child_by_field_name("object") {
                        if object.kind() != "self" {
                            owner = if matches!(object.kind(), "constant" | "scope_resolution") {
                                self.ruby_constant(self.text(file, object), &context.nesting)
                            } else { None };
                        }
                    }
                }
                let label = owner.as_ref().map(|owner| format!("{owner}{}{name}", if singleton { "." } else { "#" })).unwrap_or_else(|| name.clone());
                let id = self.symbol(file, label, node.start_position().row + 1);
                if let Some(owner) = &owner {
                    self.ruby_methods.entry((owner.clone(), singleton, name)).or_default().push(id);
                } else if !singleton && context.class.is_none() {
                    self.ruby_top_methods.entry((file, name)).or_default().push(id);
                }
                let mut inner = context.clone();
                inner.caller = id;
                inner.scope = self.scope(None, true);
                inner.class = owner;
                inner.singleton = singleton;
                inner.definitions_singleton = false;
                if let Some(parameters) = node.child_by_field_name("parameters") {
                    self.ruby_parameters(parameters, file, &inner);
                }
                if let Some(body) = node.child_by_field_name("body") { self.ruby(body, file, &inner); }
                return;
            }
            "call" => {
                if let Some(method) = node.child_by_field_name("method") {
                    let receiver = node.child_by_field_name("receiver").map(|n| self.ruby_receiver(n, file)).unwrap_or(RubyReceiver::Implicit);
                    self.ruby_calls.push(RubyCall { file, context: context.clone(), receiver, method: self.text(file, method).into(), bare: false, site: self.call_site(file, method) });
                }
                for field in ["receiver", "arguments", "block"] {
                    if let Some(child) = node.child_by_field_name(field) { self.ruby(child, file, context); }
                }
                return;
            }
            "identifier" => {
                self.ruby_calls.push(RubyCall { file, context: context.clone(), receiver: RubyReceiver::Implicit, method: self.text(file, node).into(), bare: true, site: self.call_site(file, node) });
                return;
            }
            "assignment" | "operator_assignment" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.ruby_assignment(left, file, context);
                    self.pattern(left, file, context.scope);
                }
                if let Some(right) = node.child_by_field_name("right") { self.ruby(right, file, context); }
                return;
            }
            "block" | "do_block" | "lambda" => {
                let mut inner = context.clone();
                inner.caller = self.symbol(file, format!("<block@{}>", node.start_position().row + 1), node.start_position().row + 1);
                inner.scope = self.scope(Some(context.scope), true);
                if let Some(parameters) = node.child_by_field_name("parameters") { self.pattern(parameters, file, inner.scope); }
                if let Some(body) = node.child_by_field_name("body") { self.ruby(body, file, &inner); }
                return;
            }
            "scope_resolution" | "constant" | "symbol" | "simple_symbol" | "method_parameters" | "block_parameters" | "string" | "regex" => return,
            _ => {}
        }
        for child in named(node) { self.ruby(child, file, context); }
    }

    fn ruby_parameters(&mut self, node: Node<'_>, file: usize, context: &RubyContext) {
        match node.kind() {
            "identifier" => self.bind(context.scope, self.text(file, node).into(), Binding::Unknown),
            "optional_parameter" | "keyword_parameter" => {
                if let Some(name) = node.child_by_field_name("name") { self.pattern(name, file, context.scope); }
                if let Some(value) = node.child_by_field_name("value") { self.ruby(value, file, context); }
            }
            _ => for child in named(node) { self.ruby_parameters(child, file, context); },
        }
    }

    fn ruby_receiver(&self, node: Node<'_>, file: usize) -> RubyReceiver {
        match node.kind() {
            "self" => RubyReceiver::SelfValue,
            "constant" | "scope_resolution" => RubyReceiver::Constant(self.text(file, node).into()),
            "call" => {
                if node.child_by_field_name("method").is_some_and(|n| self.text(file, n) == "new") {
                    if let Some(receiver) = node.child_by_field_name("receiver").filter(|n| matches!(n.kind(), "constant" | "scope_resolution")) {
                        return RubyReceiver::Instance(self.text(file, receiver).into());
                    }
                }
                RubyReceiver::Unknown
            }
            _ => RubyReceiver::Unknown,
        }
    }

    fn ruby_constant(&self, raw: &str, nesting: &[String]) -> Option<String> {
        let absolute = raw.starts_with("::");
        let raw = raw.trim_start_matches("::");
        let first = raw.split("::").next()?;
        let prefix = if absolute { first.to_owned() } else {
            nesting.iter().map(|parent| format!("{parent}::{first}")).find(|candidate| self.ruby_constants.contains(candidate)).unwrap_or_else(|| first.to_owned())
        };
        let suffix = &raw[first.len()..];
        let qualified = format!("{prefix}{suffix}");
        let mut current = String::new();
        for part in qualified.split("::") {
            if !current.is_empty() { current.push_str("::"); }
            current.push_str(part);
            if self.ruby_assigned_constants.contains(&current) { return None; }
        }
        self.ruby_classes.contains(&qualified).then_some(qualified)
    }

    fn ruby_assignment(&mut self, node: Node<'_>, file: usize, context: &RubyContext) {
        if matches!(node.kind(), "constant" | "scope_resolution") {
            let raw = self.text(file, node);
            if node.kind() == "scope_resolution" {
                self.ruby_qualified_assignments.push((raw.into(), context.nesting.clone()));
                return;
            }
            let qualified = if raw.starts_with("::") { raw.trim_start_matches("::").into() }
                else if let Some(parent) = context.nesting.first() { format!("{parent}::{raw}") } else { raw.into() };
            self.ruby_constants.insert(qualified.clone());
            self.ruby_assigned_constants.insert(qualified);
        } else if matches!(node.kind(), "left_assignment_list" | "destructured_left_assignment" | "rest_assignment") {
            for child in named(node) { self.ruby_assignment(child, file, context); }
        }
    }

    fn ruby_target(&self, call: &RubyCall) -> Option<usize> {
        let implicit = matches!(call.receiver, RubyReceiver::Implicit | RubyReceiver::SelfValue);
        if call.bare && self.ruby_local(call.context.scope, &call.method) { return None; }
        let (owner, singleton) = match &call.receiver {
            RubyReceiver::Implicit | RubyReceiver::SelfValue => (call.context.class.clone(), call.context.singleton),
            RubyReceiver::Constant(raw) => (self.ruby_constant(raw, &call.context.nesting), true),
            RubyReceiver::Instance(raw) => {
                let owner = self.ruby_constant(raw, &call.context.nesting)?;
                if self.ruby_inherited_classes.contains(&owner) || self.ruby_methods.contains_key(&(owner.clone(), true, "new".into())) { return None; }
                (Some(owner), false)
            }
            RubyReceiver::Unknown => return None,
        };
        if let Some(owner) = owner {
            let (singleton, method) = if singleton && call.method == "new" && !self.ruby_methods.contains_key(&(owner.clone(), true, "new".into())) {
                if self.ruby_inherited_classes.contains(&owner) { return None; }
                (false, "initialize")
            } else { (singleton, call.method.as_str()) };
            return self.ruby_methods.get(&(owner, singleton, method.into())).filter(|ids| ids.len() == 1).map(|ids| ids[0]);
        }
        if implicit && call.context.class.is_none() {
            return self.ruby_top_methods.get(&(call.file, call.method.clone())).filter(|ids| ids.len() == 1).map(|ids| ids[0]);
        }
        None
    }

    fn ruby_local(&self, mut scope: usize, name: &str) -> bool {
        loop {
            if self.scopes[scope].bindings.contains_key(name) { return true; }
            let Some(parent) = self.scopes[scope].parent else { return false };
            scope = parent;
        }
    }

    fn finish(mut self) -> Graph {
        for (raw, nesting) in self.ruby_qualified_assignments.clone() {
            let Some((namespace, name)) = raw.rsplit_once("::") else { continue };
            let owner = if namespace.is_empty() { Some(String::new()) } else { self.ruby_constant(namespace, &nesting) };
            if let Some(owner) = owner {
                let qualified = if owner.is_empty() { name.to_owned() } else { format!("{owner}::{name}") };
                self.ruby_constants.insert(qualified.clone());
                self.ruby_assigned_constants.insert(qualified);
            }
        }
        for (mut scope, name) in self.assignments.clone() {
            loop {
                if let Some(value) = self.scopes[scope].bindings.get_mut(&name) { *value = Binding::Unknown; break; }
                let Some(parent) = self.scopes[scope].parent else { break };
                scope = parent;
            }
        }
        for (scope, context, object, name) in self.member_assignments.clone() {
            let target = match &object {
                Expr::This => context,
                Expr::Member(constructor, property) if property == "prototype" => match self.js_value(constructor, scope, context) { Binding::Class(id) => Some((id, false)), _ => None },
                _ => match self.js_value(&object, scope, context) { Binding::Class(id) => Some((id, true)), _ => None },
            };
            if let Some((class, singleton)) = target {
                self.classes[class].methods.insert((singleton, name), None);
            }
        }
        let mut links = Vec::new();
        for call in &self.js_calls {
            let target = self.js_value(&call.target, call.scope, call.class);
            let target = if call.constructor {
                match target { Binding::Class(class) => self.js_method(class, false, "constructor"), _ => None }
            } else { match target { Binding::Callable(id) => Some(id), _ => None } };
            if let Some(target) = target { links.push((call.caller, target, call.site)); }
            else { self.graph.unresolved_sites += 1; }
        }
        for call in &self.ruby_calls {
            if let Some(target) = self.ruby_target(call) { links.push((call.context.caller, target, call.site)); }
            else if !call.bare || !self.ruby_local(call.context.scope, &call.method) { self.graph.unresolved_sites += 1; }
        }
        self.graph.outgoing = vec![Vec::new(); self.graph.symbols.len()];
        self.graph.incoming = vec![Vec::new(); self.graph.symbols.len()];
        let mut unique = HashSet::new();
        for (caller, callee, site) in links {
            let CallSite { line, start_column, end_column } = site;
            if !unique.insert((caller, callee, line, start_column, end_column)) { continue; }
            let id = self.graph.edges.len();
            self.graph.edges.push(CallEdge { caller: self.graph.symbols[caller].clone(), callee: self.graph.symbols[callee].clone(), line, start_column, end_column });
            self.graph.endpoints.push((caller, callee));
            self.graph.outgoing[caller].push(id);
            self.graph.incoming[callee].push(id);
        }
        self.graph
    }
}

fn named(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn token(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|child| child.kind() == kind);
    found
}

fn literal(text: &str) -> Option<String> {
    let quote = text.as_bytes().first()?;
    if (*quote == b'\'' || *quote == b'"') && text.as_bytes().last() == Some(quote) {
        let value = &text[1..text.len() - 1];
        if !value.contains('\\') { return Some(value.into()); }
    }
    None
}

fn normalize(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => { if parts.last().is_some_and(|part| *part != "..") { parts.pop(); } else { parts.push(part); } }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(files: &[(&str, &str)]) -> Graph {
        Graph::build(&files.iter().map(|(path, text)| SourceFile { path: (*path).into(), text: (*text).into() }).collect::<Vec<_>>(), None)
    }

    #[test]
    fn separate_calls_to_the_same_method_on_one_line_keep_each_occurrence() {
        let js = graph(&[
            ("a.js", "import {run} from './b'; export function start() { run(run()); run(); }"),
            ("b.js", "export function run() {}"),
        ]);
        assert_eq!(js.parse_errors, 0);
        assert_eq!(js.edges.len(), 3);
        let ruby = graph(&[
            ("a.rb", "class A\n def self.start; B.run(B.run); B.run; end\nend"),
            ("b.rb", "class B\n def self.run; end\nend"),
        ]);
        assert_eq!(ruby.parse_errors, 0);
        assert_eq!(ruby.edges.len(), 3);
        assert_eq!(js.related("a.js", Direction::Callees, 1).call_sites.len(), 3);
        assert_eq!(ruby.related("a.rb", Direction::Callees, 1).call_sites.len(), 3);
    }

    fn assert_call_names(graph: &Graph, file: &str, source: &str, expected: &[(usize, &str)]) {
        let lines: Vec<_> = source.lines().map(|line| line.replace('\t', "    ")).collect();
        let mut actual: Vec<_> = graph.edges.iter().filter(|edge| edge.caller.file == file).map(|edge| {
            let name: String = lines[edge.line - 1].chars().skip(edge.start_column).take(edge.end_column - edge.start_column).collect();
            (edge.line, edge.start_column, edge.end_column, name)
        }).collect();
        actual.sort();
        let mut positions = HashMap::<usize, usize>::new();
        let expected: Vec<_> = expected.iter().map(|&(line, name)| {
            let text = &lines[line - 1];
            let next = positions.entry(line).or_default();
            let byte = *next + text[*next..].find(name).unwrap();
            *next = byte + name.len();
            let start = text[..byte].chars().count();
            (line, start, start + name.chars().count(), name.to_owned())
        }).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn javascript_call_ranges_select_names_in_rendered_unicode_and_multiline_source() {
        let source = "import {run, Worker} from './b'; import * as api from './b';\nexport function start() {\n\tconst café = '🙂界'; run(run()); run?.(\n\t\t'arg'\n\t);\n\tapi?.run?.(); new api.Worker(\n\t); new Worker().go();\n\tapi\n\t .run();\n}";
        let graph = graph(&[("a.js", source), ("b.js", "export function run() {} export class Worker { constructor() {} go() {} }")]);
        assert_eq!(graph.parse_errors, 0);
        assert_call_names(&graph, "a.js", source, &[(3, "run"), (3, "run"), (3, "run"), (6, "run"), (6, "Worker"), (7, "Worker"), (7, "go"), (9, "run")]);
    }

    #[test]
    fn typescript_call_ranges_unwrap_function_expressions_without_including_wrappers() {
        let source = "import {run} from './b';\nexport function start() {\n\tconst café = '🙂'; (run)(); run!(); (run as Function)(); run<string>();\n}";
        let graph = graph(&[("a.ts", source), ("b.ts", "export function run<T>() {}")]);
        assert_eq!(graph.parse_errors, 0);
        assert_call_names(&graph, "a.ts", source, &[(3, "run"), (3, "run"), (3, "run"), (3, "run")]);
    }

    #[test]
    fn ruby_call_ranges_select_methods_and_bare_names_after_unicode_and_tabs() {
        let source = "class A\n def self.start\n\t'🙂界'; B.run(B.run); B.run(\n\t 1\n\t); B.new.go\n\tB\n\t .run\n\thelper; helper()\n end\n def self.helper; end\nend";
        let graph = graph(&[("a.rb", source), ("b.rb", "class B\n def self.run(*args); end\n def initialize; end\n def go; end\nend")]);
        assert_eq!(graph.parse_errors, 0);
        assert_call_names(&graph, "a.rb", source, &[(3, "run"), (3, "run"), (3, "run"), (5, "new"), (5, "go"), (7, "run"), (8, "helper"), (8, "helper")]);
    }

    #[test]
    fn deeper_symbols_add_call_sites_when_the_reached_file_set_stays_the_same() {
        let graph = graph(&[
            ("a.js", "import {entry} from './b'; import {leaf} from './c'; export function start() { entry(); leaf(); }"),
            ("b.js", "import {middle} from './c'; import {unrelated} from './d'; export function entry() { middle(); } export function other() { unrelated(); }"),
            ("c.js", "import {entry} from './b'; export function middle() { leaf(); } export function leaf() { entry(); }"),
            ("d.js", "export function unrelated() {}"),
        ]);
        let shallow = graph.related("a.js", Direction::Callees, 1);
        let deep = graph.related("a.js", Direction::Callees, 8);
        assert_eq!(shallow.paths, HashSet::from(["a.js".into(), "b.js".into(), "c.js".into()]));
        assert_eq!(deep.paths, shallow.paths);
        assert_eq!((shallow.reached_symbols, deep.reached_symbols), (2, 3));
        assert_eq!((shallow.call_sites.len(), deep.call_sites.len()), (2, 5));
        assert!(!deep.call_sites.iter().any(|edge| edge.caller.name == "other" || edge.callee.file == "d.js"));
        assert_eq!(deep.call_sites.iter().filter(|edge| edge.caller.name == "leaf" && edge.callee.name == "entry").count(), 1);
        assert_eq!(deep.witnesses, shallow.witnesses);
        let empty = graph.related("a.js", Direction::Callees, 0);
        assert!(empty.call_sites.is_empty());
        assert_eq!(empty.reached_symbols, 0);
    }

    #[test]
    fn caller_traversal_preserves_repeated_and_cyclic_call_sites_once() {
        let graph = graph(&[
            ("a.js", "import {b} from './b'; export function a() { b(); b(); }"),
            ("b.js", "import {a} from './a'; export function b() { a(); }"),
        ]);
        let shallow = graph.related("b.js", Direction::Callers, 1);
        assert_eq!(shallow.call_sites.len(), 2);
        assert_eq!(shallow.reached_symbols, 1);
        let deep = graph.related("b.js", Direction::Callers, 8);
        assert_eq!(deep.call_sites.len(), 3);
        assert_eq!(deep.reached_symbols, 1);
        assert_eq!(deep.paths, shallow.paths);
    }

    #[test]
    fn follows_method_calls_and_preserves_evidence_in_both_directions() {
        let graph = graph(&[
            ("a.ts", "import {b as invoke} from './b';\nexport function a() { invoke(); }"),
            ("b.ts", "import c from './c'; export function b() { c(); }"),
            ("c.ts", "export default function c() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        let related = graph.related("a.ts", Direction::Callees, 2);
        assert_eq!(related.direct, HashSet::from(["b.ts".into()]));
        assert_eq!(related.indirect, HashSet::from(["c.ts".into()]));
        let chain = &related.witnesses["c.ts"];
        assert_eq!(chain.len(), 2);
        assert_eq!((&*chain[0].caller.name, &*chain[0].callee.name, chain[0].line), ("a", "b", 2));
        assert_eq!(chain[0].callee, chain[1].caller);
        assert!(!graph.related("a.ts", Direction::Callees, 1).paths.contains("c.ts"));
        let callers = graph.related("c.ts", Direction::Callers, 2);
        assert!(callers.direct.contains("b.ts"));
        assert!(callers.indirect.contains("a.ts"));
        assert_eq!(callers.witnesses["a.ts"], *chain);
    }

    #[test]
    fn unrelated_methods_in_an_intermediate_file_do_not_create_a_path() {
        let graph = graph(&[
            ("a.js", "import {b} from './b'; export function a() { b(); }"),
            ("b.js", "import {c} from './c'; export function b() {} export function unrelated() { c(); }"),
            ("c.js", "export function c() {}"),
        ]);
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.related("a.js", Direction::Callees, MAX_DEPTH).paths, HashSet::from(["a.js".into(), "b.js".into()]));
        assert_eq!(graph.related("c.js", Direction::Callers, MAX_DEPTH).paths, HashSet::from(["b.js".into(), "c.js".into()]));
    }

    #[test]
    fn imports_alone_unknown_receivers_and_shadowed_bindings_do_not_create_edges() {
        let graph = graph(&[
            ("a.ts", "import {run} from './b'; function a(run: () => void, obj: any) { run(); obj.run(); } function b() { const run = external; run(); }"),
            ("b.ts", "export function run() {}"),
            ("unrelated.ts", "export function run() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert_eq!(graph.edges.len(), 0);
        assert_eq!(graph.unresolved_sites, 3);
    }

    #[test]
    fn namespace_default_and_reexport_aliases_resolve_without_import_edges() {
        let graph = graph(&[
            ("a.tsx", "import * as api from './barrel'; import invoke from './b'; export const a = () => { api.renamed(); invoke(); };"),
            ("barrel.ts", "export {b as renamed} from './b';"),
            ("b.ts", "export function b() {} export default b;"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        let result = graph.related("a.tsx", Direction::Callees, 4);
        assert_eq!(result.direct, HashSet::from(["b.ts".into()]));
        assert!(!result.paths.contains("barrel.ts"));
    }

    #[test]
    fn cycles_terminate_and_use_the_shortest_file_distance() {
        let graph = graph(&[
            ("a.ts", "import {b} from './b'; export function a() { b(); }"),
            ("b.ts", "import {a} from './a'; import {c} from './c'; export function b() { a(); c(); }"),
            ("c.ts", "export function c() {}"),
        ]);
        assert_eq!(graph.related("a.ts", Direction::Callees, 100).paths.len(), 3);
        assert_eq!(graph.related("a.ts", Direction::Callees, 0).paths, HashSet::from(["a.ts".into()]));
    }

    #[test]
    fn explicit_static_and_constructed_instance_calls_resolve() {
        let graph = graph(&[
            ("a.ts", "import Worker from './b'; export function a() { Worker.build(); new Worker().run(); }"),
            ("b.ts", "import {c} from './c'; export default class Worker { constructor() {} static build() {} run() { c(); } }"),
            ("c.ts", "export function c() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.ts", Direction::Callees, 2).indirect.contains("c.ts"));
        assert_eq!(graph.edges.len(), 4);
    }

    #[test]
    fn ruby_namespaces_and_self_calls_form_symbol_paths() {
        let graph = graph(&[
            ("a.rb", "module App\n class A\n def self.a\n B.b\n end\n end\nend"),
            ("b.rb", "module App\n class B\n def self.b\n self.helper\n end\n def self.helper\n ::App::C.c\n end\n end\nend"),
            ("c.rb", "module App\n class C\n def self.c; end\n end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        let result = graph.related("a.rb", Direction::Callees, 3);
        assert!(result.direct.contains("b.rb"));
        assert!(result.indirect.contains("c.rb"));
        let chain = &result.witnesses["c.rb"];
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].callee, chain[1].caller);
        assert_eq!(chain[1].callee, chain[2].caller);
    }

    #[test]
    fn ruby_unrelated_methods_and_unknown_instances_do_not_form_paths() {
        let graph = graph(&[
            ("a.rb", "class A\n def self.a(obj)\n B.b\n obj.run\n end\nend"),
            ("b.rb", "class B\n def self.b; end\n def self.unrelated\n C.c\n end\nend"),
            ("c.rb", "class C\n def self.c; end\n def run; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert_eq!(graph.related("a.rb", Direction::Callees, 8).paths, HashSet::from(["a.rb".into(), "b.rb".into()]));
    }

    #[test]
    fn ruby_explicit_instances_and_duplicate_definitions_are_conservative() {
        let graph = graph(&[
            ("a.rb", "class A\n def self.a\n B.new.run\n end\nend"),
            ("b.rb", "class B\n def initialize; end\n def run; C.c; end\nend"),
            ("c.rb", "class C\n def self.c; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.rb", Direction::Callees, 2).indirect.contains("c.rb"));
        let duplicate = super::tests::graph(&[("a.rb", "class A\n def self.a; B.b; end\nend"), ("b.rb", "class B\n def self.b; end\nend"), ("other.rb", "class B\n def self.b; end\nend")]);
        assert_eq!(duplicate.edges.len(), 0);
    }

    #[test]
    fn base_url_is_explicit_and_external_packages_stay_unresolved() {
        let sources = [SourceFile { path: "webpack/a.ts".into(), text: "import {b} from 'lib/b'; import {missing} from 'external-package'; export function a() { b(); missing(); }".into() }, SourceFile { path: "webpack/lib/b.ts".into(), text: "export function b() {}".into() }];
        assert_eq!(Graph::build(&sources, None).edges.len(), 0);
        let graph = Graph::build(&sources, Some("webpack"));
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.unresolved_sites, 1);
    }

    #[test]
    fn parse_errors_and_unsupported_files_cannot_supply_symbols() {
        let graph = graph(&[("a.ts", "import {b} from './b'; export function a() { b(); }"), ("b.ts", "export function b( {"), ("c.txt", "function pretend() {}")]);
        assert_eq!(graph.analyzed_files, 2);
        assert_eq!(graph.parse_errors, 1);
        assert_eq!(graph.edges.len(), 0);
    }

    #[test]
    fn ruby_shadowing_constants_do_not_fall_back_to_a_global_class() {
        let graph = graph(&[
            ("a.rb", "module App\n B = dynamic_class\n class Caller\n def self.a; B.b; end\n end\nend"),
            ("b.rb", "class B\n def self.b; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.rb", Direction::Callees, 8).direct.is_empty());
    }

    #[test]
    fn reassigned_functions_and_class_methods_are_unresolved() {
        let graph = graph(&[
            ("a.ts", "import {b, Worker} from './b'; export function a() { b(); Worker.run(); new Worker().go(); }"),
            ("b.ts", "export function b() {} b = external; export class Worker { static run() {} go() {} } Worker.run = external; Worker.prototype.go = external;"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert_eq!(graph.edges.len(), 0);
    }

    #[test]
    fn nested_object_methods_do_not_become_class_methods() {
        let graph = graph(&[
            ("a.ts", "import {Worker} from './b'; export function a() { Worker.unrelated(); }"),
            ("b.ts", "import {c} from './c'; export class Worker { static build() { const object = { unrelated() { c(); } }; } }"),
            ("c.ts", "export function c() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.ts", Direction::Callees, 8).direct.is_empty());
    }

    #[test]
    fn class_fields_and_accessors_that_override_methods_do_not_create_paths() {
        for replacement in ["run = external;", "get run() { return external; }", "['run'] = external;"] {
            let graph = graph(&[
                ("a.ts", "import {Worker} from './b'; export function a() { new Worker().run(); }"),
                ("b.ts", &format!("import {{c}} from './c'; export class Worker {{ run() {{ c(); }} {replacement} }}")),
                ("c.ts", "export function c() {}"),
            ]);
            assert_eq!(graph.parse_errors, 0);
            assert!(graph.related("a.ts", Direction::Callees, 8).direct.is_empty(), "{replacement}");
        }
    }

    #[test]
    fn destructuring_reassignment_does_not_keep_the_imported_function_target() {
        let graph = graph(&[
            ("a.ts", "import {b} from './b'; function a() { ({b} = external); b(); }"),
            ("b.ts", "export function b() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.ts", Direction::Callees, 8).direct.is_empty());
    }

    #[test]
    fn loop_binding_shadows_an_import_inside_the_loop_only() {
        let graph = graph(&[
            ("a.ts", "import {b} from './b'; export function a(items: any[]) { for (let b of items) { b(); } } export function other() { b(); }"),
            ("b.ts", "export function b() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].caller.name, "other");
        assert_eq!(graph.unresolved_sites, 1);
    }

    #[test]
    fn constructors_with_unknown_return_values_do_not_imply_an_instance_target() {
        let js = graph(&[
            ("a.ts", "import {Worker} from './b'; export function a() { new Worker().run(); }"),
            ("b.ts", "import {c} from './c'; export class Worker { constructor() { return external; } run() { c(); } }"),
            ("c.ts", "export function c() {}"),
        ]);
        assert_eq!(js.parse_errors, 0);
        assert!(!js.related("a.ts", Direction::Callees, 8).paths.contains("c.ts"));
        let ruby = graph(&[
            ("a.rb", "class A\n def self.a; B.new.run; end\nend"),
            ("b.rb", "class B\n def self.new; external; end\n def run; C.c; end\nend"),
            ("c.rb", "class C\n def self.c; end\nend"),
        ]);
        assert_eq!(ruby.parse_errors, 0);
        assert!(!ruby.related("a.rb", Direction::Callees, 8).paths.contains("c.rb"));
    }

    #[test]
    fn javascript_class_fields_override_prototype_methods() {
        let graph = graph(&[
            ("a.js", "import Worker from './b'; export function a() { new Worker().run(); }"),
            ("b.js", "import {c} from './c'; export default class Worker { run() { c(); } run = () => {}; }"),
            ("c.js", "export function c() {}"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(!graph.related("a.js", Direction::Callees, 8).paths.contains("c.js"));
    }

    #[test]
    fn ruby_qualified_constant_assignments_resolve_the_namespace_before_shadowing() {
        let graph = graph(&[
            ("a.rb", "module App\nend\nmodule Other\n App::B = Object.new\nend\nclass Entry\n def self.a; App::B.b; end\nend"),
            ("b.rb", "module App\n class B\n def self.b; C.c; end\n end\nend"),
            ("c.rb", "class C\n def self.c; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("a.rb", Direction::Callees, 8).direct.is_empty());
    }

    #[test]
    fn nested_qualified_declarations_cannot_invent_a_namespace() {
        let graph = graph(&[
            ("entry.rb", "class Entry\n def self.start; ::Other::B::Nested.go; end\nend"),
            ("nested.rb", "class B; end\nmodule Other\n class B::Nested\n def self.go; C.c; end\n end\nend"),
            ("c.rb", "class C\n def self.c; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(graph.related("entry.rb", Direction::Callees, 8).direct.is_empty());
    }

    #[test]
    fn unknown_singleton_methods_do_not_become_file_local_functions() {
        let graph = graph(&[
            ("a.rb", "def start; hidden; end\nclass << object\n def hidden; B.b; end\nend"),
            ("b.rb", "class B\n def self.b; end\nend"),
        ]);
        assert_eq!(graph.parse_errors, 0);
        assert!(!graph.edges.iter().any(|edge| edge.caller.name == "start" && edge.callee.name == "hidden"));
    }
}
