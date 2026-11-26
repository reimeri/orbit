{ pkgs }:
let
  cef = pkgs.cef-binary;
  wrapper = pkgs.stdenv.mkDerivation {
    pname = "orbit-cef-wrapper";
    inherit (cef) version;
    src = cef;
    nativeBuildInputs = [ pkgs.cmake pkgs.ninja ];
    installPhase = ''mkdir -p "$out/lib"; cp libcef_dll_wrapper/libcef_dll_wrapper.a "$out/lib/"'';
  };
in pkgs.stdenv.mkDerivation {
  pname = "orbit-browser";
  inherit (cef) version;
  src = ./host.cc;
  dontUnpack = true;
  buildInputs = [ pkgs.nlohmann_json pkgs.libpng ];
  buildPhase = ''
    $CXX -x c++ -std=c++20 -O2 -I${cef} "$src" -x none \
      ${wrapper}/lib/libcef_dll_wrapper.a -L${cef}/Release -lcef -lpng -pthread -ldl \
      -Wl,-rpath,'$ORIGIN' -o orbit-browser
  '';
  installPhase = ''
    mkdir -p "$out/libexec/orbit"
    cp orbit-browser "$out/libexec/orbit/"
    ln -s ${cef}/Release/* ${cef}/Resources/* "$out/libexec/orbit/"
    install -Dm644 ${cef}/LICENSE.txt "$out/share/licenses/orbit-browser/CEF.txt"
    install -Dm644 ${cef}/CREDITS.html "$out/share/licenses/orbit-browser/CREDITS.html"
  '';
}
