{
  lib,
  rustPlatform,
  installShellFiles,
}:

# b4: a command-line reader for Banking4 vault files (.sub). `b4 decrypt` turns
# a vault into a DuckDB database; the other commands query that database.
# Self-contained: DuckDB is compiled in (duckdb `bundled`), so there are no
# runtime dependencies. That makes the build take a few minutes.
rustPlatform.buildRustPackage {
  pname = "b4";
  # Keep in sync with Cargo.toml (the authoritative copy); CI asserts the two
  # agree.
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./tests
      ./examples
      # Tests check docs/schema.md against the schema definition and parse
      # record tables out of the format description.
      ./docs
    ];
  };

  # Use cargoHash, not cargoLock.lockFile (importCargoLock): crates.io 403s
  # nixpkgs' default fetchurl UA, and only the fetcher behind cargoHash sends
  # an allowed UA. Regenerate on dependency changes: set this to lib.fakeHash,
  # build, then copy the printed hash back here.
  cargoHash = "sha256-H0mSriiVP/0W4X1PhQmiC+rsxbh8r4/Lql5d281pjKg=";

  nativeBuildInputs = [
    installShellFiles # provides installManPage / installShellCompletion
  ];

  # Generate the man pages + bash/zsh/fish completions from the single clap
  # command (via the build-only b4-gen-assets helper), install them, then delete
  # the helper so only `b4` ships. Runs after the bins are in $out, so the
  # helper is available to execute.
  postInstall = ''
    assets="$(mktemp -d)"
    "$out/bin/b4-gen-assets" "$assets"

    installManPage "$assets"/*.1
    installShellCompletion \
      --bash "$assets/b4.bash" \
      --zsh "$assets/_b4" \
      --fish "$assets/b4.fish"

    rm -f "$out/bin/b4-gen-assets"
    rm -rf "$assets"
  '';

  meta = {
    description = "Read Banking4 vault files: decrypt a .sub vault into a DuckDB database and query it";
    longDescription = ''
      b4 reads the vault files (.sub) written by Subsembly's Banking4 personal
      finance software. `b4 decrypt` asks for the vault password and writes the
      accounts, bookings, payees, categories, standing orders and portfolio data
      into a DuckDB database; the remaining commands list, search and export
      from that database, so scripts and agents can work with the data without
      the Banking4 GUI. b4 only ever reads a vault and never exports the bank
      access data stored in it. It is an independent tool and is not affiliated
      with Subsembly GmbH.
    '';
    homepage = "https://github.com/ckesselh/b4";
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
    mainProgram = "b4";
  };
}
