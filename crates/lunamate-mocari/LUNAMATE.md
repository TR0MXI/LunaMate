# LunaMate maintenance notes

This crate contains the source of Mocari 0.4.0 maintained in the LunaMate
workspace.

- Upstream: https://github.com/Eatgrapes/Mocari
- Upstream commit: `0a5f39647c9ea8c299d6c284d5f26e824bf76716`
- crates.io package checksum: `bbcaf6c7d9f1c750fd4f4d591912dc512f993ea7dc555afa4080d3767ee14173`
- License: MIT, copyright 2026 Eatgrapes; see `LICENSE`

Local changes:

- The WGPU backend imports WGPU 29 through `gpui_wgpu::wgpu`, so LunaMate and
  GPUI use one WGPU crate version.
- The WGPU 30 optional vertex-buffer layout and queue presentation calls were
  adapted to their WGPU 29 equivalents.
- The unused direct `png` dependency was removed. PNG decoding remains enabled
  through `image`.
- Runtime loading accepts caller-owned `RuntimeModelAssets`: a parsed Model3,
  owned MOC bytes, parsed optional Physics3/Pose3 values, decoded textures, and
  a metadata-only model directory. `load_model_runtime_from_assets` constructs
  the runtime without opening any manifest reference, allowing LunaMate to use
  the exact bounded snapshots it validated. The filesystem path API remains for
  independent Mocari consumers, but decodes textures sequentially instead of
  creating one thread per manifest entry. Upstream status: the pinned commit has
  no equivalent owned-assets loader; remove this adaptation after upstream
  provides a loader with the same no-path-access contract for all required
  runtime assets.
- Expression players expose transition activity and remain available until
  their declared fade-out completes, so a long fade-out is not truncated when
  the replacement expression fades in quickly. The manager aggregates
  additive, multiply, and overwrite values before applying the combined fade,
  and bounds overlapping players.
- Motion and expression players retain parsed `Motion3` and `Expression3` data
  in `Arc`, while time, weight, looping, and fade state remain player-local.
  Their constructors accept either owned or shared parsed data, so repeated
  manifest declarations do not clone large curve or parameter vectors.
  Upstream status: the pinned commit stores parsed values directly in every
  player; remove this adaptation after upstream provides equivalent shared
  immutable ownership without coupling independent playback state.
- WGPU vertex encoding clamps finite opacity and color channels to the same
  range used by LunaMate's CPU rasterizer; non-finite values remain rejected
  by the application validation layer.
- Constant-width slice iteration uses `as_chunks` to satisfy the Clippy lint
  enabled by the current stable Rust toolchain without changing the validated
  mesh data handling. Remove this adaptation if the upstream source adopts the
  same lint fix.
- Dynamic WGPU vertex uploads use LunaMate's reusable staging belt instead of
  allocating one native staging resource per changed drawable. The mesh buffer
  cache also reuses its CPU encoding scratch and only rebuilds draw order when
  that order changes.
- Deformer composition caches its static parent-depth order and reads packed
  warp-grid values directly instead of allocating a temporary vector for every
  active keyform.
- Steady mesh updates retain keyform axes and slots, composed deformer storage,
  each warp grid, art-mesh interpolation positions, drawable part opacities,
  and glue interpolation storage at their cross-frame high-water marks. Failed
  composition invalidates partial results without discarding those capacities;
  cloning a runtime starts with empty scratch instead of copying the retained
  buffers.
- Steady mesh updates evaluate every drawable's exact opacity, colors, and order,
  then update positions only for visible drawables and the transitive mask/glue
  geometry they consume. Invisible geometry is refreshed before it becomes
  visible again. A previous-frame hidden-vertex threshold keeps ordinary and
  fully visible models on the original fused update path when dependency planning
  would cost more than it saves. Models with offscreen metadata conservatively
  retain the full geometry path until Mocari implements the corresponding effect
  passes.
- Warp and rotation deformers expose prepared `WarpTarget` and `RotationTarget`
  values that validate the grid geometry, precompute the extrapolation basis and
  build the rotation matrix once per deformer, plus a batched `transform_slice`.
  Deformer composition and art-mesh vertex transforms resolve the parent
  deformer and its type once per batch instead of once per point, so the
  per-point path no longer repeats range checks, checked arithmetic, grid length
  validation, corner-basis reconstruction, enum dispatch or `sin`/`cos`.
- MOC3 count tables have structural, per-section, and aggregate complexity
  limits before parser allocation. Generic sections and id tables use fallible
  reservations so malformed models return an error instead of requesting an
  unbounded vector. Derived warp grids, interpolation axes, draw-order spans,
  offscreen slots, parent/group graphs, aggregate mesh geometry, and negative
  index counts are bounded or rejected as well.
- Expression JSON parameters are bounded and the manager caches its active ID
  set, so frame application does not repeatedly build a quadratic union.
- Runtime model and physics indexes use rapidhash 4.5.1 with its `unsafe`
  feature enabled to reduce hashing overhead in model loading and lookup paths.
- Physics parsing derives setting, input, output, and vertex counts from decoded
  arrays and requires all four `Meta` counts to match. It caps files at 256
  settings, 256 inputs/outputs/vertices per setting, and 4,096 total items of
  each kind before publishing `Physics3`. All decoded floats must be finite;
  weights stay in 0..=100, mobility/acceleration in 0..=1,000, and delay is zero
  or in 0.0001..=1,000 so its guarded velocity division cannot overflow at the
  fixed-step ceiling. Vector components, normalization values, radius, and the
  magnitude of signed output scales stay at or below 1,000,000. Normalization
  ranges must be ordered with their default inside the range, and every output
  vertex must have both its selected particle and a preceding particle. Negative
  scales and negative angle ranges remain supported. The official Cubism Native
  sample set peaks at 16 settings, totals of 43 inputs, 42 outputs, and 58
  vertices, per-setting counts of 5, 9, and 11, and scalar values no larger than
  10 acceleration, 72.1 radius, or 40 scale; these ceilings therefore retain at
  least 16x structural headroom and much larger numeric headroom while bounding
  fixed-step work. Upstream status:
  the pinned commit trusts decoded arrays, metadata, floats, and vertex indexes;
  remove this patch after updating to an upstream revision with equivalent
  decoded-data budgets, count consistency, numeric range, and index validation.
- Physics fixed-step metadata is clamped to 240 FPS, and each evaluation runs at
  most 240 catch-up steps before dropping excess accumulated time. This prevents
  malformed physics metadata from making floating-point subtraction stall in an
  unbounded loop. Upstream status: the pinned commit has no equivalent bounds;
  remove this patch after updating to an upstream revision with both an FPS
  ceiling and a hard per-evaluation catch-up budget.
- Motion parsing derives curve, segment, and point counts from decoded data and
  requires them to match `Meta`. It caps motions at 4,096 curves, 16,384 segments
  per curve, 65,536 total segments, and 131,072 total points; rejects non-finite
  metadata, curve fade values, and points plus non-monotonic segment or Bezier
  control times; and
  uses binary search for per-frame segment lookup. These limits follow Mocari's
  existing 4,096 expression-parameter and art-mesh ceilings plus its 65,536 MOC3
  structural budget, while leaving more than four minutes of frame-by-frame keys
  on one 60 FPS curve. Upstream status: the pinned commit trusts metadata and
  scans every curve from the start; remove this patch after updating to an
  upstream revision with equivalent decoded-data budgets, finite/ordering
  validation, and logarithmic segment lookup.
- The motion parser accepts VTube Studio recording exports marked as version 0;
  those files use the Cubism 3 curve and segment layout but may report three
  extra metadata points per curve. Decoded-data budgets remain authoritative,
  and no other count mismatch is accepted.
- Upstream examples, integration tests, and development dependencies are not
  included in this workspace crate.
- The local-only `benchmark-support` feature exposes a synthetic two-keyform
  warp chain to Criterion. `deformer_composition` measures warmed steady-state
  scratch; `deformer_composition_allocations` asserts that it performs zero
  allocations. `runtime_mesh_update` loads a user-supplied model and verifies the
  optimized path against an unpruned reference before measuring named parameter
  states. Criterion, allocation-counter, and mimalloc are dev-only dependencies
  and the feature is not enabled by LunaMate's production edge.
- Three retained unit tests that require the undistributed Hiyori model are
  ignored by default and can be run manually after supplying that asset.

`Cargo.toml.upstream` preserves the original published dependency declaration.
Live2D model assets and Live2D Inc. software are not included in this directory.
