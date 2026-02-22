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
1. Builds binaries for macOS (ARM + Intel), Linux (x86_64), and Windows (x86_64)
2. Builds `.deb` package (Debian/Ubuntu)
3. Builds `.rpm` package (Fedora/RHEL)
4. Publishes to crates.io
5. Creates a GitHub Release with all artifacts
6. Updates Homebrew formula (if enabled)
7. Updates WinGet manifest (if enabled)
8. Updates AUR package (if enabled)

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
