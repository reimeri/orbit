{ pkgs }:
pkgs.applyPatches {
  name = "orbit-iced-widget-0.14.2-patched";
  src = pkgs.fetchurl {
    name = "iced_widget-0.14.2.tar.gz";
    url = "https://static.crates.io/crates/iced_widget/iced_widget-0.14.2.crate";
    hash = "sha256-sVlq+g0xCcJhjovBK65sEdMGTfj5XELfzlcDl9vpV6s=";
  };
  patches = [ ./editor-access.patch ./editor-performance.patch ./pick-list-f32.patch ./menu-gutter.patch ./menu-click-through.patch ./markdown-table-wrap.patch ./display-math.patch ./markdown-items.patch ];
  patchFlags = [ "-p1" "--fuzz=0" ];
}
