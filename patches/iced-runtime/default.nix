{ pkgs }:
pkgs.applyPatches {
  name = "orbit-iced-runtime-0.14.0-patched";
  src = pkgs.fetchurl {
    name = "iced_runtime-0.14.0.tar.gz";
    url = "https://static.crates.io/crates/iced_runtime/iced_runtime-0.14.0.crate";
    hash = "sha256-0YibgZzkwGZ0GDJC4zbI1JRlZlRBOWkU3AfMhvRPqNQ=";
  };
  patches = [ ./notification-activation.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
