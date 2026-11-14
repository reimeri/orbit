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
      # Never copy the reference implementation, target/, or session assets into Nix builds.
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml ./Cargo.lock ./.cargo ./crates ./scripts
        ];
      };
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
      };
    };
}
