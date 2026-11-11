#!/bin/sh
# DevHub installer. On a server that has Docker (as root, or a user of the docker group):
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub:install
#
# or, on a fresh Linux server without Docker (installs it first):
#
#   curl -fsSL https://raw.githubusercontent.com/Tweastly/DevHub/main/install.sh | sh
#
# Everything stays in Docker: the `devhub` container (DevHub and its HTTPS front, Caddy),
# its data in the `devhub-data` volume, its certificates in `devhub-caddy`, its settings in
# the container itself. Nothing is written on the server outside Docker. It starts on the
# server's IP and prints the address of the installation guide and its code: the domain
# name, the account and the rest are chosen in the browser.
# Run it again to update DevHub or change a setting: data and settings are kept. An older
# installation made with docker compose (a folder such as /opt/devhub) moves into the
# volumes; its folder is left for you to delete.
#
# Options (or the matching environment variables):
#   --domain dev.example.com     DEVHUB_DOMAIN         start on this domain (projects: *.dev.example.com)
#   --ip                                               back to the server's IP, without a domain
#   --cloudflare-token TOKEN     CLOUDFLARE_API_TOKEN  wildcard certificate through Cloudflare's DNS
#   --env KEY=VALUE                                    any other DevHub setting (repeatable)
#   --image IMAGE                DEVHUB_IMAGE          DevHub's image
#                                DEVHUB_PUBLIC_IP      the server's public IP, when it can't be detected
#   --yes                                              install Docker without asking, if it's missing
set -eu

IMAGE="${DEVHUB_IMAGE:-ghcr.io/tweastly/devhub:latest}"
NAME="devhub"
DOMAIN="${DEVHUB_DOMAIN:-}"
# domain, ip, or empty: an existing installation keeps its mode, a new one starts on the IP.
MODE=""
CF_TOKEN="${CLOUDFLARE_API_TOKEN:-}"
# Settings given to the container, one KEY=VALUE per line.
SETS=""
YES=0
IN_DOCKER="${DEVHUB_IN_DOCKER:-0}"

# ---------- Output ----------
if [ -t 1 ]; then
  B="$(printf '\033[1m')"; D="$(printf '\033[2m')"; G="$(printf '\033[32m')"; Y="$(printf '\033[33m')"
  R="$(printf '\033[31m')"; A="$(printf '\033[38;5;173m')"; N="$(printf '\033[0m')"
else
  B=""; D=""; G=""; Y=""; R=""; A=""; N=""
fi
ok() { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*"; }
info() { printf '  %s…%s %s\n' "$D" "$N" "$*"; }
die() { printf '  %s✗%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

usage() {
  awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0" 2>/dev/null || true
  echo "Voir https://github.com/Tweastly/DevHub"
}
add() { SETS="$SETS$1
"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="${2:-}"; MODE=domain; shift 2 ;;
    --domain=*) DOMAIN="${1#*=}"; MODE=domain; shift ;;
    --ip) MODE=ip; shift ;;
    --cloudflare-token) CF_TOKEN="${2:-}"; shift 2 ;;
    --cloudflare-token=*) CF_TOKEN="${1#*=}"; shift ;;
    --env) case "${2:-}" in *=*) add "$2" ;; *) die "--env KEY=VALUE attendu" ;; esac; shift 2 ;;
    --image) IMAGE="${2:-}"; shift 2 ;;
    --dir|--branch) warn "$1 ignoré : DevHub tient entièrement dans Docker"; shift 2 ;;
    -y|--yes) YES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "option inconnue : $1 (voir --help)" ;;
  esac
done
[ -z "$DOMAIN" ] || [ -n "$MODE" ] || MODE=domain

# The only question (installing Docker) goes through the terminal: with `curl … | sh`, the
# script itself is on stdin.
TTY=0
if [ "$YES" -eq 0 ] && (exec </dev/tty) 2>/dev/null; then TTY=1; fi
confirm() { # confirm "question" → yes by default
  [ "$YES" -eq 1 ] && return 0
  [ "$TTY" -eq 1 ] || return 1
  printf '  %s?%s %s [O/n] : ' "$A" "$N" "$1" >/dev/tty
  IFS= read -r a </dev/tty || a=""
  case "$a" in n|N|non|Non|no|No) return 1 ;; *) return 0 ;; esac
}

SUDO=""
if [ "$IN_DOCKER" != "1" ] && [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 || die "lance ce script en root (ou installe sudo)"
  SUDO="sudo"
fi
run() { $SUDO "$@"; }

printf '\n  %sDevHub%s · installation\n\n' "$B" "$N"
[ "$(uname -s)" = "Linux" ] || die "DevHub s'installe sur un serveur Linux"

# fetch URL: prints the body (curl, or BusyBox wget in the installer image).
fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsS4 --max-time 6 "$1"
  else wget -q -T 6 -O - "$1"; fi
}

# ---------- Docker ----------
if [ "$IN_DOCKER" = "1" ]; then
  docker info >/dev/null 2>&1 || die "Docker injoignable : ajoute -v /var/run/docker.sock:/var/run/docker.sock à la commande docker run"
elif ! command -v docker >/dev/null 2>&1; then
  warn "Docker n'est pas installé"
  confirm "Installer Docker avec le script officiel (get.docker.com) ?" || die "installe Docker puis relance le script"
  command -v curl >/dev/null 2>&1 || die "curl est nécessaire pour installer Docker"
  curl -fsSL https://get.docker.com | run sh >/dev/null
fi
if ! run docker info >/dev/null 2>&1; then
  run systemctl start docker >/dev/null 2>&1 || run service docker start >/dev/null 2>&1 || true
  run docker info >/dev/null 2>&1 || die "Docker ne répond pas (docker info)"
fi
ok "Docker $(run docker version --format '{{.Server.Version}}' 2>/dev/null || echo '?') trouvé"

# ---------- Existing installation ----------
EXISTS=0
OLD_DIR=""
OLD_DOMAIN=""
if run docker inspect "$NAME" >/dev/null 2>&1; then
  EXISTS=1
  OLD_DOMAIN="$(run docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$NAME" | sed -n 's/^DEVHUB_DOMAIN=//p' | head -n 1)"
  OLD_DIR="$(run docker inspect -f '{{index .Config.Labels "com.docker.compose.project.working_dir"}}' "$NAME" 2>/dev/null || true)"
  [ "$OLD_DIR" != "<no value>" ] || OLD_DIR=""
  if [ -n "$OLD_DIR" ]; then
    ok "DevHub installé avec docker compose dans $OLD_DIR : il passe dans des volumes Docker (données et réglages gardés)"
  else
    ok "DevHub déjà installé : mise à jour (données et réglages gardés)"
  fi
fi

# ---------- Address ----------
is_ip() { printf '%s' "$1" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$'; }
PUBLIC_IP="${DEVHUB_PUBLIC_IP:-}"
[ -n "$PUBLIC_IP" ] || PUBLIC_IP="$(fetch https://api.ipify.org 2>/dev/null || fetch https://ifconfig.me 2>/dev/null || true)"
is_ip "$PUBLIC_IP" || PUBLIC_IP=""
[ -n "$MODE" ] || [ "$EXISTS" -eq 1 ] || MODE=ip
case "$MODE" in
  ip)
    [ -n "$PUBLIC_IP" ] || die "adresse IP publique introuvable : indique-la avec DEVHUB_PUBLIC_IP=…, ou un domaine avec --domain"
    DOMAIN="$PUBLIC_IP"
    add "DEVHUB_DOMAIN=$DOMAIN"
    if [ "$EXISTS" -eq 0 ]; then
      ok "DevHub démarrera sur l'adresse du serveur, ${B}$DOMAIN$N : le nom de domaine se choisit ensuite dans le navigateur"
    else
      ok "Sans nom de domaine : panel sur ${B}https://$DOMAIN$N"
    fi
    ;;
  domain)
    DOMAIN="$(printf '%s' "$DOMAIN" | tr '[:upper:]' '[:lower:]' | sed 's#^https\{0,1\}://##; s#/.*$##')"
    [ "$DOMAIN" != "dev.example.com" ] || die "dev.example.com est un exemple : mets ton propre domaine"
    printf '%s' "$DOMAIN" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$' || die "domaine invalide : $DOMAIN"
    add "DEVHUB_DOMAIN=$DOMAIN"
    ok "Domaine du panel : $B$DOMAIN$N"
    ;;
  *)
    DOMAIN="$OLD_DOMAIN"
    ok "Adresse gardée : $B$DOMAIN$N"
    ;;
esac
if [ -n "$CF_TOKEN" ]; then
  add "DEVHUB_TLS=wildcard"
  add "CLOUDFLARE_API_TOKEN=$CF_TOKEN"
fi

# ---------- DNS (with a domain given here) ----------
resolve() {
  { getent ahostsv4 "$1" 2>/dev/null || nslookup "$1" 2>/dev/null | sed -n 's/^Address[^:]*: *\([0-9.]*\)$/\1/p'; } | awk 'NF { print $1; exit }'
}
check_dns() { # check_dns name label
  ip="$(resolve "$1" || true)"
  if [ -z "$ip" ]; then
    warn "DNS  $2 ne se résout pas : ajoute un enregistrement A vers ${PUBLIC_IP:-l'IP du serveur}"
  elif [ -n "$PUBLIC_IP" ] && [ "$ip" != "$PUBLIC_IP" ]; then
    warn "DNS  $2 → $ip, mais ce serveur est $PUBLIC_IP (proxy Cloudflare ? mets-le en « DNS only »)"
  else
    ok "DNS  $2 → $ip ${D}(ce serveur)$N"
  fi
}
if [ "$MODE" = domain ]; then
  check_dns "$DOMAIN" "$DOMAIN"
  check_dns "devhub-install-check.$DOMAIN" "*.$DOMAIN"
fi

# ---------- Ports ----------
# Only checked: the server's firewall is not DevHub's to change.
PORTS="80 et 443"
if is_ip "$DOMAIN"; then PORTS="80, 443 et 20000-20099"; fi
if [ "$IN_DOCKER" != "1" ] && [ "$EXISTS" -eq 0 ] && command -v ss >/dev/null 2>&1; then
  BUSY=""
  for p in 80 443; do
    if run ss -ltnH 2>/dev/null | awk '{ print $4 }' | grep -Eq "[:.]$p\$"; then BUSY="$BUSY $p"; fi
  done
  if [ -n "$BUSY" ]; then warn "port(s)$BUSY déjà utilisé(s) sur ce serveur (un autre serveur web ?) : Caddy ne pourra pas démarrer"
  else ok "Ports 80 et 443 libres"; fi
fi
info "Ouvre les ports $PORTS (TCP) dans le pare-feu du serveur ou de l'hébergeur, s'il y en a un"

# ---------- Image ----------
info "Téléchargement de DevHub ($IMAGE)"
run docker pull -q "$IMAGE" >/dev/null || die "image $IMAGE indisponible (réseau ? paquet GitHub public ?)"
ok "Image à jour"

# ---------- Container ----------
# `devhub deploy`, from DevHub's image: creates the container and its volumes, or replaces
# the existing one with its data and settings (and moves a compose installation in).
set -- deploy --container "$NAME" --image "$IMAGE"
while IFS= read -r line; do
  [ -z "$line" ] || set -- "$@" --env "$line"
done <<EOF
$SETS
EOF
if [ "$MODE" = domain ]; then set -- "$@" --unset DEVHUB_PORT_RANGE; fi
run docker rm -f devhub-installer >/dev/null 2>&1 || true
run docker run --rm --name devhub-installer -v /var/run/docker.sock:/var/run/docker.sock "$IMAGE" devhub "$@" 2>&1 | sed "s/^/  $D│$N /" || true
STATE="$(run docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)"
[ "$STATE" = "true" ] || die "DevHub ne tourne pas : voir ci-dessus, et docker logs $NAME"
ok "DevHub tourne (un seul conteneur, Caddy compris)"

# ---------- Installation code ----------
CODE=""
READY=0
i=0
while [ "$i" -lt 30 ]; do
  LOGS="$(run docker logs "$NAME" 2>&1 || true)"
  CODE="$(printf '%s\n' "$LOGS" | sed -n "s/.*CODE D'INSTALLATION : \([A-Z0-9-]*\).*/\1/p" | tail -n 1)"
  [ -n "$CODE" ] && break
  if printf '%s\n' "$LOGS" | grep -q "listening on"; then READY=1; fi
  [ "$READY" -eq 1 ] && [ "$i" -ge 2 ] && break
  i=$((i + 1))
  sleep 2
done

URL="https://$DOMAIN"
LINE="  $D────────────────────────────────────────────────$N"
echo
echo "$LINE"
if [ -n "$CODE" ]; then
  printf '  La suite se passe dans ton navigateur :\n\n'
  printf '  Ouvre   %s%s/setup%s\n' "$A$B" "$URL" "$N"
  printf '  Code    %s%s%s  %s(une seule fois)%s\n\n' "$A$B" "$CODE" "$N" "$D" "$N"
  if is_ip "$DOMAIN"; then
    printf '  %sLe guide propose d'"'"'abord de passer sur ton nom de domaine, si tu en as un.\n  Sur l'"'"'adresse IP, le navigateur peut afficher un avertissement de certificat la\n  première fois (certificat de secours de Caddy) : tu peux continuer.%s\n' "$D" "$N"
  fi
else
  printf '  DevHub est prêt : %s%s%s\n' "$A$B" "$URL" "$N"
fi
if [ -n "$OLD_DIR" ]; then
  printf '\n  %sL'"'"'ancien dossier %s ne sert plus : vérifie que tout va bien, puis\n  supprime-le sur le serveur (sudo rm -rf %s).%s\n' "$D" "$OLD_DIR" "$OLD_DIR" "$N"
fi
echo "$LINE"
printf '  %sDonnées dans les volumes Docker devhub-data et devhub-caddy · journaux : docker logs devhub%s\n\n' "$D" "$N"
