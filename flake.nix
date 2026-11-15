{
  description = "Pico V2 — native Wayland Rust/iced harness";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;
      runtimeLibraries = with pkgs; [ wayland libxkbcommon vulkan-loader libGL fontconfig ];
      runtimePath = lib.makeLibraryPath runtimeLibraries;
      fontConfig = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
      patchedWinit = import ./patches/winit { inherit pkgs; };
      patchedIcedGraphics = import ./patches/iced-graphics { inherit pkgs; };
      patchedIcedWidget = import ./patches/iced-widget { inherit pkgs; };
      # Never copy the reference implementation, target/, or session assets into Nix builds.
      projectSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml ./Cargo.lock ./.cargo ./crates ./scripts ./patches
        ];
      };
      # The same generated Cargo path is used by package and formatting checks.
      # Upstream source stays in the Nix store, never in the tracked project.
      src = pkgs.runCommand "pico-evo-source" { } ''
        cp -R ${projectSrc} "$out"
        chmod u+w "$out"
        mkdir "$out/.build-deps"
        ln -s ${patchedWinit} "$out/.build-deps/winit"
        ln -s ${patchedIcedWidget} "$out/.build-deps/iced-widget"
        ln -s ${patchedIcedGraphics} "$out/.build-deps/iced-graphics"
      '';
      desktopItem = pkgs.makeDesktopItem {
        name = "pico-evo";
        desktopName = "Pico V2";
        comment = "Native Wayland AI coding harness";
        exec = "pico-evo";
        terminal = false;
        categories = [ "Development" ];
      };
      pico = pkgs.rustPlatform.buildRustPackage {
        pname = "pico-evo";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
        inherit src;
        cargoLock.lockFile = ./Cargo.lock;
        strictDeps = true;
        nativeBuildInputs = with pkgs; [ pkg-config mold makeWrapper ];
        buildInputs = runtimeLibraries ++ [ pkgs.sqlite ];
        # Rendered unit tests need fonts inside the build sandbox too, not only
        # in the development shell and installed wrapper.
        FONTCONFIG_FILE = fontConfig;
        doCheck = true;
        nativeCheckInputs = [ pkgs.git ];
        cargoTestFlags = [ "--workspace" ];
        doInstallCheck = true;
        nativeInstallCheckInputs = [ pkgs.weston ];
        postInstall = ''
          mkdir -p "$out/share/applications"
          cp ${desktopItem}/share/applications/* "$out/share/applications/"
        '';
        postFixup = ''
          wrapProgram "$out/bin/pico-evo" \
            --prefix LD_LIBRARY_PATH : ${runtimePath} \
            --prefix PATH : ${lib.makeBinPath [ pkgs.git pkgs.xdg-utils ]} \
            --set-default FONTCONFIG_FILE ${fontConfig}
        '';
        installCheckPhase = ''
          runHook preInstallCheck
          bash scripts/smoke-wayland.sh "$out/bin/pico-evo"
          runHook postInstallCheck
        '';
        meta = {
          description = "Pico V2 native Wayland coding harness";
          mainProgram = "pico-evo";
          platforms = [ system ];
        };
      };
    in {
      packages.${system} = { default = pico; pico-evo = pico; };
      apps.${system}.default = {
        type = "app";
        program = "${pico}/bin/pico-evo";
      };
      checks.${system} = {
        package = pico; # Includes unit tests and the headless Wayland window check.
        formatting = pkgs.runCommand "pico-evo-formatting" {
          nativeBuildInputs = with pkgs; [ cargo rustfmt ];
        } ''
          cargo fmt --manifest-path ${src}/Cargo.toml --all --check
          touch "$out"
        '';
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          rustc cargo rustfmt clippy rust-analyzer
          pkg-config mold git just time xdg-utils
          weston wayland-utils
        ];
        buildInputs = runtimeLibraries ++ [ pkgs.sqlite ];
        LD_LIBRARY_PATH = runtimePath;
        FONTCONFIG_FILE = fontConfig;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        shellHook = ''
          # Locate this checkout even when entering from a package subdirectory.
          root="$PWD"
          while [[ ! -f "$root/patches/winit/default.nix" ]]; do
            if [[ "$root" == / ]]; then
              echo "Enter nix develop from the Pico checkout to prepare its patched dependencies." >&2
              exit 1
            fi
            root="$(dirname "$root")"
          done
          mkdir -p "$root/.build-deps" &&
            ln -sfnT ${patchedWinit} "$root/.build-deps/winit" &&
            ln -sfnT ${patchedIcedWidget} "$root/.build-deps/iced-widget" &&
            ln -sfnT ${patchedIcedGraphics} "$root/.build-deps/iced-graphics" || {
              echo "Could not prepare .build-deps. Check checkout permissions and re-enter nix develop." >&2
              exit 1
            }
        '';
      };
    };
}
