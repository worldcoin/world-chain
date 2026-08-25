{
  description = "World Chain — reproducible Nitro enclave image";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    optimism = {
      url = "github:ethereum-optimism/optimism/96ffbb2a94f19886fe7e27c45f3310e64ccd18b3";
      flake = false;
    };
    nitro-util = {
      url = "github:monzo/aws-nitro-util";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, crane, rust-overlay, optimism, nitro-util, ... }:
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
        ./proofs/measured/nitro-enclave
        ./proofs/measured/core
        ./proofs/measured/kona-client
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

      manifest = "proofs/measured/nitro-enclave/Cargo.toml";
      cargoLockPath = ./proofs/measured/nitro-enclave/Cargo.lock;

      # The optimism rev Cargo.lock pins, so the NUT bundles grafted into the vendor tree below
      # cannot come from a different commit than the kona crates built against them.
      lockedOptimismRev =
        let
          lock = builtins.fromTOML (builtins.readFile cargoLockPath);
          sources = lib.unique (lib.filter
            (source: source != null && lib.hasInfix "ethereum-optimism/optimism" source)
            (map (package: package.source or null) lock.package));
        in
        assert lib.assertMsg (lib.length sources == 1)
          "expected one ethereum-optimism/optimism git source in Cargo.lock, found ${toString (lib.length sources)}";
        lib.last (lib.splitString "#" (lib.head sources));

      checkedOptimism =
        assert lib.assertMsg (optimism.rev == lockedOptimismRev)
          ("flake input `optimism` is ${optimism.rev} but Cargo.lock pins ${lockedOptimismRev}"
            + " — point the input at the rev Cargo.lock uses");
        optimism;

      # kona-hardforks' build script probes its own ancestors for op-core/nuts/bundles/*.json,
      # which exists only in a monorepo checkout — cargo vendors every crate standalone. The
      # first ancestor probed is the crate directory itself, so restoring the bundles there
      # satisfies the probe without patching the build script. Nothing verifies the addition:
      # cargo writes `{"files":{}}` as the checksum manifest for vendored git crates.
      #
      # This has to hook the checkout rather than the assembled vendor directory, whose
      # config.toml points cargo back at the original store paths by absolute path.
      cargoVendorDir = craneLib.vendorCargoDeps {
        cargoLock = cargoLockPath;
        overrideVendorGitCheckout = packages: drv:
          if lib.any (package: lib.hasInfix "ethereum-optimism/optimism" (package.source or "")) packages then
            pkgs.runCommandLocal "optimism-checkout-with-nut-bundles" { } ''
              cp -R --no-preserve=mode,ownership ${drv} $out
              chmod -R u+w $out
              for crate in $out/kona-hardforks-*; do
                mkdir -p "$crate/op-core/nuts/bundles"
                cp ${checkedOptimism}/op-core/nuts/bundles/*.json "$crate/op-core/nuts/bundles/"
              done
            ''
          else
            drv;
      };

      # The proof system's release version — validated against the proofs/v* tag by
      # release-proof.yml. Deliberately independent of the crate version below: the crate
      # tracks the workspace, the proof system versions its own trust anchors.
      version = "1.0.0-rc.1";

      # The crate's own version, read from its manifest so the two never drift.
      crateVersion =
        (builtins.fromTOML (builtins.readFile ./proofs/measured/nitro-enclave/Cargo.toml))
          .workspace.package.version;

      commonArgs = {
        inherit src cargoVendorDir;
        pname = "world-chain-proof-nitro-enclave";
        version = crateVersion;
        strictDeps = true;

        # The enclave is its own workspace; point cargo at it rather than at the repo root,
        # which is not a workspace this source tree even contains.
        cargoToml = ./proofs/measured/nitro-enclave/Cargo.toml;
        cargoLock = ./proofs/measured/nitro-enclave/Cargo.lock;
        cargoExtraArgs = lib.concatStringsSep " " [
          "--locked"
          "--manifest-path ${manifest}"
          "--features enclave"
          "--bin world-chain-proof-nitro-enclave"
        ];

        # Cargo hashes the absolute workspace path into each crate's -Cmetadata (the path is
        # the SourceId of every path dependency), so the same source built in two different
        # directories produces two different binaries — and two different PCRs. Sandboxed
        # builds all run in /build; give sandbox-less builders (the CI pods, which cannot use
        # user namespaces) the same path. Their workflows pre-create /build writable; anywhere
        # /build is absent this is a no-op and the sandbox is what carries reproducibility.
        # NIX_BUILD_TOP is re-exported so the cc/ld wrappers treat /build as the build tree
        # (their purity check refuses to link objects outside it) and derive the same
        # path-mapping flags a sandboxed build gets.
        postUnpack = ''
          if [ "$NIX_BUILD_TOP" != /build ] && [ -d /build ] && [ -w /build ]; then
            rm -rf "/build/$sourceRoot"
            mv "$sourceRoot" "/build/$sourceRoot"
            cd /build
            export NIX_BUILD_TOP=/build
          fi
        '';

        # crane hands the deps-only pass a synthesized source tree carrying the lock it was
        # given at the root, but cargo looks for one next to the manifest — and this manifest
        # is a nested workspace. Without the copy, resolving the vendored git dependencies
        # fails with "requires a lock file to be present first". In the real build the two
        # files are the same lock, so the copy changes nothing.
        preBuild = ''
          cp Cargo.lock ${builtins.dirOf manifest}/Cargo.lock
        '';

        # Matches the apt list the Dockerfile installs for kona / rkyv / kzg-rs.
        nativeBuildInputs = with pkgs; [ clang cmake pkg-config ];
        buildInputs = with pkgs; [ openssl ];
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
      };

      # Dependencies build once and cache separately from the crate itself.
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      enclave = craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;

        # crane decides what to install by running `cargo metadata` from the source root, and
        # this workspace is nested — there is no manifest up there to find. Do the install from
        # the crate directory instead. The log path has to be absolutised first: the hook
        # resolves it against the working directory.
        doNotPostBuildInstallCargoBinaries = true;
        installPhaseCommand = ''
          buildLog=$(realpath "$cargoBuildLog")
          pushd ${builtins.dirOf manifest} >/dev/null
          installFromCargoBuildLog "$out" "$buildLog"
          popd >/dev/null
        '';
      });

      nitroBlobs = nitro-util.lib.${system}.blobs.x86_64;

      # The EIF itself, assembled entirely inside Nix by monzo/aws-nitro-util: deterministic
      # cpio ramdisks (sorted entries, epoch mtimes, root-owned) fed to AWS's eif_build, with
      # the same AWS-published kernel/init/nsm.ko blobs nitro-cli ships. No Docker daemon or
      # linuxkit anywhere in the measured path — converting the rootfs through a container
      # runtime made PCR0/PCR2 depend on the machine doing the conversion, which is exactly
      # what a recorded measurement cannot afford.
      eif = nitro-util.lib.${system}.buildEif {
        inherit version;
        name = "world-chain-proof-nitro-enclave";
        arch = "x86_64";
        kernel = nitroBlobs.kernel;
        kernelConfig = nitroBlobs.kernelConfig;
        nsmKo = nitroBlobs.nsmKo;
        # AWS's blob init — the same binary nitro-cli EIFs boot — rather than nitro-util's
        # from-source Go rewrite, which does not evaluate against this nixpkgs.
        init = nitroBlobs.init;
        copyToRoot = pkgs.buildEnv {
          name = "world-chain-nitro-enclave-root";
          paths = [ enclave pkgs.cacert ];
          pathsToLink = [ "/bin" "/etc" ];
        };
        entrypoint = "/bin/world-chain-proof-nitro-enclave";
        env = ''
          RUST_LOG=info
          NITRO_VSOCK_PORT=5005
          SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt
        '';
      };

      # OCI tarball of the same rootfs, published alongside releases for provenance and local
      # runs. Not on the measured path: PCR0/PCR2 come from `eif` above.
      enclave-image = pkgs.dockerTools.buildLayeredImage {
        name = "world-chain-nitro-enclave";
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
        default = eif;
        inherit enclave enclave-image eif;
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
