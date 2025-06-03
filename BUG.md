# BUG: Nix Cross-Compilation to Musl Fails with Linker Errors

## 1. Issue Description

When attempting to build the `quiltd` (main rust binary) and `quilt-cli` (rust binary) packages for the `x86_64-unknown-linux-musl` target using Nix Flakes, the build process fails during the linking stage. 

The primary error observed is related to `proc-macro2` and `libc` (and their build scripts) failing to compile/link, with underlying linker errors indicating that `glibc` symbols are being referenced or that standard C libraries (`libutil`, `librt`, `libpthread`, `libm`, `libdl`, `libc`) cannot be found for static linking by `musl-gcc`.

The key linker error line often includes:
`undefined reference to symbol 'gnu_get_libc_version@@GLIBC_2.2.5'`
followed by:
`/nix/store/.../glibc-2.40-66/lib/libc.so.6: error adding symbols: DSO missing from command line`
and also:
`cannot find -lutil: No such file or directory` (and similar for other standard C libs).

This suggests that even though the overall target is `musl`, some part of the build process (likely build scripts or proc-macros) is being compiled or linked against the host's `glibc` environment.

A notable observation from the build logs is the line:
`quiltd> cargoBuildHook flags: -j 2 --target x86_64-unknown-linux-gnu --offline --profile release`
This appears even when `target = "x86_64-unknown-linux-musl";` is set in the `buildRustPackage` definition, indicating the hook might not be respecting the target for all its operations.

## 2. Environment

*   **Host Build System:** Ubuntu (DigitalOcean Droplet, initially 1GB RAM, later upgraded to 8GB RAM which resolved OOM issues but not this linking issue).
*   **Nix Installation:** Multi-user installation, flakes enabled.
*   **Target:** `x86_64-unknown-linux-musl` for fully static binaries.
*   **Project Structure:** Main Rust binary (`quiltd`) at project root, `quilt-cli` in a subdirectory. Both use `tonic-build` in their `build.rs` to compile protobufs.

## 3. `flake.nix` Configuration (Relevant Parts)

```nix
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
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        rustVersion = pkgs.rust-bin.stable."latest".default.override {
          targets = ["x86_64-unknown-linux-musl"];
          extensions = ["rust-src"];
        };
        pkgsMusl = import nixpkgs {
          inherit system;
          crossSystem = {
            config = "x86_64-unknown-linux-musl";
          };
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustVersion                   
            pkgsMusl.stdenv.cc            
            pkgsMusl.stdenv.cc.bintools   
            pkgs.protobuf                 
            pkgs.pkg-config               
            pkgs.openssl.dev              
            pkgs.gdb                      
            pkgs.cargo-watch              
            pkgs.coreutils                
          ];
          shellHook = ''''
            # ... (original complex shellHook was simplified for debugging, but issue persists)
            # Key parts that were in shellHook and moved to package defs:
            # export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="${pkgsMusl.stdenv.cc}/bin/musl-gcc"
            # export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static ..."
            echo "Nix dev shell for Quilt (musl target) activated."
          '''';
        };

        packages = {
          quiltd = pkgs.rustPlatform.buildRustPackage {
            pname = "quiltd";
            version = "0.1.0"; 
            src = ./.; 
            cargoLock.lockFile = ./Cargo.lock;
            stdenv = pkgsMusl.stdenv;            
            target = "x86_64-unknown-linux-musl";

            NIX_CFLAGS_COMPILE = "-I${pkgsMusl.openssl.dev}/include -I${pkgsMusl.zlib}/include";
            RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-static -L ${pkgsMusl.openssl.out}/lib -L ${pkgsMusl.zlib.out}/lib";
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgsMusl.stdenv.cc}/bin/musl-gcc";
            
            nativeBuildInputs = [ pkgs.pkg-config pkgsMusl.stdenv.cc pkgs.protobuf ]; 
            buildInputs = [ pkgsMusl.openssl pkgsMusl.zlib pkgsMusl.stdenv.cc.libc ]; 
            supportedSystems = ["x86_64-linux"];
          };
          # quilt-cli definition is similar
        };
      }
    )
  # Removed potential extraneous semicolon here during debugging
}
```

## 4. Troubleshooting Steps Attempted

1.  **Initial Cross-Compilation Attempt (Cargo only):**
    *   Installed `rustup target add x86_64-unknown-linux-musl`.
    *   Installed `musl-tools` on host (Ubuntu).
    *   Created `.cargo/config.toml` specifying `linker = "musl-gcc"` and `rustflags = ["-C", "target-feature=+crt-static"]`.
    *   `cargo build --target x86_64-unknown-linux-musl` failed with segfaults when running the binary on Alpine (via Docker), even with `libc6-compat`.

2.  **Transition to Nix Flakes:**
    *   Installed Nix (multi-user) and enabled flakes.
    *   Created `flake.nix` (multiple iterations).
    *   **Syntax Errors:** Resolved several syntax errors in `flake.nix` related to semicolons and attribute set structure.
    *   **OOM Errors:** Resolved OOM killer issues by upgrading VM RAM from 1GB to 8GB.
    *   **`protoc` Not Found:** Resolved by adding `pkgs.protobuf` to `nativeBuildInputs` of the rust packages.
    *   **Current Linker Errors (`gnu_get_libc_version`, `cannot find -lutil`, etc.):**
        *   Ensured `stdenv = pkgsMusl.stdenv;` is set for the packages.
        *   Set `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgsMusl.stdenv.cc}/bin/musl-gcc";`.
        *   Set `RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=-static ...";`.
        *   Added `pkgsMusl.openssl`, `pkgsMusl.zlib`, and eventually `pkgsMusl.stdenv.cc.libc` to `buildInputs`.
        *   Explicitly set `target = "x86_64-unknown-linux-musl";` in the `buildRustPackage` arguments.

None of these Nix configurations have fully resolved the final linker error related to `glibc` symbols and missing static versions of standard C libraries when linking the `musl` target.

## 5. Hypothesis

The `rustPlatform.buildRustPackage` or its underlying `cargoBuildHook` is not correctly using the `musl` environment (`pkgsMusl.stdenv`) for *all* compilation stages, particularly for build scripts and proc-macros. These may still be picking up the host's `x86_64-unknown-linux-gnu` target and `glibc`, leading to incompatible object files or linking expectations when the final `musl` binary is assembled.

The explicit `cargoBuildHook flags: ... --target x86_64-unknown-linux-gnu ...` in the logs, despite our settings, strongly points to this.

## 6. Next Steps Considered

*   Investigate how to force `rustPlatform.buildRustPackage` and its hooks to use the `x86_64-unknown-linux-musl` target consistently for all Rust compilations (including build scripts and proc-macros).
*   Explore alternative ways of defining the Rust build for `musl` in Nix, potentially using `pkgs.makeRustPlatform` with more explicit control, or looking for community examples of `rust-overlay` usage for static musl binaries that handle build scripts correctly.
*   Consider if any specific crate has a build script that is particularly problematic in cross-compilation scenarios and might need patching or specific environment variables. 