{
  description = "Orbit — native Wayland Rust/iced harness";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;
      runtimeLibraries = with pkgs; [ wayland libxkbcommon vulkan-loader libGL fontconfig ];
      runtimePath = lib.makeLibraryPath runtimeLibraries;
      testVulkanIcd = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
      fontConfig = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
      enDictionary = pkgs.hunspellDicts.en_US;
      spellingData = pkgs.runCommand "orbit-dictionaries" { } ''
        mkdir -p "$out"
        cp ${enDictionary}/share/hunspell/en_US.{aff,dic} "$out/"
        cp ${enDictionary}/share/doc/*.txt "$out/"
      '';
      patchedQuickjs = import ./patches/quickjs { inherit pkgs; };
      patchedWinit = import ./patches/winit { inherit pkgs; };
      patchedCmark = import ./patches/pulldown-cmark { inherit pkgs; };
      patchedCosmicText = import ./patches/cosmic-text { inherit pkgs; };
      patchedIcedGraphics = import ./patches/iced-graphics { inherit pkgs; };
      patchedIcedWgpu = import ./patches/iced-wgpu { inherit pkgs; };
      patchedIcedWinit = import ./patches/iced-winit { inherit pkgs; };
      patchedIcedRuntime = import ./patches/iced-runtime { inherit pkgs; };
      patchedIcedWidget = import ./patches/iced-widget { inherit pkgs; };
      # Never copy target/ or session assets into Nix builds.
      projectSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml ./Cargo.lock ./.cargo ./crates ./scripts ./patches
        ];
      };
      # The same generated Cargo path is used by package and formatting checks.
      # Upstream source stays in the Nix store, never in the tracked project.
      src = pkgs.runCommand "orbit-source" { } ''
        cp -R ${projectSrc} "$out"
        chmod u+w "$out"
        mkdir "$out/.build-deps"
        ln -s ${patchedWinit} "$out/.build-deps/winit"
        ln -s ${patchedCmark} "$out/.build-deps/pulldown-cmark"
        ln -s ${patchedIcedWinit} "$out/.build-deps/iced-winit"
        ln -s ${patchedIcedRuntime} "$out/.build-deps/iced-runtime"
        ln -s ${patchedIcedWidget} "$out/.build-deps/iced-widget"
        ln -s ${patchedCosmicText} "$out/.build-deps/cosmic-text"
        ln -s ${patchedIcedGraphics} "$out/.build-deps/iced-graphics"
        ln -s ${patchedIcedWgpu} "$out/.build-deps/iced-wgpu"
        ln -s ${patchedQuickjs.sys} "$out/.build-deps/rquickjs-sys"
        ln -s ${patchedQuickjs.core} "$out/.build-deps/rquickjs-core"
      '';
      desktopItem = pkgs.makeDesktopItem {
        name = "orbit";
        desktopName = "Orbit";
        icon = "orbit";
        comment = "Native Wayland AI coding harness";
        exec = "orbit";
        terminal = false;
        categories = [ "Development" ];
      };
      orbit = pkgs.rustPlatform.buildRustPackage {
        pname = "orbit";
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
        nativeCheckInputs = [ pkgs.git pkgs.valgrind pkgs.dbus pkgs.mesa ];
        ORBIT_TEST_VULKAN_ICD = testVulkanIcd;
        ORBIT_DICTIONARY_DIR = spellingData;
        ORBIT_TEST_DICTIONARY_DIR = spellingData;
        preCheck = ''
          export VK_DRIVER_FILES="$ORBIT_TEST_VULKAN_ICD"
          unset VK_ICD_FILENAMES
          export WGPU_BACKEND=vulkan ICED_BACKEND=wgpu MESA_SHADER_CACHE_DISABLE=true
          export LD_LIBRARY_PATH="${runtimePath}:''${LD_LIBRARY_PATH:-}"
        '';
        postCheck = ''
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="${pkgs.bash}/bin/bash $PWD/scripts/check-quickjs-memory.sh" \
            cargo test --locked --offline --release --target ${pkgs.stdenv.hostPlatform.rust.rustcTarget} \
              --workspace --test quickjs_lifetime -- --test-threads=1
          bash scripts/check-quickjs-memory.sh
        '';
        cargoTestFlags = [ "--workspace" ];
        doInstallCheck = true;
        nativeInstallCheckInputs = [ pkgs.weston ];
        postInstall = ''
          install -Dm644 -t "$out/share/licenses/orbit/katex" ${./docs/licenses/katex}/*.txt
          mkdir -p "$out/share/applications" "$out/share/orbit/dictionaries"
          cp -r ${spellingData}/. "$out/share/orbit/dictionaries/"
          cp ${desktopItem}/share/applications/* "$out/share/applications/"
          for size in 16 32 48 64 128 256 512; do
            dir="$out/share/icons/hicolor/''${size}x$size/apps"
            mkdir -p "$dir"
            ${pkgs.imagemagick}/bin/magick ${./assets/icon/orbit.png} -resize "''${size}x$size" -strip "$dir/orbit.png"
          done
        '';
        postFixup = ''
          wrapProgram "$out/bin/orbit" \
            --prefix LD_LIBRARY_PATH : ${runtimePath} \
            --prefix PATH : ${lib.makeBinPath [ pkgs.git pkgs.xdg-utils ]} \
            --set-default ORBIT_DICTIONARY_DIR "$out/share/orbit/dictionaries" \
            --set-default FONTCONFIG_FILE ${fontConfig}
        '';
        installCheckPhase = ''
          runHook preInstallCheck
          bash scripts/smoke-wayland.sh "$out/bin/orbit"
          runHook postInstallCheck
        '';
        meta = {
          description = "Orbit native Wayland coding harness";
          mainProgram = "orbit";
          platforms = [ system ];
        };
      };
    in {
      packages.${system} = { default = orbit; inherit orbit; };
      apps.${system}.default = {
        type = "app";
        program = "${orbit}/bin/orbit";
      };
      checks.${system} = {
        package = orbit; # Includes unit tests and the headless Wayland window check.
        formatting = pkgs.runCommand "orbit-formatting" {
          nativeBuildInputs = with pkgs; [ cargo rustfmt ];
        } ''
          cargo fmt --manifest-path ${src}/Cargo.toml --all --check
          touch "$out"
        '';
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          dbus
          rustc cargo rustfmt clippy rust-analyzer
          pkg-config mold git just time xdg-utils valgrind
          weston wayland-utils
          imagemagick
        ];
        buildInputs = runtimeLibraries ++ [ pkgs.sqlite ];
        LD_LIBRARY_PATH = runtimePath;
        FONTCONFIG_FILE = fontConfig;
        # Expose the test driver path, but do not select it for cargo run/the app.
        ORBIT_TEST_VULKAN_ICD = testVulkanIcd;
        ORBIT_DICTIONARY_DIR = spellingData;
        ORBIT_TEST_DICTIONARY_DIR = spellingData;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        shellHook = ''
          # Locate this checkout even when entering from a package subdirectory.
          root="$PWD"
          while [[ ! -f "$root/patches/winit/default.nix" ]]; do
            if [[ "$root" == / ]]; then
              echo "Enter nix develop from the Orbit checkout to prepare its patched dependencies." >&2
              exit 1
            fi
            root="$(dirname "$root")"
          done
          mkdir -p "$root/.build-deps" &&
            ln -sfnT ${patchedWinit} "$root/.build-deps/winit" &&
            ln -sfnT ${patchedCmark} "$root/.build-deps/pulldown-cmark" &&
            ln -sfnT ${patchedIcedWinit} "$root/.build-deps/iced-winit" &&
            ln -sfnT ${patchedIcedRuntime} "$root/.build-deps/iced-runtime" &&
            ln -sfnT ${patchedIcedWidget} "$root/.build-deps/iced-widget" &&
            ln -sfnT ${patchedCosmicText} "$root/.build-deps/cosmic-text" &&
            ln -sfnT ${patchedIcedGraphics} "$root/.build-deps/iced-graphics" &&
            ln -sfnT ${patchedIcedWgpu} "$root/.build-deps/iced-wgpu" &&
            ln -sfnT ${patchedQuickjs.sys} "$root/.build-deps/rquickjs-sys" &&
            ln -sfnT ${patchedQuickjs.core} "$root/.build-deps/rquickjs-core" || {
              echo "Could not prepare .build-deps. Check checkout permissions and re-enter nix develop." >&2
              exit 1
            }
        '';
      };
    };
}
