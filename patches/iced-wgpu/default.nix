{ pkgs }:
pkgs.applyPatches {
  name = "orbit-iced-wgpu-0.14.0-patched";
  src = pkgs.fetchurl {
    name = "iced_wgpu-0.14.0.tar.gz";
    url = "https://static.crates.io/crates/iced_wgpu/iced_wgpu-0.14.0.crate";
    hash = "sha256-/xRKmZsMoPihAleTRQAGAkCCXELpUOwOvunIrjBWHBM=";
  };
  patches = [ ./snap-edges.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
