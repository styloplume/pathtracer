//! `pt-geom`: the modeling kernel. Polygon meshes with half-edge connectivity and
//! attributes (texture coordinates per corner, edge creases, material groups), their
//! selection helpers (loops, rings, growing, linked parts), the modeling operations
//! (extrude, inset, bevel, loop cut, knife, bridge, merge, dissolve, fill, mirror, ...),
//! primitive shapes as meshes, and conversion from and to the tracer's meshes and PLY / OBJ
//! files.
//!
//! Operations never change a mesh in place: they read one and build another (`Mesh::build`
//! makes the connectivity again), which keeps every operation simple and every result
//! consistent. Moving vertices is the exception: positions are plain data.

pub mod io;
pub mod mesh;
pub mod ops;
pub mod primitives;
pub mod select;
mod triangulate;

pub use mesh::{Mesh, NONE, Polygon, edge_key};
pub use select::{Mode, Selection};
pub use triangulate::ear_clip;
