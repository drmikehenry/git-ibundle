# Maintainer's guide

## Setup to build

- Add targets for MUSL and Windows:

      rustup target add x86_64-unknown-linux-musl
      rustup target add x86_64-pc-windows-gnu

- Provide native toolchains; on Ubuntu:

      apt-get install -y musl-dev musl-tools
      apt-get install -y gcc-mingw-w64-x86-64

## Making a release

- Verify version is correct in `Cargo.toml`.

- Ensure everything is committed.

- Run tests.

- Test packaging, verifying the package contents:

      cargo package --list

- Build release binaries for Linux and Windows:

      ./scripts/make_release

- Create a tag as prompted by the `make_release` script, e.g.:

      If all looks good, tag this release:

        git tag -am "Release v0.1.1." v0.1.1

- Push to Github.

- Create a release at Github:

  - Visit <https://github.com/drmikehenry/git-ibundle/releases>.

  - Draft a new release.

  - Upload archives from `target/github/`:

        git-ibundle-0.1.1-x86_64-unknown-linux-musl.tar.gz
        git-ibundle-0.1.1-x86_64-pc-windows-gnu.zip

  - Add notes from `CHANGES.md` as desired.

- Publish the package to crates.io:

      cargo publish
