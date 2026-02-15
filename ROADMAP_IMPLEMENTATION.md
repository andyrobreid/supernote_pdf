# supernote_pdf Implementation Roadmap

This document expands the current README roadmap into an execution plan with implementation-level detail.
It uses `jya-dev/supernote-tool` as a reference baseline for format support and feature parity, while preserving `supernote_pdf`'s speed-first Rust design.

## Scope and Principles

- Primary product goal: fast and reliable `.note` to `.pdf` archival conversion.
- Keep defaults simple and safe; add advanced behavior as opt-in flags.
- Preserve current performance profile for raster conversion.
- Introduce structured modules before adding major features.

## Reference Baseline (supernote-tool)

Reference project inspected: `/tmp/supernote-tool-ref`

Relevant behaviors to mirror or adapt:
- Broader device support and parser flexibility (`supernotelib/parser.py`, `supernotelib/fileformat.py`)
- Optional vector PDF generation (`supernotelib/converter.py`)
- Link and keyword embedding in PDF (`supernotelib/converter.py`)
- Analyze/debug surfaces for metadata (`supernotelib/cmds/supernote_tool.py`)
- Parser policy (`strict` vs `loose`) for unknown signatures (`supernotelib/parser.py`)

What to keep different in `supernote_pdf`:
- Rust-native parallel decode/render/write path
- Minimal default UX focused on fast PDF export
- Predictable filesystem-safe directory conversion behavior

## Cross-Cutting Foundation Work (Do First)

Current state is mostly in `src/main.rs`. Before roadmap features, split into modules.

### Target architecture

- `src/cli.rs`: clap command/option definitions
- `src/parser/`:
  - `metadata.rs`: metadata blocks and key extraction
  - `device.rs`: device signature/equipment mapping and dimensions
  - `notebook.rs`: in-memory model for notebook/page/layer/link/tag
  - `rle.rs`: RATTA_RLE decode and validation
- `src/render/`:
  - `raster.rs`: layer compositing and page image generation
  - `vector.rs`: vector stroke/page rendering (future)
- `src/pdf/`:
  - `writer.rs`: PDF object writing and xref
  - `annotations.rs`: hyperlinks/tags annotations and destinations
- `src/app/`:
  - `convert.rs`: single-file and directory workflows
  - `errors.rs`: user-facing error taxonomy
- `src/wasm/` (future): web adapter entrypoints

### Foundation deliverables

- Typed metadata model:
  - Replace broad `HashMap<String, String>` with typed structs where stable.
  - Keep unknown metadata in an extension map for forward compatibility.
- Capability model:
  - Resolve notebook capabilities early (`supports_links`, `supports_titles`, `supports_vector_path`, `supports_recognition`).
- Unified page coordinate model:
  - One canonical coordinate system used by raster, vector, and PDF annotations.
- Deterministic tests harness:
  - Golden sample notes by device/firmware with expected page count, dimensions, and checksum.

### Foundation risks

- Refactor can regress speed if ownership/cloning is not carefully handled.
- Parsing assumptions currently implicit in map lookups may fail when made explicit.

### Foundation completion criteria

- Existing CLI behavior unchanged.
- Existing benchmark class remains within 10% of current runtime.
- Test fixtures cover current A5X/A5X2/A6X2 support and failure modes.

## Roadmap Item 1: Optional Vector Graphics Support

README item: "Vector graphic support as an optional feature."

### Target behavior

- New flag, example: `--pdf-type raster|vector` (default `raster`).
- Raster mode remains current fast path.
- Vector mode emits paths/strokes into PDF for scalable output.

### Implementation approach

1. Parse and model path/stroke data:
- Add parser support for path payloads (analogous to `TOTALPATH` handling in supernote-tool parser).
- Define stroke primitives (line, bezier, pressure/width, color).

2. Add vector renderer:
- Convert stroke primitives into PDF path commands.
- Group by layer and draw order to match notebook semantics.
- Normalize brush width and antialias approximations for acceptable visual parity.

3. Build dual PDF backends:
- Keep current image XObject pipeline for raster mode.
- Add vector content stream builder that writes drawing ops directly.
- Optionally keep background/template as raster while foreground pen data is vector (hybrid mode, future enhancement).

4. CLI and validation:
- Reject `vector` when path data is absent with clear fallback suggestion.
- Optionally add `--vector-fallback-to-raster`.

### Test plan

- Visual regression tests (rasterized snapshot from produced PDFs).
- Structural tests: ensure generated PDFs contain vector operators and expected object counts.
- Compare against supernote-tool vector output on selected fixtures.

### Performance considerations

- Vector conversion will be slower; expose this in help text.
- Use per-page parallelism but avoid contention during PDF finalization.

### Exit criteria

- Vector mode works on at least A5X/A5X2 fixtures.
- Output displays correctly in major viewers.
- Raster default performance unaffected.

## Roadmap Item 2: More Device Format Support (A6X, etc.)

README item: "Support for more Supernote device formats (A6X, etc.)."

### Target behavior

- Automatically detect and convert notes from A5, A6X, A5X, A6X2, A5X2 where format is known.
- Graceful handling of unknown signatures with optional loose mode.

### Implementation approach

1. Expand signature and equipment mapping:
- Introduce a signature registry (reference `SN_FILE_VER_*` handling from supernote-tool parser).
- Map `APPLY_EQUIPMENT` and/or signature to page dimensions, orientation behavior, and block quirks.

2. Parser policy mode:
- Add CLI option such as `--policy strict|loose`.
- `strict`: reject unknown signatures.
- `loose`: attempt parse with best-effort defaults and emit warnings.

3. Layer/page parsing compatibility:
- Support layered and non-layered note pages.
- Make layer key order configurable by format profile.
- Handle optional/absent metadata keys without panic.

4. Fixture corpus and compatibility matrix:
- Create `tests/fixtures/<device>/<firmware>/` structure.
- Track per-fixture status: parse, render, visual parity, known limitations.

### Test plan

- Unit tests for signature detection and dimension resolution.
- Parse smoke tests over fixture corpus.
- End-to-end conversion checks for each supported device profile.

### Performance considerations

- Device/profile branching should happen once per notebook, not per pixel.
- Keep decoding hot loops device-agnostic where possible.

### Exit criteria

- Published support matrix in README.
- Deterministic successful conversion for all promoted profiles.
- Clear error messages for unsupported or corrupted formats.

## Roadmap Item 3: Web-based Interface (WASM Drag-and-Drop)

README item: "A web-based interface (WASM) for drag-and-drop conversion."

### Target behavior

- Browser app accepts `.note` file drag-and-drop and returns downloadable PDF.
- All processing local in-browser (no upload).

### Implementation approach

1. Crate split for WASM readiness:
- Move core parsing/rendering into a library crate (`supernote_pdf_core`).
- Keep CLI as thin wrapper crate (`supernote_pdf_cli`).

2. WASM compatibility audit:
- Replace/feature-gate APIs unavailable in wasm32:
  - filesystem direct I/O
  - thread model assumptions (rayon)
- Add an in-memory conversion API: `fn convert_note_bytes_to_pdf_bytes(...)`.

3. Runtime model:
- Start with single-threaded WASM for compatibility.
- Add optional Web Worker/off-main-thread processing for large files.

4. Frontend shell:
- Minimal UI with drag-drop zone, progress state, conversion errors, download link.
- Size and page count display pre/post conversion.

5. Packaging and deployment:
- Use `wasm-bindgen` + bundler workflow.
- Host static assets via GitHub Pages/Cloudflare Pages.

### Test plan

- Browser integration tests (playwright/cypress) for upload-download flow.
- Golden tests comparing native and WASM output page counts and metadata.

### Performance considerations

- WASM memory pressure on large notes; stream/decode by page where feasible.
- Warn user for very large files with estimated processing time.

### Exit criteria

- One-file conversion works reliably in latest Chrome/Firefox/Safari.
- No network dependency for conversion pipeline.
- Documented limits and expected performance.

## Roadmap Item 4: PDF Hyperlinks and Tags

README item: "Support for PDF hyperlinks and tags."

### Target behavior

- Preserve internal page links and external web links when present.
- Optionally include keyword/title annotations/bookmarks for navigation.

### Implementation approach

1. Metadata extraction:
- Parse footer link/title/keyword blocks (similar to supernote-tool parser strategy).
- Normalize into typed structs:
  - `Link { page_index, rect, kind, target }`
  - `Keyword { page_index, rect, text }`
  - `Title { page_index, text, level? }`

2. Coordinate mapping:
- Convert note-space rectangles into PDF user space consistently.
- Handle portrait/landscape orientation and scaling.

3. PDF annotation output:
- Create `/Annots` entries per page with `/Subtype /Link`.
- Internal links: `/Dest` or named destination mapping.
- External links: `/A << /S /URI /URI (...) >>`.

4. Document structure enhancements:
- Optional outline/bookmarks generated from titles.
- Optional text annotations for keywords.

5. CLI flags:
- `--pdf-links on|off` (default `on` if metadata exists).
- `--pdf-keywords on|off` (default `off`).
- `--pdf-bookmarks on|off` (default `off` initially).

### Test plan

- Unit tests for rectangle transformation correctness.
- PDF structure tests inspecting annotation objects and destination references.
- Viewer interoperability checks (Adobe, Preview, browser viewer).

### Performance considerations

- Annotation generation is metadata-bound and should have minimal runtime impact.

### Exit criteria

- Internal and URI links function from converted PDFs.
- Optional keyword/title artifacts render and do not corrupt PDFs.

## Roadmap Item 5: CI Pipeline

README item: "CI pipeline"

### Target behavior

- Automated checks for build, lint, test, and release quality gates.

### Implementation approach

1. GitHub Actions workflows:
- `ci.yml` for pushes/PRs:
  - format (`cargo fmt --check`)
  - lint (`cargo clippy -- -D warnings`)
  - tests (`cargo test`)
  - optional benchmark smoke test on stable fixture
- `release.yml`:
  - tagged release build artifacts for Linux/macOS/Windows
  - crates publish (manual/approved)

2. Fixture management:
- Keep tiny representative fixtures in-repo or pull from LFS/private artifact store.
- Add checksum guard to detect fixture drift.

3. Quality gates:
- Enforce minimum test coverage threshold (if introducing coverage tooling).
- Block merge on failing conversion regression tests.

4. Supply-chain and security checks:
- `cargo audit` scheduled run.
- Dependency update automation (Dependabot or Renovate).

### Test plan

- Verify CI locally with `act` or scripted equivalents for critical jobs.

### Exit criteria

- Every PR gets deterministic pass/fail signal.
- Release process is scripted and repeatable.

## Suggested Milestone Plan

### Milestone 0: Stabilize base

- Module refactor + typed parser + fixture harness
- No feature expansion yet

### Milestone 1: Device coverage

- Signature/equipment registry
- Strict/loose parser policy
- Publish compatibility matrix

### Milestone 2: PDF semantics

- Hyperlink support
- Keywords/titles optional embedding
- Bookmark/outline basics

### Milestone 3: Vector mode

- Vector parser + renderer
- `--pdf-type vector` with tests
- Performance profiling and optimization pass

### Milestone 4: Web/WASM

- Core library split
- wasm32 compile path
- Drag-and-drop web app MVP

### Milestone 5: CI and release hardening

- Full GitHub Actions suite
- Cross-platform artifacts and release docs

## Immediate Next Actions (Execution-Ready)

1. Refactor `src/main.rs` into module skeleton without behavior changes.
2. Add `--policy strict|loose` plumbing and signature registry scaffolding.
3. Introduce fixture test harness with at least one sample per currently supported device profile.
4. Add PDF annotation data model (`Link`, `Keyword`, `Title`) and parse stubs.
5. Stand up CI workflow for fmt/clippy/test to guard further feature work.

## Definition of Done (Project-Level)

- Roadmap items implemented with documented CLI flags and defaults.
- Supported-device matrix is explicit and tested.
- Conversion remains meaningfully faster than reference tooling in raster mode.
- CI enforces reliability and prevents format regressions.
