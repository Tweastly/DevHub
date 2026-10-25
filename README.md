# DevHub

Dashboard (Rust) pour gérer tes projets : un conteneur Docker isolé par projet, des ports attribués automatiquement, une connexion uniquement par passkey, et un serveur MCP par projet pour que **Claude Desktop travaille directement dans le conteneur**.

```
Navigateur ──passkey──▶ Caddy (HTTPS) ──▶ devhub (Rust, axum) ──socket──▶ Docker
Claude Desktop ──https://domaine/mcp/<jeton>──┘                      ├─ devhub-projet-a
                                                                     └─ devhub-projet-b
```

## Fonctionnalités

- **Projets** : création à partir d'un modèle (Node, Rust, Python, Go, Debian) ou d'une image perso, avec un dépôt Git optionnel cloné dans `/workspace`.
- **Isolation** : chaque projet a son réseau, une limite RAM/CPU/PIDs, `no-new-privileges` et des capabilities retirées. Il n'a **aucun** accès au socket Docker. Le code vit dans un volume (`/workspace`) qui survit aux rebuilds.
- **Ports** : attribués automatiquement dans `DEVHUB_PORT_RANGE`, sans conflit. Modifiables depuis le dashboard.
- **Dashboard** : état, CPU et RAM de chaque projet, start/stop/restart/rebuild, logs, variables d'environnement, suppression, journal d'audit (y compris chaque action de Claude).
- **Passkey** (WebAuthn), sans mot de passe. Tu peux enregistrer plusieurs appareils.
- **MCP par projet** : outils `run_command`, `read_file`, `write_file`, `edit_file`, `list_dir`, `project_info`. Chaque URL ne donne accès qu'à un seul conteneur, et elle est régénérable.

## Déploiement sur le VPS

1. DNS : un enregistrement `A` de `dev.tondomaine.com` vers l'IP du VPS.
2. Sur le VPS (avec Docker installé) :
   ```bash
   git clone <ce dépôt> devhub && cd devhub
   echo "DEVHUB_DOMAIN=dev.tondomaine.com" > .env
   docker compose up -d --build
   docker compose logs devhub | grep "SETUP TOKEN"
   ```
3. Ouvre `https://dev.tondomaine.com/login`, colle le setup token et crée ton passkey. Le token ne sert qu'une fois.
4. Pare-feu : ouvre 80, 443 et la plage de ports des projets (par défaut 20000-20999) si tu veux y accéder depuis l'extérieur. Pour garder les ports privés, mets `DEVHUB_BIND_IP=127.0.0.1`.

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
| `DEVHUB_BIND_IP` | `0.0.0.0` | IP sur laquelle les ports projets sont publiés |
| `DEVHUB_DATA` | `./data` | base SQLite |

## Dev local

```bash
DEVHUB_ORIGIN=http://localhost:8080 cargo run
```
(les navigateurs acceptent les passkeys sur `localhost` sans HTTPS)
