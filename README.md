# megamap

## Explorer fork

This fork adds file filtering, static call paths, and syntax highlighting to the
original map. The viewer uses a zoomable 2D treemap.

- **Filter:** space-separated terms are ANDed; `|` joins OR groups; `-term`
  excludes a path. Examples: `app/ | webpack/`, `app/ -controllers/`, `**/*.tsx`.
  Use **Hide specs / tests** to remove tests from every group. Filtering resizes
  the map and updates counts; search navigates within the visible selection.
  Invalid expressions keep the last valid filter and display an error.
- **Call paths:** select a Ruby, JavaScript or TypeScript file, then choose
  **Trace selected file**. **Calls from root** follows callees; **Callers of root**
  follows callers. Depth limits traversal to 1, 2, 3, 5 or 8 method calls. Deeper
  calls can stay inside files already visible, so file counts can remain the
  same. The status bar also reports reached methods and cross-file call sites.
  Gold marks the root; other files have stable colors. Resolved calls in the
  source use the same color as their destination file, including in the line
  minimaps. **Syntax colors** starts on and can be turned off for neutral source
  text while retaining call colors. Changing depth preserves the current view
  and readable source position; starting a new trace fits the map.
  Select a connected file for the method names and call-site lines that explain
  its connection. Select **All files** to leave the relationship view.
- **Jump to definition:** at readable source zoom, click a colored call name
  to fly to the resolved declaration. Its line is highlighted and the inspector
  shows the method and line number. The trace root, filters and depth stay put.
  Only resolved colored calls are links; dynamic or ambiguous calls remain plain.
- **Ruby receivers:** follows single source assignments and aliases, simple
  method returns, readers and compatible instance-variable assignments. Rails
  model inheritance enables finder/query and association inference, including
  labeled `current_company`-style helpers. The inspector shows inference reasons
  for calls, paths and clicked targets; the status counts inferred call sites.
  Generated Rails targets jump to the model, macro or schema annotation that
  supports the inference, rather than claiming a source method body exists.
- **Source colors:** the trace view highlights exact resolved method-call names
  at readable zoom. The inspector lists their source lines and columns. Incoming
  call sites are in the caller files; select one to see its calls into the root.
  Optional Ruby, JS/JSX and TS/TSX syntax colors use Tree-sitter. Parsing runs on a background worker and is
  cached. Unsupported languages and files above 2 MiB retain
  plain text. The map's existing column clipping still applies.
- **Loading:** a gold Triforce pulses in the bottom-right corner while background
  scans, history, call indexing, folder expansion or source loading are pending.
  It disappears when all pending work finishes.

Call paths traverse method/function symbols before grouping them into files.
If A calls `B.run`, and only the unrelated `B.other` calls C, A does not acquire
an indirect connection to C. Imports supply bindings; they are not call edges.

This is **static analysis**, not a recording of runtime execution. Source-bound
calls and labeled receiver/framework inferences become edges. Rails inferences
can be wrong when runtime behavior differs from the supported conventions.
Ambiguous writes, conflicting types and unsupported association contracts stay
unresolved. Dynamic Ruby dispatch, Rails callbacks/autoload behavior, reflection,
framework scheduling, custom inflections, interprocedural argument types, nested
qualified Ruby class declarations, HTTP/GraphQL boundaries and arbitrary receiver
types are not reconstructed. A missing edge
does not prove that code cannot execute. The status bar reports unresolved sites
and files rejected for parse errors. TypeScript relative imports and an explicit
`compilerOptions.baseUrl` in `tsconfig.json` are supported; custom path aliases
and inherited/commented tsconfig files are not resolved. Reopen the repository
to rebuild the index after source changes. The index is built only on demand,
from non-ignored supported source files in the original scan.

The original setup instructions below still apply. Build and test the fork with
`cargo build --release --bins --locked` and `cargo test --release --locked`.

Fly over a codebase as a zoomable GPU treemap. Based on
[Code Map](https://github.com/Peeter95/code-map).

- Every file is a box sized by its line count, grouped by folder. Zoom in far enough and the
  actual code appears inside the box.
- Respects `.gitignore` (the file list comes from `git ls-files`). Ignored entries are hidden
  by default. **Show ignored** reveals striped collapsed boxes, read only when clicked.
- Search: type to highlight matches, Enter flies to the next one, Escape clears.
- Git heatmap: color by "Recently changed" or "Most changed" instead of file type.

## Setup

You need Rust (https://rustup.rs) and a Makepad checkout **next to** this repo, because
`Cargo.toml` points at `../makepad`:

```sh
mkdir megamap && cd megamap
git clone https://github.com/makepad/makepad.git
git -C makepad checkout a4ea2536a4ab223fb31e0282be923191230f44fe   # dev branch, 2026-09-15
git clone --branch alex/ruby-receiver-inference https://github.com/alexbatesdev/code-map.git
```

Makepad's API moves fast, so stick to the pinned commit above. Newer commits may or may not
build.

## Run

```sh
cd code-map
cargo run --release -- /path/to/some/project
```

The first build compiles Makepad and takes a few minutes. Later builds are fast. Without a path
it maps the current folder. It works best on a git repository: the ignore rules then match
`git status` exactly and the heatmap has history to show. Outside git it falls back to a simple
ignore list.

Headless check, no window, just scan statistics:

```sh
cargo run --release --bin scan -- /path/to/some/project [ignored/path ...]
```

## Controls

| Input | Action |
| --- | --- |
| Scroll | Zoom |
| Drag | Pan |
| Click | Inspect a file or folder |
| Click a colored call | Jump to its definition |
| Double click | Fly to it |
| Enter in search | Next match |

## Code tour

| File | What it does |
| --- | --- |
| `src/main.rs` | App shell: window, toolbar and inspector written in Makepad's Splash DSL |
| `src/scan.rs` | Reads the project from disk, asks git which files are ignored, summarizes every line |
| `src/model.rs` | Treemap layout and colors |
| `src/history.rs` | Reads `git log` for the heatmap modes |
| `src/map_view.rs` | The custom `CodeMap` widget: drawing, zoom, picking, search |

Tested on macOS. Makepad also targets Windows, Linux and web, but this app has not been tried
there.
