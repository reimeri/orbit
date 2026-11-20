{ pkgs }:
pkgs.applyPatches {
  name = "pico-cosmic-text-0.15.0-patched";
  src = pkgs.fetchurl {
    name = "cosmic-text-0.15.0.tar.gz";
    url = "https://static.crates.io/crates/cosmic-text/cosmic-text-0.15.0.crate";
    hash = "sha256-FzhSKDqaV6PL42XYbnTcQooJxQQhR31a1v6dlQnjdzc=";
  };
  patches = [ ./editor-performance.patch ./incremental-shaping.patch ./replay-caret.patch ./history-delete.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
