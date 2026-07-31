//! Backend-neutral rendering helpers.
//!
//! This module converts runtime drawable meshes into renderer-friendly data:
//! sorted draw indices, packed vertices, packed indices, and clipping-mask plans.
//! Use it when writing your own renderer instead of the built-in `wgpu` backend.

mod clipping;
mod vertex;

#[cfg(feature = "wgpu")]
pub(crate) use clipping::draw_order_indices_from;
pub use clipping::{
    ClippingContext, ClippingLayout, ClippingLayoutError, ClippingPlan, ClippingRect, DrawableInfo,
    MaskChannel, draw_order_indices,
};
#[cfg(feature = "wgpu")]
pub(crate) use vertex::drawable_render_values;
pub use vertex::{
    DrawableVertex, encode_indices, encode_vertices, encode_vertices_from_drawable,
    vertex_from_drawable_vertex, vertices_from_drawable,
};
