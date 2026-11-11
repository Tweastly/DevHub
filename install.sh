#!/bin/sh
# DevHub installer for a Linux server that may not have Docker yet (installs it first):
#
#   curl -fsSL https://raw.githubusercontent.com/Tweastly/DevHub/main/install.sh | sh
#
# On a server that has Docker, the same in one command, without this script:
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub
#
# Either way DevHub's image installs itself (`devhub install`, src/install.rs): the
# `devhub` container, the `devhub-data` and `devhub-caddy` volumes, nothing written on the
# server outside Docker. Options are passed on (see `--help`): --domain, --ip,
# --cloudflare-token, --env KEY=VALUE, --image. --yes installs Docker without asking.
set -eu

IMAGE="${DEVHUB_IMAGE:-ghcr.io/tweastly/devhub:latest}"
YES=0
for a in "$@"; do
  case "$a" in -y|--yes) YES=1 ;; esac
done

if [ -t 1 ]; then G="$(printf '\033[32m')"; Y="$(printf '\033[33m')"; R="$(printf '\033[31m')"; A="$(printf '\033[38;5;173m')"; N="$(printf '\033[0m')"
else G=""; Y=""; R=""; A=""; N=""; fi
ok() { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*"; }
die() { printf '  %s✗%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

# The only question goes through the terminal: with `curl … | sh`, the script is on stdin.
confirm() {
  [ "$YES" -eq 1 ] && return 0
  (exec </dev/tty) 2>/dev/null || return 1
  printf '  %s?%s %s [O/n] : ' "$A" "$N" "$1" >/dev/tty
  IFS= read -r a </dev/tty || a=""
  case "$a" in n|N|non|Non|no|No) return 1 ;; *) return 0 ;; esac
}

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 || die "lance ce script en root (ou installe sudo)"
  SUDO="sudo"
fi
run() { $SUDO "$@"; }

[ "$(uname -s)" = "Linux" ] || die "DevHub s'installe sur un serveur Linux"
if ! command -v docker >/dev/null 2>&1; then
  warn "Docker n'est pas installé"
  confirm "Installer Docker avec le script officiel (get.docker.com) ?" || die "installe Docker puis relance le script"
  command -v curl >/dev/null 2>&1 || die "curl est nécessaire pour installer Docker"
  curl -fsSL https://get.docker.com | run sh >/dev/null
  ok "Docker installé"
fi
if ! run docker info >/dev/null 2>&1; then
  run systemctl start docker >/dev/null 2>&1 || run service docker start >/dev/null 2>&1 || true
  run docker info >/dev/null 2>&1 || die "Docker ne répond pas (docker info)"
fi

run docker pull -q "$IMAGE" >/dev/null || die "image $IMAGE indisponible (réseau ? paquet GitHub public ?)"
TTY=""
[ -t 1 ] && TTY="-t"
# shellcheck disable=SC2086
exec $SUDO docker run --rm $TTY -v /var/run/docker.sock:/var/run/docker.sock -e DEVHUB_IMAGE="$IMAGE" "$IMAGE" install "$@"
