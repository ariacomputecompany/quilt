{
  description = "Quilt - Lightweight container runtime";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay }: 
      let
    system = "x86_64-linux";
    
    # Standard packages
        pkgs = import nixpkgs {
          inherit system;
      overlays = [ rust-overlay.overlays.default ];
        };

    # Rust toolchain for development
    rustVersion = pkgs.rust-bin.stable."1.75.0".default.override {
      extensions = ["rust-src" "rustfmt" "clippy"];
        };

    # Standard Rust package builder
    buildRustPackage = args: pkgs.rustPlatform.buildRustPackage (args // {
      cargoLock = {
        lockFile = ./Cargo.lock;
      };
      
      nativeBuildInputs = with pkgs; [
        protobuf
        pkg-config
          ];
          
      buildInputs = with pkgs; [
        openssl.out
        zlib.out
          ];

      # Environment variables for dependencies
      PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig:${pkgs.zlib.dev}/lib/pkgconfig";
      OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
      OPENSSL_INCLUDE_DIR = "${pkgs.openssl.dev}/include";
        });

  in {
    devShells.${system}.default = pkgs.mkShell {
      buildInputs = with pkgs; [
        rustVersion
        protobuf
        pkg-config
        openssl
        zlib
        stdenv.cc
          ];

          shellHook = ''
        echo "Quilt development environment"
        echo "Use 'cargo build' to build standard binaries"
        
        export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig:${pkgs.zlib.dev}/lib/pkgconfig"
        export OPENSSL_LIB_DIR="${pkgs.openssl.out}/lib"
        export OPENSSL_INCLUDE_DIR="${pkgs.openssl.dev}/include"
          '';
        };

    packages.${system} = {
      quiltd = buildRustPackage {
            pname = "quiltd";
            version = "0.1.0";
            src = ./.;
        cargoBuildFlags = "--bin quiltd";
          };

      quilt-cli = buildRustPackage {
            pname = "quilt-cli";
            version = "0.1.0";
              src = ./.;
        cargoBuildFlags = "--bin cli";
          };

          default = self.packages.${system}.quiltd;
        };
  };
}