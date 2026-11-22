# Pixel snapping for images and quads

Nix applies `snap-edges.patch` to checksum-pinned crates.io **iced_wgpu 0.14.0**.
The layer scissor already rounds each physical edge with Rust's `f32::round`.
Upstream images use WGSL `round` (half-pixel ties can round to even), while
quads add a small bias (which also moves edges just below a tie). A snapped
image can therefore start a pixel before its hover panel or the panel's scissor.

The image and quad vertex shaders now share the scissor's nearest-pixel rule,
with ties away from zero. The image clip extent is the difference between the
two snapped edges, not between a snapped end and a fractional start.
Unsnapped drawing is unchanged.

When updating iced, check whether the upstream shaders still need the patch.
Run `nix develop --command just test metadata_covers_fractional_edges` to
compare the image and its hover panel on Vulkan at multiple scales.
