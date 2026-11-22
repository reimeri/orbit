{ pkgs }:
let
  version = "0.30.13";
in
pkgs.applyPatches {
  name = "orbit-winit-${version}-patched";
  src = pkgs.fetchurl {
    name = "winit-${version}.tar.gz";
    url = "https://static.crates.io/crates/winit/winit-${version}.crate";
    hash = "sha256-pnVfpYqfg1C9HkctTD/MJfgk7DWJM7ujMwbQtj31l40=";
  };
  patches = [ ./wayland-image-input.patch ./notification-activation.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
