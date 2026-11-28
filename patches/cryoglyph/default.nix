{ pkgs }:
pkgs.applyPatches {
  name = "orbit-cryoglyph-0.1.0-patched";
  src = pkgs.fetchurl {
    name = "cryoglyph-0.1.0.tar.gz";
    url = "https://static.crates.io/crates/cryoglyph/cryoglyph-0.1.0.crate";
    hash = "sha256-CLx5W9vM29Rhc2+xY5MKAJ2mWXsibW9vzjPnqOtuxRk=";
  };
  patches = [ ./atlas-growth.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
