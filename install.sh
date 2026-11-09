#!/usr/bin/env bash
# Install or update the Celsus server with Docker.
#
#   curl -fsSL https://raw.githubusercontent.com/byte-me-pls/celsus/main/install.sh | bash
#
# Run it again later to update. Settings live in ~/celsus/.env (or $CELSUS_DIR/.env) and are kept.
set -euo pipefail

REPO="byte-me-pls/celsus"
REF="${CELSUS_REF:-main}"
DIR="${CELSUS_DIR:-$HOME/celsus}"
RAW="https://raw.githubusercontent.com/$REPO/$REF"
# Where this script lives, worked out before anything changes directory. Empty when it arrives through a pipe.
here=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

say() { printf '\033[1;33m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;31m!!\033[0m %s\n' "$*" >&2; }
die() { warn "$*"; exit 1; }

# Questions come from the terminal even when this script arrives through a pipe.
ask() {
  local prompt="$1" default="${2:-}" answer=""
  if [ -r /dev/tty ]; then
    read -r -p "$prompt" answer < /dev/tty || true
  fi
  printf '%s' "${answer:-$default}"
}

command -v docker >/dev/null 2>&1 || die "Docker is not installed. See https://docs.docker.com/engine/install/ (on a Raspberry Pi: curl -fsSL https://get.docker.com | sh)."
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is missing (the 'docker compose' command)."
docker info >/dev/null 2>&1 || die "Docker is not running, or this user may not use it (try: sudo usermod -aG docker \"\$USER\", then log in again)."

mkdir -p "$DIR/deploy"
cd "$DIR"

# The deployment files: from this checkout if the script runs inside one, otherwise from GitHub.
fetch() {
  local path="$1"
  if [ -n "$here" ] && [ -f "$here/$path" ] && [ "$here" != "$DIR" ]; then
    cp "$here/$path" "$DIR/$path"
  else
    curl -fsSL "$RAW/$path" -o "$DIR/$path.tmp" || die "Could not download $path from $RAW (is the repository public?)."
    mv "$DIR/$path.tmp" "$DIR/$path"
  fi
}
say "Getting the deployment files into $DIR"
fetch compose.yml
fetch deploy/Caddyfile
fetch deploy/celsus.env.example

if [ ! -f .env ]; then
  say "First install: a few questions"
  # CELSUS_DOMAIN answers the question in advance (set it empty for home network only).
  if [ -n "${CELSUS_DOMAIN+set}" ]; then
    domain="$CELSUS_DOMAIN"
  else
    domain="$(ask 'Domain name for the library (leave empty for home network only): ')"
  fi
  domain="$(printf '%s' "$domain" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]' | sed -e 's#^https\?://##' -e 's#/.*##')"
  password="$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 40 || true)"
  [ "${#password}" -eq 40 ] || die "Could not make a random password."
  umask 077
  {
    echo "# Written by install.sh on $(date -u +%Y-%m-%d). Keep this file private."
    echo "POSTGRES_PASSWORD=$password"
    if [ -n "$domain" ]; then
      echo "COMPOSE_PROFILES=https"
      echo "CELSUS_DOMAIN=$domain"
      echo "CELSUS_PUBLIC_URL=https://$domain"
      echo "CELSUS_BIND=127.0.0.1"
    else
      echo "CELSUS_BIND=0.0.0.0"
      echo "CELSUS_INSECURE_HTTP=1"
    fi
  } > .env
  chmod 600 .env
  if [ -z "$domain" ]; then
    warn "Home network mode: sign-ins travel without encryption on your network."
    warn "Do not forward this port to the internet. For access from outside, run this again with a domain."
    if systemctl is-active --quiet ufw 2>/dev/null; then
      warn "A firewall (ufw) is on. If other devices cannot reach the library, allow the port from your network only:"
      warn "  sudo ufw allow from 192.168.0.0/16 to any port ${CELSUS_PORT:-8080} proto tcp"
    fi
  else
    say "Point $domain at this machine and forward ports 80 and 443 to it; Caddy gets the certificate."
  fi
else
  say "Keeping the existing settings in $DIR/.env"
fi

say "Downloading the images"
if ! docker compose pull; then
  warn "Could not download the images; starting with the ones already on this machine, if any."
fi
say "Starting Celsus"
docker compose up -d --remove-orphans

say "Waiting for the server"
for _ in $(seq 1 60); do
  status="$(docker inspect --format '{{.State.Health.Status}}' "$(docker compose ps -q celsus)" 2>/dev/null || true)"
  [ "$status" = "healthy" ] && break
  sleep 2
done
[ "${status:-}" = "healthy" ] || { docker compose logs --tail 40 celsus; die "Celsus did not become healthy. The log is above."; }

. ./.env
if [ -n "${CELSUS_DOMAIN:-}" ]; then
  url="https://$CELSUS_DOMAIN"
else
  # The address the machine uses on its network (not a Docker bridge): the source of the default route.
  ip="$( (ip -4 route get 1.1.1.1 2>/dev/null || true) | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}')"
  [ -n "$ip" ] || ip="$( (hostname -I 2>/dev/null || true) | awk '{print $1}')"
  url="http://${ip:-localhost}:${CELSUS_PORT:-8080}"
fi
code="$(docker compose logs celsus 2>/dev/null | grep -o 'setup code: [A-Z0-9-]*' | tail -1 | cut -d' ' -f3 || true)"

say "Celsus is running: $url"
if [ -n "$code" ]; then
  say "Open it and create the owner account with this one-time setup code: $code"
fi
echo "Update later by running this script again. Backups: see docs/server-setup.md."
