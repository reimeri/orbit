{ pkgs }:
pkgs.applyPatches {
  name = "pico-iced-graphics-0.14.0-patched";
  src = pkgs.fetchurl {
    name = "iced_graphics-0.14.0.tar.gz";
    url = "https://static.crates.io/crates/iced_graphics/iced_graphics-0.14.0.crate";
    hash = "sha256-I0yhws7EFVBV9o+l+tG1JCxJasgjjYCiWbyjgvtEoQI=";
  };
  patches = [ ./ligature-caret.patch ./editor-performance.patch ./editor-range-spans.patch ./editor-history.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
