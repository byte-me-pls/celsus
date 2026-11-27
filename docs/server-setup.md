# Running the Celsus server

The server edition turns a home server or a Raspberry Pi (64-bit, 4 GB RAM or more) into a library several
people share: each has an account, books are private or shared, and translations run on the server.

In short: run one command, open the address it prints, enter the setup code, create your account, add a model.

- [Install](#install): with a domain name, or on the home network only
- [After installing](#after-installing): models, inviting people, two-step sign-in
- [Members](#members): limits, deleting an account, a forgotten password
- [Translations](#translations): translating pages again, removing a translation
- [Backups](#backups)
- [Settings](#settings)
- [Languages](#languages)

## Install

You need Docker with the Compose plugin. On a Raspberry Pi OS (64-bit) or Debian/Ubuntu machine:

```sh
curl -fsSL https://get.docker.com | sh          # if Docker is not installed yet
sudo usermod -aG docker "$USER"                  # then log out and in again
curl -fsSL https://raw.githubusercontent.com/byte-me-pls/celsus/main/install.sh | bash
```

The script asks for a domain name, writes `~/celsus/.env` with a random database password, starts everything and
prints a **one-time setup code**. Open the address it shows, enter the code and create the owner account. Nobody
can claim the server without that code; it disappears once used. If you lose it, restart the server and look in
the log: `docker compose -f ~/celsus/compose.yml logs celsus | grep "setup code"`.

Run the script again at any time to update; your settings and data are kept.

### With a domain name (recommended)

Point the domain's DNS at your home address, forward ports 80 and 443 on your router to the server, and give the
domain to the installer. Caddy fetches and renews an HTTPS certificate on its own, and it is the only part reachable
from outside; the database is on a network with no way in or out.

No open ports? A Cloudflare Tunnel or Tailscale Funnel in front of `http://localhost:8080` also works. Set
`CELSUS_PUBLIC_URL` to the public https address, leave `COMPOSE_PROFILES` empty, and add the tunnel's address to
`CELSUS_TRUSTED_PROXIES` only if it connects from a fixed address.

### Home network only

Leave the domain empty. The library is then reachable at `http://<server-address>:8080` from your network.
Browsers keep secure cookies only over HTTPS, so this mode (`CELSUS_INSECURE_HTTP=1`) sends the session cookie
without that flag: anyone on the same network who can watch traffic could take over a session. Use it only on a
network you trust, and never forward this port to the internet. The server refuses to start in this mode if
`CELSUS_PUBLIC_URL` is https.

If the server runs a firewall such as ufw that also filters Docker's published ports, other devices cannot reach
the library until you allow the port from your network only, for example
`sudo ufw allow from 192.168.0.0/16 to any port 8080 proto tcp`.

## After installing

1. **Connections and models** (Admin → Connections and models): add an OpenAI, NVIDIA, OpenRouter or other
   OpenAI-compatible connection with its key, then choose the translation model. Keys are encrypted with the
   server key and never shown again.
2. A model server on the same machine (Ollama, LM Studio) is reached as `http://host.docker.internal:11434/v1`;
   tick **Local network** for it. Without that tick the server refuses every private or local address, so a mistyped
   or malicious address cannot reach your router or other devices.
3. **Invite people** (Admin → Members): each link works once and expires after 7 days.
4. Turn on **two-step sign-in** for your own account (Account → Two-step sign-in).

## Members

Admin → Members lists everyone with their books. Admins manage members; only the owner manages admins.

- **Disable** an account to stop it signing in at once (its sessions, keys and connected apps stop working). Its
  books stay. A disabled member's private books can be removed with **Remove private books**: nobody else can see
  them, so without this they would stay on the server.
- **Delete** an account (after a confirmation) to remove it for good. Its private books are deleted with it. Its
  shared books pass to the admin who deletes the account, so the members reading them keep them. Its sessions, API
  keys, connected apps and the jobs it started go too. The audit log keeps what it did.
- **Limits** (Admin → Translation → Limits for members): the most books one member may have, and the most pages
  one member may have waiting to be translated at once. 0 is no limit. Uploads (also over MCP) and new jobs past
  a limit are refused with a message saying why. Admins and the owner are not held to them.

### A forgotten password

The server has no email, so a password is reset on the server itself. Anyone who can run commands there can do
it; nobody else can:

```sh
docker compose -f ~/celsus/compose.yml exec celsus celsus-server reset-password someone@example.com
```

It prints a new random password once and ends every session of the account; sign in with it and choose a new one
under Account. For a lost phone, add `--reset-2fa` to turn off two-step sign-in for the account too (its backup
codes go with it); turn it on again after signing in. A disabled account stays disabled. The reset is in the
audit log, without the password.

## Translations

On a book's page, the translate card queues pages for the server's model. Pages translated already are written
from the kept translations and cost nothing; tick **Translate pages that are translated already again** to have
them translated anew. Each stays in the translated PDF as it was until its new translation is finished. The reader
offers the same.

Under **Translations**, **Remove** deletes one language's translation of a book (after a confirmation): the
translated PDF, its pages, the pictures redrawn for that language, its jobs, and the translations kept for it.
A job translating the book into that language is stopped first. Only those who may edit the book (its owner, or an
admin for a shared book) can do this.

## Backups

Everything lives in two Docker volumes: `celsus_db` (accounts, books, jobs, cached translations) and `celsus_data`
(the PDFs and `server.key`). Back up both; without `server.key` the stored provider keys and two-step secrets cannot
be decrypted, and without the database the key alone is useless.

```sh
cd ~/celsus
docker compose exec -T db pg_dump -U celsus -Fc celsus > celsus-db-$(date +%F).dump
docker run --rm -v celsus_data:/data -v "$PWD":/backup debian:trixie-slim tar czf /backup/celsus-files-$(date +%F).tar.gz -C /data .
```

Keep the backups somewhere safe: they contain everyone's books and the server key.

Restore onto a fresh install (before creating any account):

```sh
docker compose up -d db
docker compose exec -T db pg_restore -U celsus -d celsus --clean --if-exists < celsus-db-YYYY-MM-DD.dump
docker run --rm -v celsus_data:/data -v "$PWD":/backup debian:trixie-slim sh -c "tar xzf /backup/celsus-files-YYYY-MM-DD.tar.gz -C /data && chown -R 10001:10001 /data"
docker compose up -d
```

## Settings

All in `~/celsus/.env`:

| Setting | Meaning |
|---|---|
| `POSTGRES_PASSWORD` | Database password (made by the installer) |
| `COMPOSE_PROFILES=https`, `CELSUS_DOMAIN` | Run Caddy for automatic HTTPS on that domain |
| `CELSUS_PUBLIC_URL` | The address people use; requests from other origins are refused |
| `CELSUS_BIND`, `CELSUS_PORT` | Where port 8080 is published (`127.0.0.1` behind Caddy, `0.0.0.0` for home network mode) |
| `CELSUS_INSECURE_HTTP` | `1` only for home network mode |
| `CELSUS_MAX_UPLOAD_MB` | Largest PDF accepted (default 300) |
| `CELSUS_VERSION` | Image tag to run (default `latest`) |
| `CELSUS_HTTP_PORT`, `CELSUS_HTTPS_PORT` | Ports Caddy publishes (default 80 and 443) |
| `CELSUS_SUBNET`, `CELSUS_PROXY_IP` | The proxy network (default `172.30.70.0/24`, Caddy at `.10`); change both if that range is taken |

Building the image yourself (`docker build -t ghcr.io/byte-me-pls/celsus .`) needs no BuildKit. On a Raspberry Pi
or a busy server, `--build-arg BUILD_JOBS=2` keeps memory and CPU use down.

After changing `.env`: `docker compose up -d`.

## Languages

Books can be translated into about 130 languages; `GET /api/languages` lists them with their names and writing
direction. The image carries the fonts to write them: Noto for Latin, Cyrillic, Greek, Arabic, Hebrew, the Indian
scripts, Thai, Lao, Khmer, Myanmar, Ethiopic, Armenian, Georgian and more, and WenQuanYi Zen Hei for Chinese, Japanese
and Korean. Arabic letters are joined and right-to-left lines run right to left; Indian scripts get their conjuncts.

A block whose translation contains characters no installed font has keeps its original text, and the job reports
which characters were missing. To add fonts, mount a folder of TrueType files (`.ttf`, `.ttc`) and point
`CELSUS_FONTS_DIR` at it; subfolders are searched too.

## What the container does to stay small and contained

The server runs as an unprivileged user with a read-only file system, no Linux capabilities and no way to gain
privileges; only `/data` is writable. The database is not published on any port. See
[server-security.md](server-security.md) for how accounts, sessions and the library are protected.
