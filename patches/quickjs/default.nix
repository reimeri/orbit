{ pkgs }:
{
  sys = pkgs.applyPatches {
    name = "orbit-rquickjs-sys-0.11.0-patched";
    src = pkgs.fetchurl {
      name = "rquickjs-sys-0.11.0.tar.gz";
      url = "https://static.crates.io/crates/rquickjs-sys/rquickjs-sys-0.11.0.crate";
      hash = "sha256-JzRGAe8nRg6C1qTh7Lnn6Z9RgSIJXzxRKW2o6b4rnYM=";
    };
    patches = [ ./module-lifetime.patch ./writable-build-inputs.patch ];
    patchFlags = [ "-p1" "--fuzz=0" ];
  };
  core = pkgs.applyPatches {
    name = "orbit-rquickjs-core-0.11.0-patched";
    src = pkgs.fetchurl {
      name = "rquickjs-core-0.11.0.tar.gz";
      url = "https://static.crates.io/crates/rquickjs-core/rquickjs-core-0.11.0.crate";
      hash = "sha256-uL94QChcMhw6sg51Kpr7lVSMdc1/RjKgYnzqNQfjEME=";
    };
    patches = [ ./module-handles.patch ];
    patchFlags = [ "-p1" "--fuzz=0" ];
  };
}
