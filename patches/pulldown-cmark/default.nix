{ pkgs }:
let
  version = "0.12.2";
in
pkgs.applyPatches {
  name = "pico-pulldown-cmark-${version}-patched";
  src = pkgs.fetchurl {
    # The official crates.io release archive, not a moving branch or tag.
    name = "pulldown-cmark-${version}.tar.gz";
    url = "https://static.crates.io/crates/pulldown-cmark/pulldown-cmark-${version}.crate";
    hash = "sha256-+GuiBSrrzMQsu7PtI0uLE85291w1UaMDyyvP/P8SuxQ=";
  };
  patches = [ ./display-blocks.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
