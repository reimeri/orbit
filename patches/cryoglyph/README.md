# GPU glyph atlas growth

Base: checksum-pinned crates.io **cryoglyph 0.1.0**. The atlas allocator keeps
the existing coordinates when it grows. Upstream nevertheless rasterizes
every cached glyph again and issues a texture upload for each. On a large
first chat frame this was a substantial part of unoptimized build time.

`atlas-growth.patch` copies the old mask or color texture to the enlarged
texture on the GPU. The copy is submitted immediately: earlier queued glyph
writes to the old texture must run before it, and later writes into recycled
slots in the new texture must run after it. Rasterization of new glyphs, atlas
capacity/eviction, color formats, sampling and typography do not change.

The `growing_the_glyph_atlas` test compares a single frame containing many
distinct large glyphs with the same text rendered in smaller batches. The
large frame uses an independent cold atlas on the shared test device.
