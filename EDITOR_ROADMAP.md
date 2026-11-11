# Editor roadmap

A scene layout and modeling application for the path tracer, built in the same repository as a second binary.

Guiding constraints:

- **Zero external crates.** Like the tracer, it uses only std plus raw platform FFI: our own rasterizer, UI toolkit,
  font rendering, text input and clipboard.
- **The scene TOML is the shared format.** The editor reads every scene the tracer can load and writes files that
  someone can still edit by hand. Saving keeps comments, key order and `include` structure.
- **One implementation of each thing.** Scene model, loaders, BVH, renderer and modeling kernel are libraries used by
  both binaries. Modeled geometry becomes parametric scene object types that the tracer tessellates itself, so the
  scene does not fill up with baked meshes.
- **Scene layout first, then modeling.**

Legend: `[x]` done · `[ ]` planned. Order within a milestone is a suggestion, not a contract.

Target layout:

```
crates/
  pt-core/     scene model, TOML read/write, schema, loaders, BVH, renderer, platform layer
  pt-geom/     modeling kernel: half-edge meshes, subdivision, curves, NURBS, tessellation
  pathtracer/  the CLI and progressive window app
  ptedit/      the editor
```

---

## E0 — Workspace & document model

- [x] Cargo workspace: move today's `src/` into `pt-core` (library) and a thin `pathtracer` binary; keep lints,
  `cargo lint`, the pre-commit hook, CI, regression tests and `bench` working unchanged
- [x] Scene schema: one table of every section, key, type, default, range and alias, used to validate scenes, to
  generate the inspector UI (E2) and to check `docs/scene-format.md` for drift
- [x] Scene document model, separate from the render-ready `Scene`: an editable tree of sections, objects,
  lights, materials, media and prototypes, each knowing which file it came from (`include`)
- [x] Format-preserving TOML: the parser keeps spans, comments and whitespace; the writer changes only the values
  that were edited and adds new entries in the file's own style
- [x] Round-trip tests: every `scenes/*.toml` → document → save → reload gives the same scene and the same
  regression render; saving an unedited document leaves the file byte-identical
- [x] Edits as commands: undo/redo stack, with consecutive edits merged (a slider drag is one undo step)
- Not yet in E0: `pt-geom` (created with E4, when it has code). New object types and keys are added to the schema
  table; a test fails until `docs/scene-format.md` mentions them.
- [x] `ptedit` binary skeleton: opens a window through the existing platform layer, loads a scene, and saves it

## E1 — Viewport

- [x] Software rasterizer: tiled and multithreaded, depth buffer, perspective-correct interpolation, clipping;
  modes: wireframe, solid (flat / Lambert headlight), shaded (base color), wireframe over solid
- [x] Blitted through the existing Win32 / X11 / Cocoa image paths; redraws only when something changes
- [x] Viewport camera separate from the scene camera; orbit / pan / zoom / fly from `controls.rs`; frame the
  selection; switch between perspective and orthographic views
- [x] Layouts: single view, and four views (top, front, side, perspective) with a maximize toggle
- [x] Ground grid that adapts to the zoom level, world axes, a small axis indicator in the corner
- [x] Display of every object type: analytic shapes tessellated at screen-adaptive density; meshes; instances
  (scatters as bounding boxes past a threshold); implicit surfaces as bounding boxes for now
- [x] Overlays: the scene camera's frustum, light icons, bounding boxes, the selection outline
- [x] Picking through the BVH (same code as the tracer); rectangle selection
- [x] Performance target: interactive at 1080p for ~300k visible triangles; level-of-detail by object size on
  screen above that
- How E1 turned out: the loader records which `[[objects]]` / `[[lights]]` entry made each primitive, instance
  and light (`Scene::provenance`), so BVH hits map back to document items. Scatter instances and meshes too small
  on screen (or past a 400k-triangle budget) show as solid boxes; implicit surfaces as wire boxes. Rectangle
  selection takes what is visible in the rectangle (the id buffer). Moving objects show where the shutter opens.
  The sky's sun is drawn but not selectable (it is not a `[[lights]]` entry). `ptedit --snapshot` renders the
  viewport headless for tests and timing: 300k triangles at 1080p draw in ~10 ms, the 56 000-instance forest of
  `scenes/instancing.toml` in ~25 ms (16 threads).

## E2 — UI toolkit

- [x] Platform input for UI: text input (`WM_CHAR`; X11 `XLookupString` / XIM; Cocoa `NSTextInputClient`),
  clipboard (Win32 clipboard, X11 `CLIPBOARD` selection, `NSPasteboard`), cursor shapes, key repeat, double
  click, HiDPI scale factor
- [x] Text: embedded bitmap font first; then our own TrueType rasterizer (`glyf` quadratic outlines, coverage
  antialiasing, glyph cache) loading a bundled or system font
- [x] Immediate-mode core: IDs, hot and active widget, focus, keyboard navigation, scrolling, clipping, tooltips
- [x] Widgets: label, button, checkbox, drop-down, numeric field with drag-to-change, slider, vec3 editor, color
  picker (RGB / HSV / Kelvin), text field, collapsible section, tree view, menu bar, context menu, modal dialog
- [x] Layout: fixed panels with splitters (viewport, outliner, inspector, status bar); layout saved per user
- [x] Outliner: objects, lights, camera, materials, media and prototypes as a tree; rename, reorder, drag and drop,
  show / hide, show which included file an entry belongs to
- [x] Inspector generated from the schema: every key of the selected item, with defaults shown and greyed out until
  set, aliases resolved, and validation errors shown next to the field
- [x] In-app file browser (open / save / pick textures and meshes); recent files
- [x] Theme (light / dark), all colors as named tokens
- How E2 turned out: the platform layer gained `Event::Text`, more keys (Enter, Tab, Backspace, Delete, Insert, End,
  Page Up / Down), clipboard text, cursor shapes, the double-click time and a scale factor (Win32: the system DPI at
  start; X11: `Xft.dpi`; macOS: the backing scale). X11 text goes through an input method (`Xutf8LookupString`, dead
  keys and compose work) and the clipboard both ways is answered and requested by us; protocol errors no longer end
  the process. macOS text comes from the key event's characters, not a full `NSTextInputClient`, so composing input
  methods (CJK) do not work there yet; Command counts as Ctrl. Text is drawn by our TrueType reader and rasterizer
  (`glyf` outlines, composites, `.ttc`, exact-area coverage, glyphs cached at quarter-pixel positions; no hinting, no
  CFF fonts, no kerning) from a system font (Segoe UI, DejaVu Sans, Arial, ...), with the embedded 8×8 font as the
  fallback and in tests. The views' labels use it too. The outliner filters by name; renaming a material, medium or
  prototype renames its references in every file as one undo step (`SceneDoc::group`); dragging reorders lights and
  objects within their file (whole `[[objects]]` blocks with their comments). The inspector also creates sections the
  file lacks (`[render]`), wraps problems under their field, and edits inline materials, textures and tables in place.
  Save As stays in the scene's folder, so that the relative paths in it stay right. Every edit rebuilds the scene (a
  drag rebuilds it each frame), which is slow for heavy scenes; E3's refit is the fix. A scene the loader rejects
  opens with an empty view and its error, to be fixed in the editor. `ptedit --snapshot --ui` draws the whole window
  headless; tests drive the toolkit and the editor with synthetic input (`PTEDIT_TRACE=1` prints window events).

## E3 — Scene editing

- [x] Create every primitive and light from a menu or shortcut, placed under the cursor on the surface or the
  ground plane; delete, duplicate, copy / paste between scenes
- [x] Transform gizmos (move / rotate / scale) in world or local space, with numeric entry while dragging;
  snapping to grid, angle steps, vertices and surfaces
- [x] Editing that writes back as `translate` / `rotate` / `scale` in the scene's own conventions
- [x] Lights: point / spot / directional helpers with editable cone angles and handles; drag the sun on the sky
  dome; environment rotation preview
- [x] Camera: "look through camera", lock the viewport to the camera, focal length / f-stop / focus distance
  with a focus plane overlay, and click-to-focus
- [x] Materials: library browser (including `scenes/lib/materials.toml`), assign by drag and drop, a material editor
  with a small path-traced preview sphere, texture pickers with thumbnails
- [x] Prototypes and instances: turn the selection into a prototype, place instances, edit `scatter` parameters
  with a live preview of the scatter box
- [x] Media and render settings editable in the inspector; `[output]` and `[render]` presets
- [x] Path-traced viewport: the progressive renderer in a viewport pane, restarted on edits; BVH refit (not
  rebuild) for transform-only changes; region render; switching between raster and rendered views
- [x] Hot-reload both ways: the editor picks up changes to files saved outside it, and the tracer window keeps
  working beside the editor as it does today
- How E3 turned out: things are added from an Add menu (`Shift+A` at the mouse) on the surface under the cursor,
  found by tracing, with its normal from two neighbouring rays, else on the ground plane, at a size of about 60 pixels
  rounded to 1, 2 or 5. Duplicates and pastes copy the entries' text (`Document::block_text` / `insert_sections`), so
  comments come along. The clipboard is TOML: the entries, the materials and prototypes they name, and file paths
  made absolute (made relative again where they are pasted). Gizmos are drawn as screen lines at a constant size.
  Moves keep the file's own form: a point key (`center`, `base`, `origin`, `position`) is moved when the object has no
  transform, a box's `min` / `max` both, else `translate`. A rotation or scale of an object placed by its point folds
  that point into `translate` first, so it turns about itself (`center = c` becomes `translate = c`, `rotate = ...`).
  Rotations are written as `rotate` angles; of the two angle sets for a rotation, the one closer to the old angles is
  chosen (no jumps from 179 to -179). Scaling is always along the item's own axes, which is what `scale` can say.
  During a drag the edits are rewritten from the tables as they were at the start, and the scene is changed in place:
  `Scene::transform_object` moves world geometry, puts the change on top of instances, and refits the BVHs
  (`Bvh::refit`); lights are replaced (`Scene::set_light`). Scatter instances (whose copies the loader places by
  adding angles, which a refit does not reproduce) are loaded again when the drag ends, and a change a refit cannot
  express (a sphere scaled unevenly) loads the scene on every step. The sky's sun loads the scene as it is dragged
  (the sky is rebuilt); the environment's `rotate` is previewed by the shaded view's backdrop and loaded when let go.
  The path-traced view renders the perspective view on its own thread (`renderer::render_region` gained regions);
  surfaces are drawn in a key color that the traced image replaces, so the selection outline, light icons and gizmos
  stay on top. Looking through the camera draws the view with the camera's pose and field of view (fisheye and
  panoramic cameras show as perspective there) and dims what is outside its image; the traced view then uses the
  camera itself, with its depth of field. Material previews render a sphere on a checkered floor at 24-48 samples on
  a worker thread (media are left out); library files are the `lib/*.toml` (and `scenes/lib/*.toml`) beside the scene
  and up to three folders above. Hot reload polls modification times twice a second; the editor's own saves are
  recognised by their text. Media and render settings were editable in the inspector since E2; E3 adds the presets.
  `ptedit --snapshot ... --rendered SPP` draws the traced view headless.

## E4 — Polygon modeling

- [ ] `pt-geom` half-edge mesh with attributes (UVs, creases, material groups); conversion from and to our meshes
- [ ] Edit mode: vertex / edge / face selection, loops and rings, grow / shrink, select linked
- [ ] Operations: extrude (along normals / along an axis), inset, bevel (edges and vertices), loop cut, knife,
  bridge, merge / weld, dissolve, fill hole, mirror, flip normals, triangulate
- [ ] Proportional (soft) editing; snapping in edit mode
- [ ] Save as PLY / OBJ next to the scene, referenced by a `mesh` object

## E5 — Subdivision surfaces

- [ ] Catmull-Clark with boundary rules and semi-sharp creases (Pixar / OpenSubdiv behaviour); Loop for
  triangle meshes
- [ ] Editor display: the cage drawn over the subdivided result, preview level per object
- [ ] Tracer: `type = "subdiv"` with a cage file (or inline cage), `level`, and creases; subdivided at load,
  optionally projected onto the limit surface with limit normals

## E6 — Curves & generated shapes

- [ ] Curve editing: polylines, cubic Béziers, uniform B-splines; open and closed; points and tangents with handles
- [ ] Parametric object types, understood by both the editor and the tracer, tessellated by `pt-geom`:
  `revolve` (lathe, with angle and segments), `extrude` (with caps and taper), `sweep` (profile along a path,
  rotation-minimizing frames), `loft` (through several profiles)
- [ ] Convert any parametric object to an editable mesh

## E7 — NURBS

- [ ] NURBS curves and surfaces (Cox–de Boor evaluation, rational weights), knot insertion and degree elevation
- [ ] Control-point editing with weights; surface creation from curves (ruled, revolved, swept, skinned)
- [ ] Adaptive tessellation by curvature and screen size; `type = "nurbs"` in the tracer
- [ ] Trimming curves, as a stretch goal

## E8 — Implicit editing

- [ ] Node editor for SDF trees: primitives, CSG with blend widths, round / shell / displace
- [ ] Hypertexture and fractal parameters with live preview
- [ ] Viewport preview by sphere tracing (the tracer's implicit code) and, for the solid view, marching cubes or
  dual contouring

---

Ideas not yet placed: keyframes for motion blur (`motion`), painting scatters onto surfaces, UV unwrapping,
mesh booleans, an OpenGL viewport backend (loaded at runtime like Xlib) for very large scenes.
