# DevHub

Dashboard (Rust) pour gérer tes projets : un conteneur Docker isolé par projet, des ports attribués automatiquement, une connexion uniquement par passkey, et un serveur MCP par projet pour que **Claude Desktop travaille directement dans le conteneur**.

```
Navigateur ──passkey──▶ Caddy (HTTPS) ──▶ devhub (Rust, axum) ──socket──▶ Docker
Claude Desktop ──https://domaine/mcp/<jeton>──┘                      ├─ devhub-projet-a
                                                                     └─ devhub-projet-b
```

## Fonctionnalités

- **Projets** : création à partir d'un modèle (Node, Rust, Python, Go, Debian) ou d'une image perso, avec un dépôt Git optionnel cloné dans `/workspace`.
- **Modèle « Page web »** : génère une page d'accueil au nom du projet (`templates/site.html`), servie tout de suite sur le port 8000 par défaut.
- **Démarrage automatique** : si `/workspace/start.sh` existe et est exécutable, il est lancé à chaque démarrage du conteneur (logs dans `/tmp/start.log`).
- **Isolation** : chaque projet a son réseau, une limite RAM/CPU/PIDs, `no-new-privileges` et des capabilities retirées. Il n'a **aucun** accès au socket Docker. Le code vit dans un volume (`/workspace`) qui survit aux rebuilds.
- **Sous-domaines** : `projet.dev.tweastly.com` pointe vers le premier port du projet, `projet-5173.dev.tweastly.com` vers un port précis. Les WebSockets passent (HMR de Vite/Next). Un projet est **privé par défaut** (passkey requis), et un bouton le rend public. Le cookie DevHub n'est jamais transmis aux projets.
- **Terminal web** : un shell interactif (xterm.js) dans le conteneur, avec redimensionnement, protégé contre le détournement de WebSocket cross-site.
- **Ports** : attribués automatiquement dans `DEVHUB_PORT_RANGE`, sans conflit. En production, ils ne sont publiés que sur `127.0.0.1` : on y accède par les sous-domaines.
- **Dashboard** : état, CPU et RAM de chaque projet, start/stop/restart/rebuild, logs, variables d'environnement, suppression, journal d'audit (y compris chaque action de Claude).
- **Mise à jour en un clic** : DevHub vérifie GitHub toutes les 30 min. Quand des commits sont plus récents que la version installée, un bandeau propose « Mettre à jour » : `git pull` puis `docker compose up -d --build`, lancés depuis un conteneur temporaire `devhub-updater`.
- **Passkey** (WebAuthn), sans mot de passe. Tu peux enregistrer plusieurs appareils.
- **MCP par projet** : outils `run_command`, `read_file`, `write_file`, `edit_file`, `list_dir`, `project_info`. Chaque URL ne donne accès qu'à un seul conteneur, et elle est régénérable.

## Déploiement sur le VPS

1. DNS : deux enregistrements `A` vers l'IP du VPS, `dev.tweastly.com` et `*.dev.tweastly.com`.
2. Sur le VPS (avec Docker installé) :
   ```bash
   git clone https://github.com/Tweastly/DevHub devhub && cd devhub
   echo "DEVHUB_DOMAIN=dev.tweastly.com" > .env
   docker compose up -d --build
   docker compose logs devhub | grep "SETUP TOKEN"
   ```
3. Ouvre `https://dev.tweastly.com/login`, colle le setup token et crée ton passkey. Le token ne sert qu'une fois.
4. Pare-feu : seuls 80 et 443 doivent être ouverts. Les certificats des sous-domaines sont générés automatiquement à la première visite.

## Mises à jour (dépôt privé)

Crée un jeton GitHub *fine-grained* : Settings → Developer settings → Fine-grained tokens. Limite-le au dépôt DevHub, avec la seule permission **Contents : Read-only**. Ajoute-le au `.env` :
```bash
echo "DEVHUB_GITHUB_TOKEN=github_pat_..." >> .env
docker compose up -d
```
Le jeton sert à vérifier les versions et à faire le `git pull`. Il n'est jamais écrit sur le disque.

## Connecter Claude Desktop à un projet

Sur la page du projet, copie l'URL MCP, puis dans Claude Desktop : **Réglages → Connecteurs → Ajouter un connecteur personnalisé**. Colle l'URL. Claude peut alors lire, écrire et exécuter du code, mais seulement dans ce conteneur.

> L'URL contient un jeton secret de 256 bits. Traite-la comme un mot de passe et régénère-la si elle fuit.

## Configuration

| Variable | Défaut | Rôle |
|---|---|---|
| `DEVHUB_DOMAIN` | `localhost` | domaine (RP ID WebAuthn) |
| `DEVHUB_ORIGIN` | `https://$DOMAIN` | origine exacte du dashboard |
| `DEVHUB_PORT_RANGE` | `20000-20999` | plage des ports hôte |
| `DEVHUB_MEMORY_MB` / `DEVHUB_CPUS` | `4096` / `2` | limites par projet |
| `DEVHUB_BIND_IP` | `0.0.0.0` (`127.0.0.1` en compose) | IP sur laquelle les ports projets sont publiés |
| `DEVHUB_SELF_CONTAINER` | — (`devhub` en compose) | nom du conteneur DevHub ; active le proxy par réseau Docker |
| `DEVHUB_DATA` | `./data` | base SQLite |
| `DEVHUB_GITHUB_TOKEN` | — | jeton lecture seule pour la mise à jour (dépôt privé) |
| `DEVHUB_REPO` | `/repo` | où le dépôt est monté dans le conteneur |

## Dev local

```bash
DEVHUB_ORIGIN=http://localhost:8080 cargo run
```
(les navigateurs acceptent les passkeys sur `localhost` sans HTTPS ; les sous-domaines `projet.localhost:8080` fonctionnent pour les projets publics)
