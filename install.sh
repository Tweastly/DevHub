#!/bin/sh
# DevHub installer. On a fresh Linux server, as root (or a user with sudo):
#
#   curl -fsSL https://raw.githubusercontent.com/Tweastly/DevHub/main/install.sh | sh
#
# Checks Docker (installs it if needed), asks for the panel's domain, checks the DNS and
# the ports, fetches DevHub into /opt/devhub, pulls its images from GitHub (or builds them),
# starts it, then prints the address of the installation guide and its code.
# Run it again to update an existing installation: the .env is kept.
#
# Options (or the matching environment variables, for unattended installs):
#   --domain dev.example.com     DEVHUB_DOMAIN       panel domain (projects: *.dev.example.com)
#   --dir /opt/devhub            DEVHUB_DIR          installation folder
#   --branch main                DEVHUB_BRANCH       branch to follow
#   --cloudflare-token TOKEN     CLOUDFLARE_API_TOKEN wildcard certificate through Cloudflare's DNS
#   --yes                                            answer yes to every question
set -eu

REPO_URL="${DEVHUB_REPO_URL:-https://github.com/Tweastly/DevHub}"
IMAGE="${DEVHUB_IMAGE:-ghcr.io/tweastly/devhub:latest}"
CADDY_IMAGE="${DEVHUB_CADDY_IMAGE:-ghcr.io/tweastly/devhub:caddy}"
DIR="${DEVHUB_DIR:-/opt/devhub}"
BRANCH="${DEVHUB_BRANCH:-main}"
DOMAIN="${DEVHUB_DOMAIN:-}"
CF_TOKEN="${CLOUDFLARE_API_TOKEN:-}"
YES=0

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
  sed -n '2,19p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || true
  echo "Voir https://github.com/Tweastly/DevHub"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="${2:-}"; shift 2 ;;
    --domain=*) DOMAIN="${1#*=}"; shift ;;
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

# Questions go through the terminal: with `curl … | sh`, the script itself is on stdin.
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

command -v curl >/dev/null 2>&1 || die "curl est nécessaire"
if ! command -v git >/dev/null 2>&1; then
  info "git absent : installation"
  pkg_install git || die "installe git puis relance le script"
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
while :; do
  if [ -z "$DOMAIN" ]; then
    [ "$TTY" -eq 1 ] || die "indique le domaine : … | sh -s -- --domain dev.example.com"
    ask DOMAIN "Domaine du panel, ex. dev.mondomaine.fr (les projets seront sur *.ce-domaine)"
  fi
  DOMAIN="$(printf '%s' "$DOMAIN" | tr '[:upper:]' '[:lower:]' | sed 's#^https\{0,1\}://##; s#/.*$##')"
  if [ -z "$DOMAIN" ]; then
    :
  elif [ "$DOMAIN" = "dev.example.com" ]; then
    warn "dev.example.com est un exemple : mets ton propre domaine"
  elif printf '%s' "$DOMAIN" | grep -Eq '^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$'; then
    break
  else
    warn "domaine invalide : $DOMAIN"
  fi
  [ "$TTY" -eq 1 ] || exit 1
  DOMAIN=""
done
ok "Domaine du panel : $B$DOMAIN$N"

# ---------- DNS ----------
resolve() { getent ahostsv4 "$1" 2>/dev/null | awk 'NR == 1 { print $1 }'; }
PUBLIC_IP="$(curl -fsS4 --max-time 6 https://api.ipify.org 2>/dev/null || curl -fsS4 --max-time 6 https://ifconfig.me 2>/dev/null || true)"
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
check_dns "$DOMAIN" "$DOMAIN"
check_dns "devhub-install-check.$DOMAIN" "*.$DOMAIN"
if [ "$DNS_OK" -eq 0 ]; then
  warn "sans ces enregistrements, le HTTPS ne marchera pas : tu peux continuer et corriger le DNS ensuite"
  confirm "Continuer quand même ?" || exit 1
fi

# ---------- Ports and firewall ----------
listening() { command -v ss >/dev/null 2>&1 && run ss -ltnH 2>/dev/null | awk '{ print $4 }' | grep -Eq "[:.]$1\$"; }
if [ "$UPDATE" -eq 0 ]; then
  for p in 80 443; do
    if listening "$p"; then
      warn "le port $p est déjà utilisé sur ce serveur (un autre serveur web ?)"
      confirm "Continuer quand même ?" || exit 1
    fi
  done
  ok "Ports 80 et 443 libres"
fi
if command -v ufw >/dev/null 2>&1 && run ufw status 2>/dev/null | grep -q "Status: active"; then
  if confirm "Le pare-feu ufw est actif : ouvrir les ports 80 et 443 ?"; then
    run ufw allow 80/tcp >/dev/null && run ufw allow 443/tcp >/dev/null && ok "Ports 80 et 443 ouverts dans ufw"
  fi
fi

# ---------- Code ----------
if [ -d "$DIR/.git" ]; then
  if run git -C "$DIR" pull -q --ff-only origin "$BRANCH"; then ok "Code de DevHub à jour ($BRANCH)"; else warn "git pull impossible : version actuelle gardée"; fi
else
  run mkdir -p "$(dirname "$DIR")"
  run git clone -q --branch "$BRANCH" "$REPO_URL" "$DIR" || die "impossible de cloner $REPO_URL"
  ok "DevHub récupéré dans $DIR"
fi

# ---------- Configuration ----------
if [ -f "$DIR/.env" ]; then
  ok "Configuration existante gardée ($DIR/.env)"
else
  if [ -z "$CF_TOKEN" ] && [ "$TTY" -eq 1 ]; then
    printf '  %sCertificat joker *.%s : un jeton Cloudflare « Zone › DNS › Modifier », sinon un certificat par projet.%s\n' "$D" "$DOMAIN" "$N"
    ask CF_TOKEN "Jeton Cloudflare (Entrée pour passer)" "" secret
  fi
  {
    echo "# Écrit par install.sh. Les autres réglages possibles sont dans .env.example."
    echo "DEVHUB_DOMAIN=$DOMAIN"
    if [ -n "$CF_TOKEN" ]; then
      echo "DEVHUB_TLS=wildcard"
      echo "CLOUDFLARE_API_TOKEN=$CF_TOKEN"
    fi
  } | run tee "$DIR/.env" >/dev/null
  run chmod 600 "$DIR/.env"
  if [ -n "$CF_TOKEN" ]; then ok "Configuration écrite, certificat joker par Cloudflare"; else ok "Configuration écrite, un certificat par projet"; fi
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
  printf '  Ouvre   %s%s/setup%s\n' "$A$B" "$URL" "$N"
  printf '  Code    %s%s%s  %s(une seule fois)%s\n' "$A$B" "$CODE" "$N" "$D" "$N"
elif [ "$READY" -eq 1 ]; then
  printf '  DevHub est prêt : %s%s%s\n' "$A$B" "$URL" "$N"
  printf '  %sIl a déjà un propriétaire : connecte-toi avec ton passkey.%s\n' "$D" "$N"
else
  printf '  DevHub démarre encore. Suis-le avec :\n'
  printf '  %sdocker logs -f devhub%s  (le code s'"'"'affiche à la ligne CODE)\n' "$B" "$N"
fi
echo "$LINE"
printf '  %sLe premier certificat HTTPS peut prendre une minute.%s\n\n' "$D" "$N"
