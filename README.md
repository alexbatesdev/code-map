# megamap

## Web viewer

The browser viewer uses WebGL2 for the treemap and a bounded text overlay for
readable source. A local Rust worker reuses the desktop scanner, filters,
relationship graph, evidence handling and syntax colors. Analysis and source browsing run locally. Normal checkout browsing requires no
external service or running Rails app.

Build the headless workers with Rust, then run with Node.js 18 or newer:

```sh
cargo build --release --locked --no-default-features --bin megamap-data --bin megamap-capture
node web/server.mjs --repo /path/to/project --port 4852
```

Open `http://127.0.0.1:4852/megamap/`. Use the filesystem sidebar, search,
filters, history colors and relationship traces as in the desktop viewer.
Click a colored call in readable map source or the inspector to jump to its
resolved definition. Depth changes preserve the camera. Every toolbar control
has hover help; a pulsing Triforce indicates pending work. **Refresh / rescan**
rebuilds the file list and index, clearing traces and optional evidence while
preserving filters, camera and the selected path where possible.

**Snapshot** imports an explicitly selected local relationship JSON file.
**Static only** removes that optional data. Rails capture is disabled by default;
add `--allow-capture` only when you want **Capture Rails metadata** to run the
registered repository's development `bin/rails runner`. Failed or unavailable
captures fall back to source analysis. Use exported snapshots when the app runs
in a container or another Ruby environment. See the evidence instructions below.

A host such as the software factory can mount the same viewer without a second
HTTP server:

```js
import { createMegamapHandler } from './web/server.mjs';
const megamap = await createMegamapHandler({
  prefix: '/megamap',
  projects: [{ id: 'my-repo', label: 'My repository', root: '/trusted/checkout' }],
  binary: '/absolute/path/to/megamap-data',
  allowCapture: false,
});
// In the existing loopback HTTP request handler:
if (await megamap.handle(req, res)) return;
// During host shutdown:
await megamap.close();
```

Project roots are registered by the host; browser requests cannot choose an
arbitrary path. Links can include `?project=my-repo&file=app/models/company.rb&line=42`.
The host must serve plain HTTP on a loopback address with its actual port in the
Host header. API requests require same-origin access and the viewer's request
token. The module starts workers on demand, retains at most four, bounds queued
jobs, and cancels owned processes when closed. It has no npm dependencies.

Local worker hosting supports macOS and Linux; only macOS has been tested.
The browser needs WebGL2 for the map; without it, the file tree and source
inspector remain available. Source previews are limited to 2 MiB per file, the
browser source cache to 24 files / 16 MiB, and snapshot uploads to 32 MiB.
Large indexing runs can take time; status remains responsive. Analysis is still
incomplete, with inferred links and unresolved/parse/unread counts labeled.
The native desktop app remains available and builds with the default features.

## Pull request changes and potential impact

Enter a GitHub PR number and click **Load PR**, or open a factory card's
**changes** link. The service uses the PR's exact merge-base and head commits,
without changing your local branch, index or uncommitted files. It reads Git blobs
into private snapshots and never checks out or runs code from the PR. The installed
`gh` CLI supplies GitHub access; the registered repository's GitHub origin is used.
A host can explicitly supply `repository: 'owner/name'` on its project configuration.

**Full context** dims unchanged files. **Changed files only** narrows the view to
edits; **Potential impact** includes possible callers of changed methods. Choose
one **Changed method** or keep **All changes**, then increase **Depth** to follow
indirect callers. Impact starts from changed method bodies and signatures, rather
than every method sharing their file. Edits outside attributable methods use an
explicitly labeled conservative file-level fallback.

Switch **Before** / **After** without moving the camera or rearranging file boxes.
Added, modified, deleted and renamed files have badges and outlines; relationship
colors stay available. The inspector shows changed-line excerpts, possible caller
chains and added/removed method relationships. Click a chain endpoint to open its
correct revision and definition. Deleted methods retain their before-side callers.
The map uses the larger of each file's two source layouts so shorter-side blank
space and absent-side boxes remain stable.

Potential impact is source analysis and labeled inference, not runtime proof.
**Analysis coverage** reports parse failures, unresolved calls and omitted data.
Anonymous/location-based and ambiguous duplicate method identities are omitted
from relationship-delta matching, with counts, rather than inventing changes from
line shifts. Unsupported languages still show file/line changes without claiming
complete caller coverage. No Rails app is booted in PR mode; imported/live app
facts are unavailable for these immutable revisions.

**Refresh / rescan** in PR mode explicitly reloads current PR metadata into a new
pinned comparison. **Exit PR** returns to the working checkout. Snapshot sessions
are local, limited and may expire after other projects/comparisons are opened;
reload the PR to restore one. Source previews retain the 2 MiB limit, diff excerpts
are bounded to 400 lines per file, regular snapshots to 8 MiB per file / 512 MiB
total, and fetched objects to a sampled 512 MiB abort threshold. Symlinks,
submodules and oversized files have explicit coverage warnings. Failed loads
leave the working checkout untouched.

Deep links use `?project=ID&pr=NUMBER`, optionally with `&file=relative/path&line=N`.
Node service regressions run with `node --test web/*.test.mjs`; the Rust release
suite covers the shared graph, comparison model and protocol.

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
and inherited/commented tsconfig files are not resolved. Reopen the repository (or use **Refresh / rescan** in the web viewer)
to rebuild the index after source changes. The index is built only on demand,
from non-ignored supported source files in the original scan.

The desktop setup instructions below still apply. Build and test the fork with
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
and retain source analysis. Reopen the project, or use **Refresh / rescan** in the web viewer, when files are added or removed.

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

The desktop viewer requires macOS 11 or newer, Rust and Apple's command-line developer
tools (`xcrun clang`). Clone this repository alone; no sibling checkout or
bundled font/resource directory is required. The build uses the installed Apple
SDK and compiles the Metal shaders at runtime. No additional graphics crates
are downloaded.

```sh
git clone --branch alex/web-viewer https://github.com/alexbatesdev/code-map.git
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
| `src/lib.rs`, `src/web_project.rs`, `src/bin/megamap-data.rs` | Shared analysis and headless JSON worker |
| `web/server.mjs`, `web/worker-host.mjs` | Embeddable loopback service and owned worker lifecycle |
| `web/viewer.mjs`, `web/renderer.mjs` | Browser controls, virtualized tree and WebGL2 map |
| `src/main.rs`, `src/ui.rs` | Event loop, custom toolbar controls and inspector |
| `src/scan.rs` | Reads the project from disk, asks git which files are ignored, summarizes every line |
| `src/model.rs` | Treemap layout and colors |
| `src/history.rs` | Reads `git log` for the heatmap modes |
| `src/map.rs`, `src/canvas.rs` | Map controller, drawing, zoom, picking and search |
| `native/megamap.m`, `src/platform.rs` | AppKit window/input, Metal batches and CoreText glyph atlas |
| `src/editor.rs` | Text selection, editing and composition |
| `src/file_tree.rs` | Cached filesystem rows, folder expansion and selected-path reveal |

The window currently supports macOS only. The `scan` binary and analysis modules
remain portable; the optional web viewer is described above.

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
