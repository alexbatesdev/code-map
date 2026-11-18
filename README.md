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
- **Optional app evidence:** normal startup uses source analysis without starting
  Rails. Load an evidence JSON file, or explicitly choose **Capture Rails** to
  inspect this repository's development app. Capture runs in the background with
  a 45-second limit; missing apps, failed captures and invalid snapshots fall
  back to source analysis. **Static only** removes optional data. Evidence
  filters distinguish source, inferred, loaded-app-assisted and observed calls.
  Importing a new snapshot refreshes the indexed source text and clears old
  navigation evidence while preserving the current source viewport.

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

## Optional Rails metadata and observed calls

The viewer works without Rails. A loaded-app snapshot can reveal generated
methods, effective mixin method owners, actual associations and attributes.
Loaded-app-assisted links remain candidate calls; they are not execution proof.
**Capture Rails** explicitly runs `bin/rails runner` in development with Spring
disabled. It does not attach to or require an already running web server. App
dependencies must already be installed; megamap never installs them.
Capture uses the viewer's Ruby environment and the sibling `megamap-capture`
helper built by `cargo build --release --bins`. For an app managed by a container
or version manager, export in that environment and import the snapshot.

To export inside a different environment, such as a container, run from that
application's root:

```sh
MEGAMAP_EVIDENCE_MODE=snapshot MEGAMAP_EVIDENCE_OUT=/tmp/megamap-loaded.json \
  bin/rails runner /absolute/path/to/megamap/tools/runtime_evidence.rb
```

Load the resulting JSON using **Load snapshot**, or launch with
`--evidence=/absolute/path/to/snapshot.json`. Relative snapshot paths in the
viewer are relative to the mapped repository. Snapshots use relative source
paths and content fingerprints, so matching container/host checkouts work.
Changed, added, deleted or unavailable Ruby context files invalidate the capture
and retain source analysis. Reopen the project when files are added or removed.

To record a chosen local request or test, explicitly wrap that behavior in an
already loaded Ruby/Rails process:

```ruby
require "/absolute/path/to/megamap/tools/runtime_evidence"
MegamapEvidence.capture(root: Rails.root, output: "/tmp/megamap-observed.json", label: "chosen scenario") do
  # Run the selected development request or test here.
end
```

Requiring the helper alone is inert. Captures record method identities, source
locations and counts, without arguments or local values. Observations cover only
the exercised execution. C, external-framework and block frames are barriers;
callbacks or background-job causality are not collapsed into invented direct
calls. Collection limits and partial metadata warnings accompany the capture.
Ambiguous same-line call tokens remain unlinked. Test the helper with
`ruby tests/runtime_evidence_test.rb` in addition to the Rust suite.

Fly over a codebase as a zoomable GPU treemap. Based on
[Code Map](https://github.com/Peeter95/code-map).

- Every file is a box sized by its line count, grouped by folder. Zoom in far enough and the
  actual code appears inside the box.
- Respects `.gitignore` (the file list comes from `git ls-files`). Ignored entries are hidden
  by default. **Show ignored** reveals striped collapsed boxes, read only when clicked.
- Search: type to highlight matches, Enter flies to the next one, Escape clears.
- Git heatmap: color by "Recently changed" or "Most changed" instead of file type.

## Setup

The viewer requires macOS 11 or newer, Rust and Apple's command-line developer
tools (`xcrun clang`). Clone this repository alone; no sibling checkout or
bundled font/resource directory is required. The build uses the installed Apple
SDK and compiles the Metal shaders at runtime. No additional graphics crates
are downloaded.

```sh
git clone --branch alex/native-metal https://github.com/alexbatesdev/code-map.git
cd code-map
cargo build --release --bins --locked
```

## Run

```sh
cargo run --release -- /path/to/some/project
```

Without a path it maps the current folder. It works best on a git repository: the ignore rules then match
`git status` exactly and the heatmap has history to show. Outside git it falls back to a simple
ignore list.

Headless check, no window, just scan statistics:

```sh
cargo run --release --bin scan -- /path/to/some/project [ignored/path ...]
```

## Controls

Hover over a toolbar control for a short explanation. Help appears after half a
second and closes when you leave the control or interact with the app.

| Input | Action |
| --- | --- |
| Scroll over treemap | Zoom |
| Scroll over File tree | Scroll the file list (Shift for horizontal) |
| Drag | Pan |
| Click | Inspect a file or folder |
| Click a colored call | Jump to its definition |
| Double click | Fly to it |
| Enter in search | Next match |

Enable **File tree** to open the expandable filesystem sidebar. Its folders and
files share the treemap's filters, ignored-file toggle and relationship trace.
Click an item to select it in the map and inspector; double-click a file to zoom
to it, or a folder to expand/collapse it. Search and definition jumps reveal the
selected file in the sidebar. Folder expansion survives filtering and hiding it.

With the sidebar focused, Up/Down and Home/End select rows, Right/Left expand or
collapse folders, and Enter centers the selected item in the treemap. Wheel or
scrollbars move the list; Shift-wheel and horizontal trackpad motion scroll long
names sideways. Large folders draw only rows in the viewport.

## Code tour

| File | What it does |
| --- | --- |
| `src/main.rs`, `src/ui.rs` | Event loop, custom toolbar controls and inspector |
| `src/scan.rs` | Reads the project from disk, asks git which files are ignored, summarizes every line |
| `src/model.rs` | Treemap layout and colors |
| `src/history.rs` | Reads `git log` for the heatmap modes |
| `src/map.rs`, `src/canvas.rs` | Map controller, drawing, zoom, picking and search |
| `native/megamap.m`, `src/platform.rs` | AppKit window/input, Metal batches and CoreText glyph atlas |
| `src/editor.rs` | Text selection, editing and composition |
| `src/file_tree.rs` | Cached filesystem rows, folder expansion and selected-path reveal |

The window currently supports macOS only. The `scan` binary and analysis modules
remain portable; no Windows, Linux or web UI is provided.

Text is shaped with system fonts and drawn from a bounded GPU glyph atlas.
Rectangles and glyphs share a batched Metal instance stream. Frames render on
changes, animation and loading; the idle viewer does not continuously redraw.
For local verification, `--remote[=port]` enables a loopback-only input/state/PNG
endpoint; it is off by default. `--hidden` renders to an owned Metal target
without showing a window. `/stats` reports frame CPU/GPU timings and capacity
drops. Captures contain source text from the mapped repository.

Native input regressions (macOS, no window) run with
`./tools/test_native_input.sh`. Run the optional loopback lifecycle/large-response
check explicitly with `cargo test --release --bin megamap -- --ignored`.
