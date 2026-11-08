# Cached Docker archive compression

Default gzip archive emission compresses each immutable OCI layer's outer Docker
tar entry independently. Descendants link inherited gzip members when the
outer-tar timestamp and compressor recipe match, then append a small compressed
config/manifest/final-padding member. Concatenated gzip members decode to the
exact original raw Docker tar bytes. The normal raw stream and explicitly
selected custom compressors retain their existing interface.

This cache includes the real outer-tar mtime and exact compressor path/recipe;
raw OCI digest identity includes package bytes, ownership and internal tar
metadata. A timestamp change cannot reuse old entry headers. Immutable parent
references retain their producer outputs; no mutable caller cache or manually
asserted version substitutes for Nix dependencies.

Native reconstruction tests cover tar padding boundaries. Packaged Darwin and
Linux owners compare decompressed default/caller-overridden outputs with the raw
exporter, inspect actual Skopeo conversions, prove inherited member links,
refuse reuse after timestamp changes, and preserve the failed-parent-archive and
failing-producer negative controls.

This eliminates inherited-layer recompression on this path, not final archive
byte duplication. It does not establish cross-profile canonical store-layer
projection or full cold/warm physical-allocation results. Existing emitted
archives remain preserved; logical byte counts are not physical savings.
