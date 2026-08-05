{ pkgs ? import <nixpkgs> { } }:

let
  runtimeLibs = with pkgs; [
    alsa-lib
    libglvnd
    libxkbcommon
    libx11
    libxcursor
    libxi
    libxrandr
    wayland
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.alsa-lib ];

  # winit, glutin and x11-dl dlopen their libraries by soname, so nothing links
  # them and rustc emits no RUNPATH to find them by. The GL vendor lib lives
  # outside the store, under the driver path NixOS maintains.
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs ++ [ "/run/opengl-driver" ]);
}
