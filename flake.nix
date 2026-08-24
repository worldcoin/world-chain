{
  description = "World Chain — reproducible Nitro enclave image";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
    # rust-overlay rather than fenix: it ships the channel manifests in-tree, so resolving a
    # dated nightly is pure evaluation. fenix reads them through import-from-derivation,
    # which would make even `nix eval` require an x86_64-linux builder — the flake could then
    # not be checked from a macOS dev machine at all.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, crane, rust-overlay, ... }:
    let
      # EIFs are linux/amd64 only, so there is nothing to gain from other systems here.
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      lib = pkgs.lib;

      # The same rust-toolchain.toml everything else uses, so a channel bump moves the
      # enclave and the node together instead of drifting. rust-overlay carries the component
      # hashes, so there is no hash to paste in here and none to go stale.
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

      # Only the measured sources, and within them only files cargo actually reads. Whole
      # directories would make the derivation hash move when a README or a Dockerfile next to
      # the code changes — the build output would be identical, but every consumer keyed on
      # the store path would see a change that means nothing.
      measuredDirs = lib.fileset.unions [
        ./proofs/backends/nitro/enclave
        ./proofs/core
        ./proofs/kona/client
      ];
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          (lib.fileset.intersection measuredDirs
            (lib.fileset.fileFilter
              (file:
                file.hasExt "rs"
                || file.name == "Cargo.toml"
                || file.name == "Cargo.lock"
                # Assets embedded with include_str!/include_bytes!: the AWS Nitro root CA in
                # attestation.rs, and the certificate fixtures its tests parse. Adding a new
                # embedded asset type means adding it here — the build fails loudly if you
                # forget, but it fails on Linux CI, so check when you add one.
                || file.hasExt "pem"
                || file.hasExt "der")
              ./.))
          ./rust-toolchain.toml
        ];
      };

      manifest = "proofs/backends/nitro/enclave/Cargo.toml";

      commonArgs = {
        inherit src;
        pname = "world-chain-proof-nitro-enclave";
        version = "2.4.2";
        strictDeps = true;

        # The enclave is its own workspace; point cargo at it rather than at the repo root,
        # which is not a workspace this source tree even contains.
        cargoToml = ./proofs/backends/nitro/enclave/Cargo.toml;
        cargoLock = ./proofs/backends/nitro/enclave/Cargo.lock;
        cargoExtraArgs = lib.concatStringsSep " " [
          "--locked"
          "--manifest-path ${manifest}"
          "--features enclave"
          "--bin world-chain-proof-nitro-enclave"
        ];

        # Matches the apt list the Dockerfile installs for kona / rkyv / kzg-rs.
        nativeBuildInputs = with pkgs; [ clang cmake pkg-config ];
        buildInputs = with pkgs; [ openssl ];
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };

      # Dependencies build once and cache separately from the crate itself.
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      enclave = craneLib.buildPackage (commonArgs // { inherit cargoArtifacts; });

      # The rootfs nitro-cli measures into PCR0 and PCR2.
      #
      # Reproducible by construction, which is the whole reason this exists. There is no apt,
      # so no dpkg database, no apt logs and no ldconfig aux-cache embedding inode numbers;
      # every path is a content-addressed store path; and dockerTools stamps a fixed creation
      # time rather than "now". None of the timestamp normalisation a Dockerfile needs applies
      # here, because there are no build-time timestamps to normalise.
      enclave-image = pkgs.dockerTools.buildLayeredImage {
        name = "world-chain-proof-nitro-enclave";
        tag = "nix";

        # `created` and `mtime` default to 1970-01-01T00:00:01Z, so the image carries no
        # build-time timestamp at all.
        contents = [ enclave pkgs.cacert ];

        config = {
          Entrypoint = [ "${enclave}/bin/world-chain-proof-nitro-enclave" ];
          Env = [
            "RUST_LOG=info"
            "NITRO_VSOCK_PORT=5005"
            "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
          ];
        };
      };
    in
    {
      packages.${system} = {
        default = enclave-image;
        inherit enclave enclave-image;
      };

      # The image is linux-only, but the shell should work on whatever people develop on.
      devShells = lib.genAttrs
        [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ]
        (sys:
          let
            shellPkgs = import nixpkgs {
              system = sys;
              overlays = [ rust-overlay.overlays.default ];
            };
          in
          {
            default = shellPkgs.mkShell {
              packages = with shellPkgs; [
                (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
                clang
                cmake
                pkg-config
                openssl
                just
              ];
              LIBCLANG_PATH = "${shellPkgs.llvmPackages.libclang.lib}/lib";
            };
          });
    };
}
