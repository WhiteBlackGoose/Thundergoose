{
  description = "Thundergoose: email as conversations";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;
        isLinux = pkgs.stdenv.hostPlatform.isLinux;
        x = name: old: pkgs.${name} or pkgs.xorg.${old};
        runtimeLibs = lib.optionals isLinux [
          pkgs.wayland
          pkgs.libxkbcommon
          pkgs.libGL
          pkgs.vulkan-loader
          pkgs.fontconfig
          pkgs.freetype
          (x "libx11" "libX11")
          (x "libxcursor" "libXcursor")
          (x "libxrandr" "libXrandr")
          (x "libxi" "libXi")
          (x "libxcb" "libxcb")
        ];
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer pkg-config ];
          buildInputs = runtimeLibs;
          LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "thundergoose";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [ pkg-config makeWrapper ];
          buildInputs = runtimeLibs;
          doCheck = false;
          postInstall = lib.optionalString isLinux ''
            install -Dm644 packaging/thundergoose.desktop $out/share/applications/thundergoose.desktop
            install -Dm644 packaging/thundergoose.svg $out/share/icons/hicolor/scalable/apps/thundergoose.svg
          '';
          postFixup = lib.optionalString isLinux ''
            wrapProgram $out/bin/thundergoose --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
          '';
          meta = {
            description = "Email client that groups mail into conversations with people";
            license = lib.licenses.mit;
            mainProgram = "thundergoose";
          };
        };

        apps.default = flake-utils.lib.mkApp { drv = self.packages.${system}.default; };
      });
}
