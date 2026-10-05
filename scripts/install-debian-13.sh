#!/usr/bin/env bash
# Prepare a Debian 13 development/validation machine. Run as a regular user with sudo.
set -euo pipefail

if [[ $EUID -eq 0 ]]; then
  echo 'Run this script as a regular user with sudo access, not as root.' >&2
  exit 1
fi

# shellcheck disable=SC1091
source /etc/os-release
if [[ ${ID:-} != debian || ${VERSION_ID:-} != 13 || ${VERSION_CODENAME:-} != trixie ]]; then
  echo 'This installer supports Debian 13 (trixie) only.' >&2
  exit 1
fi

for required in sudo curl dpkg-query; do
  if ! command -v "$required" >/dev/null 2>&1; then
    echo "Missing $required. Install it with the system administrator first." >&2
    exit 1
  fi
done

sudo -v

if ! { command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; }; then
  for conflict in docker.io docker-compose docker-doc docker-buildx podman-docker containerd runc; do
    if [[ $(dpkg-query -W -f='${Status}' "$conflict" 2>/dev/null || true) == 'install ok installed' ]]; then
      echo "Conflicting package $conflict is installed. Review Docker's Debian migration procedure before rerunning; this script does not remove packages or data." >&2
      exit 1
    fi
  done
fi

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  ca-certificates curl build-essential perl nasm pkg-config xz-utils openssl jq

if ! { command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; }; then
  sudo install -m 0755 -d /etc/apt/keyrings
  sudo curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
  sudo chmod a+r /etc/apt/keyrings/docker.asc
  docker_arch=$(dpkg --print-architecture)
  printf 'Types: deb\nURIs: https://download.docker.com/linux/debian\nSuites: trixie\nComponents: stable\nArchitectures: %s\nSigned-By: /etc/apt/keyrings/docker.asc\n' "$docker_arch" | \
    sudo tee /etc/apt/sources.list.d/docker.sources >/dev/null
  sudo apt-get update
  sudo apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
fi

sudo systemctl enable --now docker

install -d -m 0755 "$HOME/.local/bin" "$HOME/.local/opt"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

if ! command -v rustup >/dev/null 2>&1; then
  rustup_installer=$(mktemp)
  trap 'rm -f "$rustup_installer"' EXIT
  curl -fsSL https://sh.rustup.rs -o "$rustup_installer"
  sh "$rustup_installer" -y --profile minimal --default-toolchain none
fi
rustup_cmd=$(command -v rustup)
"$rustup_cmd" toolchain install 1.88.0 --profile minimal --component rustfmt --component clippy
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
(cd "$project_root" && "$rustup_cmd" override set 1.88.0)

case "$(dpkg --print-architecture)" in
  amd64) node_arch=x64 ;;
  arm64) node_arch=arm64 ;;
  *) echo 'The Node.js installation in this script supports amd64 and arm64 only.' >&2; exit 1 ;;
esac

node_version=22.22.1
node_name="node-v${node_version}-linux-${node_arch}"
node_dir="$HOME/.local/opt/$node_name"
if [[ ! -x "$node_dir/bin/node" ]]; then
  download_dir=$(mktemp -d)
  trap 'rm -rf "$download_dir"; if [[ -n ${rustup_installer:-} ]]; then rm -f "$rustup_installer"; fi' EXIT
  curl -fsSLo "$download_dir/$node_name.tar.xz" \
    "https://nodejs.org/dist/v${node_version}/$node_name.tar.xz"
  curl -fsSLo "$download_dir/SHASUMS256.txt" \
    "https://nodejs.org/dist/v${node_version}/SHASUMS256.txt"
  expected_line=$(awk -v name="$node_name.tar.xz" '$2 == name { print; exit }' "$download_dir/SHASUMS256.txt")
  if [[ -z $expected_line ]]; then
    echo 'Node.js archive checksum is missing from the official manifest.' >&2
    exit 1
  fi
  (cd "$download_dir" && printf '%s\n' "$expected_line" | sha256sum --check --status)
  tar -xJf "$download_dir/$node_name.tar.xz" -C "$HOME/.local/opt"
fi

for binary in node npm npx corepack; do
  ln -sfn "$node_dir/bin/$binary" "$HOME/.local/bin/$binary"
done

corepack enable --install-directory "$HOME/.local/bin"
corepack prepare pnpm@10.28.2 --activate

profile_line='export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"'
touch "$HOME/.profile"
if ! grep -Fqx "$profile_line" "$HOME/.profile"; then
  printf '\n%s\n' "$profile_line" >>"$HOME/.profile"
fi

echo 'Installed tool versions:'
"$rustup_cmd" run 1.88.0 rustc --version
"$rustup_cmd" run 1.88.0 cargo --version
node --version
pnpm --version
docker --version
docker compose version
echo 'Docker access remains controlled by sudo; no user was added to the docker group.'
echo 'Open a new login shell (or source ~/.profile) before running project commands.'
