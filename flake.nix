{
  description = "A lightweight container runtime for agentic firmware";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        # Host packages with rust-overlay
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Target packages for musl cross-compilation
        pkgsMusl = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
          crossSystem = {
            config = "x86_64-unknown-linux-musl";
          };
        };

        # Rust toolchain for host (development)
        hostRustVersion = pkgs.rust-bin.stable."1.75.0".default.override {
          targets = ["x86_64-unknown-linux-musl"];
          extensions = ["rust-src" "rust-analyzer"];
        };

        # Rust toolchain for target (musl builds)
        targetRustVersion = pkgsMusl.rust-bin.stable."1.75.0".default.override {
          targets = ["x86_64-unknown-linux-musl"];
        };

        # Common build configuration for Rust packages
        buildRustPackageMusl = args: pkgsMusl.rustPlatform.buildRustPackage (args // {
          # Ensure we're using musl stdenv and proper environment
          stdenv = pkgsMusl.stdenv;
          
          # Critical: Set the target properly for cargo
          CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
          
          # Configure musl-specific linker and flags
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgsMusl.stdenv.cc.targetPrefix}cc";
          
          # Static linking configuration
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = [
            "-C" "target-feature=+crt-static"
            "-C" "link-arg=-static"
          ];

          # Build inputs and native build inputs
          nativeBuildInputs = (args.nativeBuildInputs or []) ++ [
            pkgs.protobuf  # protoc needs to run on host
            pkgs.pkg-config
          ];
          
          buildInputs = (args.buildInputs or []) ++ [
            pkgsMusl.openssl.out
            pkgsMusl.zlib.out
          ];

          # Environment for C compilation
          NIX_CFLAGS_COMPILE = "-I${pkgsMusl.openssl.dev}/include -I${pkgsMusl.zlib.dev}/include";
          PKG_CONFIG_ALLOW_CROSS = 1;
          OPENSSL_STATIC = 1;
          OPENSSL_LIB_DIR = "${pkgsMusl.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR = "${pkgsMusl.openssl.dev}/include";
        });

      in
      {
        # Development shell
        devShells.default = pkgs.mkShell {
          packages = [
            hostRustVersion
            pkgs.protobuf
            pkgs.pkg-config
            pkgs.cargo-watch
            pkgs.gdb
            # Cross-compilation tools
            pkgsMusl.stdenv.cc
          ];

          shellHook = ''
            echo "Quilt development environment activated"
            echo "Rust version: $(rustc --version)"
            echo "Use 'nix build .#quiltd' or 'nix build .#quilt-cli' to build musl binaries"
            
            # Set up environment for manual cargo builds
            export CARGO_BUILD_TARGET="x86_64-unknown-linux-musl"
            export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="${pkgsMusl.stdenv.cc.targetPrefix}cc"
            export PKG_CONFIG_ALLOW_CROSS=1
            export OPENSSL_STATIC=1
            export OPENSSL_LIB_DIR="${pkgsMusl.openssl.out}/lib"
            export OPENSSL_INCLUDE_DIR="${pkgsMusl.openssl.dev}/include"
          '';
        };

        # Packages to build
        packages = {
          quiltd = buildRustPackageMusl {
            pname = "quiltd";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };

          quilt-cli = buildRustPackageMusl {
            pname = "quilt-cli";
            version = "0.1.0";
            src = pkgs.lib.cleanSourceWith {
              src = ./.;
              filter = path: type:
                let baseName = baseNameOf path; in
                # Include quilt-cli directory and proto directory
                (pkgs.lib.hasPrefix (toString ./quilt-cli) path) ||
                (pkgs.lib.hasPrefix (toString ./proto) path) ||
                (baseName == "quilt-cli") ||
                (baseName == "proto");
            };
            sourceRoot = "source/quilt-cli";
            cargoLock.lockFile = ./quilt-cli/Cargo.lock;
            
            # Copy proto files into build environment
            preBuild = ''
              mkdir -p proto
              cp -r ../proto/* proto/
            '';
          };

          # Default package
          default = self.packages.${system}.quiltd;
        };

        # Apps for easy running
        apps = {
          quiltd = flake-utils.lib.mkApp {
            drv = self.packages.${system}.quiltd;
          };
          quilt-cli = flake-utils.lib.mkApp {
            drv = self.packages.${system}.quilt-cli;
          };
        };
      }
    );
}