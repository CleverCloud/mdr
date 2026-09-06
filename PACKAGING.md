# Packaging & Release Setup Guide

This document describes how to set up secrets and external repos for automated releases.

## GitHub Actions Secrets

Configure these in **GitHub repo → Settings → Secrets and variables → Actions → Secrets**:

| Secret | How to get it | Used by |
|--------|--------------|---------|
| `CARGO_REGISTRY_TOKEN` | crates.io → Settings → Tokens → New Token (publish-update) | crates.io publish |
| `HOMEBREW_TAP_TOKEN` | GitHub PAT with write access to `CleverCloud/homebrew-misc` | Homebrew formula update |
| `WINGET_TOKEN` | GitHub classic PAT with `public_repo` scope | WinGet package update |
| `AUR_SSH_PRIVATE_KEY` | SSH key registered on aur.archlinux.org | AUR package update |

### Optional Variables

Configure in **GitHub repo → Settings → Secrets and variables → Actions → Variables**:

| Variable | Value | Purpose |
|----------|-------|---------|
| `HOMEBREW_TAP_ENABLED` | `true` | Enable Homebrew tap updates on release |
| `WINGET_ENABLED` | `true` | Enable WinGet package updates on release |
| `AUR_ENABLED` | `true` | Enable AUR package updates on release |

## Repos to Create

### `CleverCloud/homebrew-misc`

Homebrew tap for Clever Cloud tools.

1. Create the repo `CleverCloud/homebrew-misc` on GitHub
2. Initialize with a `Formula/` directory
3. Users install with: `brew tap CleverCloud/misc && brew install mdr`

## Setting Up Homebrew Tap Token

1. Go to **GitHub Settings → Developer settings → Personal access tokens → Fine-grained tokens**
2. Click **"Generate new token"**
3. Name: `mdr-homebrew`
4. Resource owner: **CleverCloud**
5. Repository access: **Only select** `CleverCloud/homebrew-misc`
6. Permissions: **Contents: Read and write**
7. Copy token → add as `HOMEBREW_TAP_TOKEN` secret in mdr repo

## Creating a Release

```bash
# Tag the release
git tag v0.1.0
git push origin v0.1.0
```

This triggers the release workflow which:
1. Builds binaries for macOS (ARM + Intel), Linux (x86_64 + aarch64), and Windows (x86_64)
2. Builds `.deb` packages (Debian/Ubuntu) for `amd64` and `arm64`
3. Builds `.rpm` packages (Fedora/RHEL) for `x86_64` and `aarch64`
4. Publishes to crates.io
5. Creates a GitHub Release with all artifacts, using the `CHANGELOG.md` section
   of the tag as release notes
6. Updates Homebrew formula (if enabled)
7. Updates WinGet manifest (if enabled)
8. Updates AUR package (if enabled)

### Release notes

The release body comes from `CHANGELOG.md`: the workflow extracts the
`## [x.y.z]` section matching the tag (minus the leading `v`) and appends a
"Full Changelog" compare link to the previous tag. The notes file is rebuilt
from scratch on every run, so re-running the workflow for the same tag
*replaces* the body instead of appending to it.

If no section matches the tag, the step emits a warning and falls back to
GitHub's auto-generated release notes — it never fails the release. So: add the
`## [x.y.z] - YYYY-MM-DD` section to `CHANGELOG.md` **before** pushing the tag.

### Linux aarch64

Linux ARM64 binaries and packages are built natively on the GitHub-hosted
`ubuntu-24.04-arm` runners. Cross-compiling from x86_64 was rejected: the
project links against GTK 3, WebKitGTK and OpenGL, which would require a full
aarch64 sysroot with those `-dev` packages.

Limitations:
- `ubuntu-24.04-arm` runners are free for public repositories; on a private
  repo they are billed and may need to be enabled for the organisation.
- The AUR package (`mdr-bin`) is still `x86_64`-only.

### Desktop integration in .deb / .rpm

Both packages install, in addition to `/usr/bin/mdr`:

| File | Destination |
|------|-------------|
| `assets/mdr.desktop` | `/usr/share/applications/mdr.desktop` |
| `assets/logo-128.png` | `/usr/share/icons/hicolor/128x128/apps/mdr.png` |
| `assets/mdr.1` | `/usr/share/man/man1/mdr.1` |
| `README.md` | `/usr/share/doc/mdr/README.md` |

The man page is a hand-written roff file rather than a `clap_mangen` build
script: the `Cli` struct lives in `src/main.rs` and cannot be reused from a
build script without duplicating it. **Keep `assets/mdr.1` in sync when adding
or changing a CLI flag** (check with `mdr --help`, lint with
`mandoc -T lint assets/mdr.1`).

## Installing the .deb / .rpm packages

`mdr` is a GUI application and pulls in GTK 3, WebKitGTK, libxdo and OpenGL.
**Use a package manager that resolves dependencies**, not the low-level
installer:

```bash
# Fedora / RHEL / openSUSE
sudo dnf install ./mdr-0.3.2-1.x86_64.rpm

# Debian / Ubuntu
sudo apt install ./mdr_0.3.2-1_amd64.deb
```

`sudo rpm -i mdr-*.rpm` (and likewise `sudo dpkg -i mdr_*.deb`) **only
installs** — it never fetches anything — so it aborts on the first missing
capability:

```
error: Failed dependencies:
	libxdo.so.3()(64bit) is needed by mdr-0.3.2-1.x86_64
```

That message means the package is doing its job: the dependency *is* declared,
and `libxdo.so.3` is provided on Fedora by `xdotool-libs`. `dnf install ./…`
pulls it in automatically. (`rpm -i` can also be used after installing the
dependencies by hand, but there is no reason to.)

### How the dependency list is built

`cargo-deb` (`depends = "$auto"`) and `cargo-generate-rpm` (`auto-req`, left at
its default `auto`) both derive the dependency list from the shared libraries
the binary **links** against — `ldd`-style. That covers GTK 3, WebKitGTK,
JavaScriptCore, libsoup3, libxdo and glib.

It does *not* cover libraries opened with `dlopen()` at runtime, and `mdr` has
some: `x11-dl` and `khronos-egl` (through `winit`/`tao`/`wgpu`) load
`libGL.so.1` and `libEGL.so.1` dynamically. They are therefore declared
explicitly in `Cargo.toml`:

- `.deb`: `depends = "$auto, libgl1, libegl1"`
- `.rpm`: `[package.metadata.generate-rpm.requires]` with
  `libGL.so.1()(64bit)` and `libEGL.so.1()(64bit)` (provided by `mesa-libGL`
  and `mesa-libEGL` on Fedora)

X11 and Wayland client libraries are deliberately *not* hard requirements:
`libX11` already comes transitively with `gtk3`, and Wayland support must stay
optional.

The release workflow prints the final dependency list of every package
(`dpkg-deb -f … Depends` and `rpm -qp --requires`), so a regression is visible
in the run logs. Note the ordering trap in the `.rpm` job: `rpm` is installed
*after* `cargo generate-rpm`, because the mere presence of
`/usr/lib/rpm/find-requires` switches `cargo-generate-rpm` away from its
builtin `auto-req`.

## Nix Flake

Users can install directly with:

```bash
nix run github:CleverCloud/mdr
```

Or add to a flake:

```nix
{
  inputs.mdr.url = "github:CleverCloud/mdr";
}
```

The derivation reads its `pname`, `version`, `description` and `homepage`
straight from `Cargo.toml` (`builtins.fromTOML`), so they cannot drift from the
crate. Dependencies come from the tracked `Cargo.lock` via
`cargoLock.lockFile`, so no `cargoHash` has to be updated on a bump.

## MSRV

`rust-version` in `Cargo.toml` declares the minimum supported Rust version. Two
CI jobs relate to it:

- `fmt` ("Format & manifest") fails if `rust-version` and the `MSRV` variable of
  `ci.yml` disagree — a purely textual check that reports in seconds. Bump both
  together.
- `msrv` pins that toolchain and runs `cargo check --all-features --all-targets`
  plus `cargo test --all-features`. Both jobs are blocking.

1.95 is the highest `rust-version` declared in the dependency tree (`kdl` 6.7.1;
next highest is 1.92 for the egui/epaint family, then 1.88 for `ratatui` 0.30
and `image` 0.25, then 1.85 for `clap` 4 / `comrak` 0.52 / `ureq` 3). It was
confirmed by an actual build: `cargo check --all-features --all-targets` on a
1.95.0 toolchain exits 0.

Recompute the floor after a dependency bump with:

```bash
cargo metadata --format-version 1 --all-features \
  | jq -r '.packages[] | select(.rust_version) | .rust_version' \
  | sort -V | tail -1
```

Note that this is a *declared* floor: 242 of the 821 resolved packages declare
no `rust-version` at all (including the direct dependencies
`mermaid-rs-renderer` and `tiny-skia`), so the `msrv` job — not this command —
is what actually proves the value.

## crates.io

Automatically published on each release via `cargo publish`.

Setup:
1. Go to https://crates.io/settings/tokens
2. "New Token" → name: `mdr-ci` → scope: publish-update → crate: `mdr`
3. Add as `CARGO_REGISTRY_TOKEN` secret

## WinGet (Windows Package Manager)

Automatically updates the WinGet manifest on each release.

Setup:
1. First release: manually submit `CleverCloud.mdr` to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) via PR
2. Go to **GitHub Settings → Developer settings → Personal access tokens → Tokens (classic)**
3. Generate new token with `public_repo` scope
4. Add as `WINGET_TOKEN` secret
5. Set `WINGET_ENABLED` variable to `true`

Users install with: `winget install CleverCloud.mdr`

## AUR (Arch Linux)

Automatically updates the `mdr-bin` AUR package on each release.

Setup:
1. Create an account on https://aur.archlinux.org
2. Generate an SSH key: `ssh-keygen -t ed25519 -f ~/.ssh/aur -C "mdr-aur"`
3. Add the public key to AUR: My Account → SSH Public Keys
4. Create the `mdr-bin` AUR package (first time, manually via `git clone ssh://aur@aur.archlinux.org/mdr-bin.git`)
5. Add the private key as `AUR_SSH_PRIVATE_KEY` secret
6. Set `AUR_ENABLED` variable to `true`

Users install with: `yay -S mdr-bin`
