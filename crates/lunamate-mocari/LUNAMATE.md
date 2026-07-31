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
- Texture decoder worker panics are converted to `AssetLoadError` instead of
  panicking the model-loading task.
- Expression players expose transition activity and remain available until
  their declared fade-out completes, so a long fade-out is not truncated when
  the replacement expression fades in quickly. The manager aggregates
  additive, multiply, and overwrite values before applying the combined fade,
  and bounds overlapping players.
- WGPU vertex encoding clamps finite opacity and color channels to the same
  range used by LunaMate's CPU rasterizer; non-finite values remain rejected
  by the application validation layer.
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
- The motion parser accepts VTube Studio recording exports marked as version 0;
  those files use the same curve and segment layout as Cubism motion version 3.
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
