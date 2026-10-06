{
  # Standalone flake for the Rust port, so `nix run` works directly from
  # keebs/. The root flake (../flake.nix) also exposes it as `.#keebs`.
  description = "keebs — SteamlessKeyboard Linux input core + on-screen keyboard (Rust port)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # keebs' FFI dlopens the system SDL3 libs and libusb by soname, so
          # they must be on LD_LIBRARY_PATH at runtime.
          sdlLibs = [ pkgs.sdl3 pkgs.sdl3-ttf pkgs.libusb1 ];
        in
        {
          keebs = pkgs.rustPlatform.buildRustPackage {
            pname = "keebs";
            version = "0.1.0";

            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            # hidapi (vendored C, hidraw backend) links libudev;
            # makeWrapper provides wrapProgram for the LD_LIBRARY_PATH wrapper.
            nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
            buildInputs = [ pkgs.udev ];

            doCheck = false;

            postInstall = ''
              mkdir -p $out/share/keebs/fonts
              install -Dm644 fonts/*.ttf fonts/*.txt $out/share/keebs/fonts/
              wrapProgram $out/bin/keebs \
                --set KEEBS_FONT_DIR "$out/share/keebs/fonts" \
                --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath sdlLibs}"
            '';

            meta = with pkgs.lib; {
              description = "SteamlessKeyboard Linux input core + on-screen keyboard (Rust port)";
              license = licenses.gpl3;
              platforms = platforms.linux;
              mainProgram = "keebs";
            };
          };

          default = self.packages.${system}.keebs;
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/keebs";
        };
      });

      # Dev shell: nix develop -c cargo run --release
      devShells.default = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
            pkgs.pkg-config
            pkgs.udev
            pkgs.sdl3
            pkgs.sdl3-ttf
          ];
          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.sdl3}/lib:${pkgs.sdl3-ttf}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          '';
        });
    };
}
