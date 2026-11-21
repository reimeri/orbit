{ pkgs }:
pkgs.applyPatches {
  name = "orbit-iced-winit-0.14.1-patched";
  src = pkgs.fetchurl {
    name = "iced_winit-0.14.1.tar.gz";
    url = "https://static.crates.io/crates/iced_winit/iced_winit-0.14.1.crate";
    hash = "sha256-dYmIjI6VGJnMaIJHppkzu3oFEfC0suEirD/NXayzf3I=";
  };
  patches = [ ./screenshot-ordering.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
