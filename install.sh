#!/bin/sh
# DevHub installer. On a fresh Linux server, as root (or a user with sudo):
#
#   curl -fsSL https://raw.githubusercontent.com/Tweastly/DevHub/main/install.sh | sh
#
# or, on a server that already has Docker, from its image (installer/Dockerfile):
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock -v /opt/devhub:/opt/devhub ghcr.io/tweastly/devhub:install
#
# Checks Docker (installs it if needed), fetches DevHub into /opt/devhub, pulls its images
# from GitHub (or builds them) and starts it on the server's IP address, then prints the
# address of the installation guide and its code. Nothing else is asked here: the domain
# name, the account and the rest are chosen in the browser, in the guide.
# Run it again to update an existing installation: the .env is kept.
#
# Options (or the matching environment variables, for unattended installs):
#   --domain dev.example.com     DEVHUB_DOMAIN       start on this domain (projects: *.dev.example.com)
#   --ip                                             back to the server's IP, without a domain
#   --dir /opt/devhub            DEVHUB_DIR          installation folder
#   --branch main                DEVHUB_BRANCH       branch to follow
#   --cloudflare-token TOKEN     CLOUDFLARE_API_TOKEN with --domain: wildcard certificate (Cloudflare DNS)
#                                DEVHUB_PUBLIC_IP    the server's public IP, when it can't be detected
#   --yes                                            install Docker without asking, if it's missing
set -eu

REPO_URL="${DEVHUB_REPO_URL:-https://github.com/Tweastly/DevHub}"
IMAGE="${DEVHUB_IMAGE:-ghcr.io/tweastly/devhub:latest}"
CADDY_IMAGE="${DEVHUB_CADDY_IMAGE:-ghcr.io/tweastly/devhub:caddy}"
DIR="${DEVHUB_DIR:-/opt/devhub}"
BRANCH="${DEVHUB_BRANCH:-main}"
DOMAIN="${DEVHUB_DOMAIN:-}"
# domain (a name: passkeys) or ip (no domain: the server's address, password).
MODE="${DEVHUB_MODE:-}"
# Without a domain, the web projects' ports (one per project, opened on the server).
IP_RANGE="${DEVHUB_PORT_RANGE:-20000-20099}"
CF_TOKEN="${CLOUDFLARE_API_TOKEN:-}"
YES=0
# Run from the installer image: the server's Docker comes through its socket and DIR is
# a folder of the server mounted at the same path.
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

while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="${2:-}"; shift 2 ;;
    --domain=*) DOMAIN="${1#*=}"; shift ;;
    --ip) MODE=ip; shift ;;
    --dir) DIR="${2:-}"; shift 2 ;;
    --dir=*) DIR="${1#*=}"; shift ;;
    --branch) BRANCH="${2:-}"; shift 2 ;;
    --branch=*) BRANCH="${1#*=}"; shift ;;
    --cloudflare-token) CF_TOKEN="${2:-}"; shift 2 ;;
    --cloudflare-token=*) CF_TOKEN="${1#*=}"; shift ;;
    -y|--yes) YES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "option inconnue : $1 (voir --help)" ;;
  esac
done

# A domain or mode given to this run rewrites those lines of an existing .env.
CHOSEN=0
[ -z "$DOMAIN$MODE" ] || CHOSEN=1

# The few questions (installing Docker, an IP that can't be found) go through the terminal:
# with `curl … | sh`, the script itself is on stdin.
TTY=0
if [ "$YES" -eq 0 ] && (exec </dev/tty) 2>/dev/null; then TTY=1; fi

ask() { # ask VAR "question" [default] [secret]
  ans=""
  if [ "$TTY" -eq 1 ]; then
    printf '  %s?%s %s%s : ' "$A" "$N" "$2" "${3:+ [$3]}" >/dev/tty
    if [ -n "${4:-}" ]; then stty -echo </dev/tty 2>/dev/null || true; fi
    IFS= read -r ans </dev/tty || ans=""
    if [ -n "${4:-}" ]; then stty echo </dev/tty 2>/dev/null || true; echo >/dev/tty; fi
  fi
  [ -n "$ans" ] || ans="${3:-}"
  eval "$1=\$ans"
}

confirm() { # confirm "question" → yes by default
  [ "$YES" -eq 1 ] && return 0
  [ "$TTY" -eq 1 ] || return 1
  printf '  %s?%s %s [O/n] : ' "$A" "$N" "$1" >/dev/tty
  IFS= read -r a </dev/tty || a=""
  case "$a" in n|N|non|Non|no|No) return 1 ;; *) return 0 ;; esac
}

# ---------- Privileges ----------
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 || die "lance ce script en root (ou installe sudo)"
  SUDO="sudo"
fi
run() { $SUDO "$@"; }

printf '\n  %sDevHub%s · installation\n\n' "$B" "$N"

[ "$(uname -s)" = "Linux" ] || die "DevHub s'installe sur un serveur Linux"

# ---------- Tools ----------
pkg_install() {
  if command -v apt-get >/dev/null 2>&1; then run apt-get update -qq && run env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >/dev/null
  elif command -v dnf >/dev/null 2>&1; then run dnf install -y -q "$@"
  elif command -v yum >/dev/null 2>&1; then run yum install -y -q "$@"
  elif command -v apk >/dev/null 2>&1; then run apk add -q "$@"
  else return 1; fi
}

# fetch URL: prints the body (curl, or BusyBox wget in the installer image).
fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsS4 --max-time 6 "$1"
  else wget -q -T 6 -O - "$1"; fi
}
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 || die "curl ou wget est nécessaire"
if ! command -v git >/dev/null 2>&1; then
  info "git absent : installation"
  pkg_install git || die "installe git puis relance le script"
fi

if [ "$IN_DOCKER" = "1" ]; then
  docker info >/dev/null 2>&1 || die "Docker injoignable : ajoute -v /var/run/docker.sock:/var/run/docker.sock à la commande docker run"
  grep -q " $DIR " /proc/self/mountinfo || die "monte le dossier d'installation au même chemin : -v $DIR:$DIR"
fi
if ! command -v docker >/dev/null 2>&1; then
  warn "Docker n'est pas installé"
  confirm "Installer Docker avec le script officiel (get.docker.com) ?" || die "installe Docker puis relance le script"
  curl -fsSL https://get.docker.com | run sh >/dev/null
fi
if ! run docker info >/dev/null 2>&1; then
  run systemctl start docker >/dev/null 2>&1 || run service docker start >/dev/null 2>&1 || true
  run docker info >/dev/null 2>&1 || die "Docker ne répond pas (docker info)"
fi
run docker compose version >/dev/null 2>&1 || die "Docker Compose v2 manquant (paquet docker-compose-plugin)"
ok "Docker $(run docker version --format '{{.Server.Version}}' 2>/dev/null || echo '?') et Compose trouvés"

# ---------- Existing installation ----------
UPDATE=0
if [ -f "$DIR/docker-compose.yml" ]; then
  UPDATE=1
  ok "DevHub déjà installé dans $DIR : mise à jour et redémarrage (le .env est gardé)"
  if [ -z "$DOMAIN" ] && [ -f "$DIR/.env" ]; then
    DOMAIN="$(run sed -n 's/^DEVHUB_DOMAIN=//p' "$DIR/.env" | tail -n 1 | tr -d '"'"'"' ')"
  fi
fi

# ---------- Domain ----------
is_ip() { printf '%s' "$1" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$'; }
PUBLIC_IP="${DEVHUB_PUBLIC_IP:-}"
[ -n "$PUBLIC_IP" ] || PUBLIC_IP="$(fetch https://api.ipify.org 2>/dev/null || fetch https://ifconfig.me 2>/dev/null || true)"
is_ip "$PUBLIC_IP" || PUBLIC_IP=""
need_ip() {
  [ -n "$PUBLIC_IP" ] && return 0
  [ "$TTY" -eq 1 ] || die "adresse IP publique introuvable : indique-la avec DEVHUB_PUBLIC_IP=…, ou un domaine avec --domain"
  ask PUBLIC_IP "Adresse IP publique du serveur (introuvable automatiquement)"
  is_ip "$PUBLIC_IP" || die "adresse IP invalide : $PUBLIC_IP"
}
# A new installation starts on the server's IP: the guide, in the browser, offers to move
# to a domain. An existing one keeps its mode.
if [ -z "$MODE" ] && { [ -z "$DOMAIN" ] || is_ip "$DOMAIN"; }; then MODE=ip; fi
case "$MODE" in
  ip) is_ip "$DOMAIN" || { need_ip; DOMAIN="$PUBLIC_IP"; } ;;
  *) MODE=domain ;;
esac
if [ "$MODE" = domain ]; then
  DOMAIN="$(printf '%s' "$DOMAIN" | tr '[:upper:]' '[:lower:]' | sed 's#^https\{0,1\}://##; s#/.*$##')"
  [ "$DOMAIN" != "dev.example.com" ] || die "dev.example.com est un exemple : mets ton propre domaine"
  printf '%s' "$DOMAIN" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$' || die "domaine invalide : $DOMAIN"
  ok "Domaine du panel : $B$DOMAIN$N"
elif [ "$UPDATE" -eq 0 ]; then
  ok "DevHub démarrera sur l'adresse du serveur, ${B}$DOMAIN$N : le nom de domaine se choisit ensuite dans le navigateur"
else
  ok "Sans nom de domaine : panel sur ${B}https://$DOMAIN$N"
fi

# ---------- DNS ----------
resolve() { getent ahostsv4 "$1" 2>/dev/null | awk 'NR == 1 { print $1 }' || true; }
IP_HINT="$PUBLIC_IP"
[ -n "$IP_HINT" ] || IP_HINT="l'IP du serveur"
check_dns() { # check_dns name label
  ip="$(resolve "$1")"
  if [ -z "$ip" ]; then
    warn "DNS  $2 ne se résout pas : ajoute un enregistrement A vers $IP_HINT"
    DNS_OK=0
  elif [ -n "$PUBLIC_IP" ] && [ "$ip" != "$PUBLIC_IP" ]; then
    warn "DNS  $2 → $ip, mais ce serveur est $PUBLIC_IP (proxy Cloudflare ? mets-le en « DNS only »)"
    DNS_OK=0
  else
    ok "DNS  $2 → $ip ${D}(ce serveur)$N"
  fi
}
DNS_OK=1
if [ "$MODE" = domain ]; then
  check_dns "$DOMAIN" "$DOMAIN"
  check_dns "devhub-install-check.$DOMAIN" "*.$DOMAIN"
fi
if [ "$DNS_OK" -eq 0 ]; then
  warn "sans ces enregistrements, le HTTPS ne marchera pas : corrige le DNS, Caddy réessaie tout seul"
fi

# ---------- Ports ----------
# Only checked: the server's firewall is not DevHub's to change.
listening() { command -v ss >/dev/null 2>&1 && run ss -ltnH 2>/dev/null | awk '{ print $4 }' | grep -Eq "[:.]$1\$"; }
PORTS="80 et 443"
[ "$MODE" != ip ] || PORTS="80, 443 et $IP_RANGE"
if [ "$IN_DOCKER" != "1" ] && [ "$UPDATE" -eq 0 ]; then
  BUSY=""
  for p in 80 443; do
    if listening "$p"; then BUSY="$BUSY $p"; fi
  done
  if [ -n "$BUSY" ]; then warn "port(s)$BUSY déjà utilisé(s) sur ce serveur (un autre serveur web ?) : Caddy ne pourra pas démarrer"
  else ok "Ports 80 et 443 libres"; fi
fi
info "Ouvre les ports $PORTS (TCP) dans le pare-feu du serveur ou de l'hébergeur, s'il y en a un"

# ---------- Code ----------
if [ -d "$DIR/.git" ]; then
  if run git -C "$DIR" pull -q --ff-only origin "$BRANCH"; then ok "Code de DevHub à jour ($BRANCH)"; else warn "git pull impossible : version actuelle gardée"; fi
else
  run mkdir -p "$(dirname "$DIR")"
  run git clone -q --branch "$BRANCH" "$REPO_URL" "$DIR" || die "impossible de cloner $REPO_URL"
  ok "DevHub récupéré dans $DIR"
fi

# ---------- Configuration ----------
# The .env lines that follow the domain / mode.
ip_sites() {
  lo="${IP_RANGE%-*}"; hi="${IP_RANGE#*-}"
  sites="https://$DOMAIN"; p="$lo"
  while [ "$p" -le "$hi" ]; do sites="$sites, https://$DOMAIN:$p"; p=$((p + 1)); done
  printf '%s' "$sites"
}
mode_lines() {
  echo "DEVHUB_DOMAIN=$DOMAIN"
  if [ "$MODE" = ip ]; then
    # No domain: docker-compose.ip.yml (Caddy on the server's network, one port per project).
    echo "COMPOSE_FILE=docker-compose.yml:docker-compose.ip.yml"
    echo "DEVHUB_PORT_RANGE=$IP_RANGE"
    echo "DEVHUB_IP_SITES=$(ip_sites)"
  fi
}
MODE_KEYS='^(DEVHUB_DOMAIN|COMPOSE_FILE|DEVHUB_PORT_RANGE|DEVHUB_IP_SITES)='
if [ -f "$DIR/.env" ] && [ "$CHOSEN" -eq 1 ]; then
  KEEP="$(run grep -vE "$MODE_KEYS" "$DIR/.env" || true)"
  { [ -z "$KEEP" ] || printf '%s\n' "$KEEP"; mode_lines; } | run tee "$DIR/.env" >/dev/null
  run chmod 600 "$DIR/.env"
  ok "Configuration mise à jour pour $DOMAIN (le reste du .env est gardé)"
elif [ -f "$DIR/.env" ]; then
  ok "Configuration existante gardée ($DIR/.env)"
else
  {
    echo "# Écrit par install.sh. Les autres réglages possibles sont dans .env.example."
    mode_lines
    if [ "$MODE" = domain ] && [ -n "$CF_TOKEN" ]; then
      echo "DEVHUB_TLS=wildcard"
      echo "CLOUDFLARE_API_TOKEN=$CF_TOKEN"
    fi
  } | run tee "$DIR/.env" >/dev/null
  run chmod 600 "$DIR/.env"
  if [ "$MODE" = ip ]; then ok "Configuration écrite, sans nom de domaine"
  elif [ -n "$CF_TOKEN" ]; then ok "Configuration écrite, certificat joker par Cloudflare"
  else ok "Configuration écrite, un certificat par projet"; fi
fi

# ---------- Images ----------
# Built by GitHub on every push: pulled here and tagged with the names compose expects.
# Without them (no access, registry down), compose builds them on this server.
cd "$DIR"
LOCAL_IMG="$(run docker compose config --images devhub 2>/dev/null | head -n 1 || true)"
[ -n "$LOCAL_IMG" ] || LOCAL_IMG="$(basename "$DIR" | tr '[:upper:]' '[:lower:]')-devhub"
pull_as() { run docker pull -q "$1" >/dev/null 2>&1 && run docker tag "$1" "$2"; }
info "Téléchargement des images depuis GitHub"
if pull_as "$IMAGE" "$LOCAL_IMG"; then
  ok "Image $IMAGE"
else
  warn "image $IMAGE indisponible : DevHub est compilé sur ce serveur (5 à 10 min)"
  run docker compose build -q devhub || die "la compilation de DevHub a échoué"
  ok "DevHub compilé"
fi
if pull_as "$CADDY_IMAGE" devhub-caddy; then
  ok "Image $CADDY_IMAGE"
else
  info "Caddy est compilé sur ce serveur (1 à 3 min)"
  run docker compose build -q caddy || die "la compilation de Caddy a échoué"
  ok "Caddy compilé"
fi

# ---------- Start ----------
run docker compose up -d >/dev/null 2>&1 || {
  run docker compose up -d
  die "le démarrage a échoué (voir ci-dessus)"
}
ok "DevHub et Caddy démarrés"

# ---------- Installation code ----------
CODE=""
READY=0
i=0
while [ "$i" -lt 60 ]; do
  LOGS="$(run docker logs devhub 2>&1 || true)"
  CODE="$(printf '%s\n' "$LOGS" | sed -n "s/.*CODE D'INSTALLATION : \([A-Z0-9-]*\).*/\1/p" | tail -n 1)"
  [ -n "$CODE" ] && break
  if printf '%s\n' "$LOGS" | grep -q "listening on"; then READY=1; fi
  [ "$READY" -eq 1 ] && [ "$i" -ge 3 ] && break
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
  if [ "$MODE" = ip ]; then
    printf '  %sLe guide propose d'"'"'abord de passer sur ton nom de domaine, si tu en as un.\n  Sur l'"'"'adresse IP, le navigateur peut afficher un avertissement de certificat la\n  première fois (certificat de secours de Caddy) : tu peux continuer.%s\n' "$D" "$N"
  fi
elif [ "$READY" -eq 1 ]; then
  printf '  DevHub est prêt : %s%s%s\n' "$A$B" "$URL" "$N"
  if [ "$MODE" = ip ]; then
    printf '  %sIl a déjà un propriétaire : connecte-toi avec ton mot de passe.%s\n' "$D" "$N"
  else
    printf '  %sIl a déjà un propriétaire : connecte-toi avec ton passkey.%s\n' "$D" "$N"
  fi
else
  printf '  DevHub démarre encore. Suis-le avec :\n'
  printf '  %sdocker logs -f devhub%s  (le code s'"'"'affiche à la ligne CODE)\n' "$B" "$N"
fi
echo "$LINE"
printf '  %sLe premier certificat HTTPS peut prendre une minute.%s\n\n' "$D" "$N"
