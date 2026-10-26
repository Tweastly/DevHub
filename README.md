# DevHub

Dashboard (Rust) pour gérer tes projets : un conteneur Docker isolé par projet, des ports attribués automatiquement, une connexion uniquement par passkey, et un serveur MCP par projet pour que **Claude Desktop travaille directement dans le conteneur**.

```
Navigateur ──passkey──▶ Caddy (HTTPS) ──▶ devhub (Rust, axum) ──socket──▶ Docker
Claude Desktop ──https://domaine/mcp/<jeton>──┘                      ├─ devhub-projet-a
                                                                     └─ devhub-projet-b
```

## Fonctionnalités

- **Interface** : barre latérale avec projets épinglés et jauges du serveur, onglets projet (Aperçu, Réseau, Fichiers, Terminal & logs, Paramètres), cartes ou liste, recherche (`/`), thème clair/sombre (système ou forcé), navigation mobile en bas. Le design system est dans `assets/app.css`.
- **Historique** : CPU et RAM du serveur et de chaque projet échantillonnés chaque minute, gardés 31 jours (sparklines 24 h / 7 j / 30 j).
- **Journal** : filtres par acteur, projet et période, regroupement par jour, pagination, export CSV. Les connexions notent le passkey utilisé et l'IP masquée.

- **Projets** : création à partir d'un modèle (Node, Rust, Python, Go, Debian) ou d'une image perso, avec un dépôt Git optionnel cloné dans `/workspace`.
- **Modèle « Page web »** : génère une page d'accueil au nom du projet (`templates/site.html`), servie tout de suite sur le port 8000 par défaut.
- **Démarrage automatique** : si `/workspace/start.sh` existe et est exécutable, il est lancé à chaque démarrage du conteneur (logs dans `/tmp/start.log`).
- **Isolation** : chaque projet a son réseau, une limite RAM/CPU/PIDs, `no-new-privileges` et des capabilities retirées. Il n'a **aucun** accès au socket Docker. Le code vit dans un volume (`/workspace`) qui survit aux rebuilds.
- **Sous-domaines** : `projet.dev.tweastly.com` pointe vers le premier port du projet, `projet-5173.dev.tweastly.com` vers un port précis. Les WebSockets passent (HMR de Vite/Next). Un projet est **privé par défaut** (passkey requis), et un bouton le rend public. Le cookie DevHub n'est jamais transmis aux projets.
- **Sous-domaines personnalisés** : sers un projet sur `<nom>.tweastly.com` (tape juste `new`) depuis sa page ou via Claude. Le domaine racine se déduit de `DEVHUB_DOMAIN` (`dev.tweastly.com` → `tweastly.com`), ou se force avec `DEVHUB_APPS_DOMAIN`. Le HTTPS est automatique. Les projets privés restent protégés par ton passkey, grâce à un passage de session à usage unique (le cookie du dashboard n'est jamais partagé).
- **Serveurs Minecraft** : modèle « Minecraft » (image `itzg/minecraft-server`, Paper par défaut, EULA acceptée). Le port `25565` est publié en **TCP public** sur l'IP du VPS (un 2ᵉ serveur prend `25566`). La mémoire Java suit la limite RAM du projet (75 %). La page du projet donne l'adresse à partager, les joueurs en ligne et une console (commandes RCON). Le monde est dans `/data`, visible dans l'onglet Fichiers. Réglages via les variables (`TYPE`, `VERSION`, `MOTD`, `DIFFICULTY`, `WHITELIST`…).
- **Ports TCP publics** : dans le champ des ports, `25565/tcp` publie un port brut sur Internet (jeux) au lieu d'un site web derrière un sous-domaine HTTPS. Pense à l'ouvrir dans le pare-feu (`ufw allow 25565/tcp`).
- **Fichiers** : explorateur de `/workspace` avec éditeur de texte (Ctrl+S), envoi de fichiers (200 Mo max par envoi), création de fichiers et dossiers, renommage, suppression et téléchargement.
- **Terminal web** : un shell interactif (xterm.js) dans le conteneur, avec redimensionnement, protégé contre le détournement de WebSocket cross-site.
- **Ports** : attribués automatiquement dans `DEVHUB_PORT_RANGE`, sans conflit. En production, ils ne sont publiés que sur `127.0.0.1` : on y accède par les sous-domaines.
- **Ressources par projet** : RAM max (Mo) et CPU max (cœurs) réglables depuis la page du projet ou par Claude. C'est appliqué à chaud, sans redémarrage, et conservé aux rebuilds. Le panel affiche la capacité du serveur et le total alloué.
- **Dashboard** : état, CPU et RAM de chaque projet **en direct** (flux SSE, toutes les ~2 s), logs en direct, navigation fluide sans rechargement (View Transitions + skeleton pendant le chargement), start/stop/restart/rebuild, logs, variables d'environnement, suppression, journal d'audit (y compris chaque action de Claude).
- **Mise à jour en un clic** : DevHub vérifie GitHub toutes les 30 min. Quand des commits sont plus récents que la version installée, un bandeau propose « Mettre à jour » : `git pull` puis `docker compose up -d --build`, lancés depuis un conteneur temporaire `devhub-updater`.
- **Passkey** (WebAuthn), sans mot de passe. Tu peux enregistrer plusieurs appareils.
- **Profils** (Paramètres → Profils) : plusieurs personnes, chacune avec ses passkeys et ses autorisations. Rôles : *Administrateur* (tout DevHub), *Développeur* (ses projets, terminal et fichiers compris), *Opérateur* (démarrer, arrêter, régler, sans terminal ni fichiers), *Lecture seule*. Chaque profil voit tous les projets ou seulement ceux cochés. À la création, DevHub donne un lien d'invitation (48 h, usage unique) : la personne l'ouvre sur son appareil et y crée son passkey. Les projets privés ne s'ouvrent qu'aux profils qui y ont accès, et l'URL MCP d'un projet n'est montrée qu'aux profils qui ont le terminal. Le profil propriétaire garde toujours tous les droits.
- **Paramètres** : Claude, passkeys, profils, journal et mise à jour sont regroupés sous ta ligne de profil, en bas du menu (onglet *Paramètres* sur mobile).
- **MCP admin (optionnel)** : une URL qui donne à Claude tout le panel. Il peut créer, configurer, redémarrer et supprimer des projets, travailler dans n'importe quel conteneur, lire les logs et lancer la mise à jour. Elle s'active, se régénère et se coupe depuis la page **Claude**. Les passkeys restent hors de sa portée.
- **MCP par projet** : outils `run_command`, `read_file`, `write_file`, `edit_file`, `list_dir`, `project_info`. Chaque URL ne donne accès qu'à un seul conteneur, et elle est régénérable.

## Déploiement sur le VPS

1. DNS : deux enregistrements `A` vers l'IP du VPS, `dev.tweastly.com` et `*.dev.tweastly.com`. Pour les domaines personnalisés, ajoute aussi `*.tweastly.com`, ou un enregistrement par domaine.
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

**Mise à jour rapide (moins d'une minute).** À chaque push sur `main`, GitHub Actions compile DevHub (`.github/workflows/binary.yml`, environ 5 à 8 min, sans action tierce : il marche quelle que soit la politique « actions autorisées » du dépôt) et publie le binaire sur la release `edge` du dépôt. Le bouton **Mettre à jour** télécharge ce binaire avec le même jeton, et l'image Docker ne fait que le copier : le panel redémarre en 20 à 30 secondes environ. La page Mise à jour indique si le binaire est prêt. S'il ne l'est pas encore (CI en cours, serveur ARM), DevHub compile sur le serveur en réutilisant le cache de compilation, donc seul le code modifié est recompilé. La toute première mise à jour vers cette version compile encore entièrement.

## Connecter Claude Desktop à un projet

Sur la page du projet, copie l'URL MCP, puis dans Claude Desktop : **Réglages → Connecteurs → Ajouter un connecteur personnalisé**. Colle l'URL. Claude peut alors lire, écrire et exécuter du code, mais seulement dans ce conteneur.

> L'URL contient un jeton secret de 256 bits. Traite-la comme un mot de passe et régénère-la si elle fuit.

## Configuration

| Variable | Défaut | Rôle |
|---|---|---|
| `DEVHUB_DOMAIN` | `localhost` | domaine (RP ID WebAuthn) |
| `DEVHUB_ORIGIN` | `https://$DOMAIN` | origine exacte du dashboard |
| `DEVHUB_PORT_RANGE` | `20000-20999` | plage des ports hôte |
| `DEVHUB_MEMORY_MB` / `DEVHUB_CPUS` | `4096` / `2` | limites par défaut des projets (modifiables projet par projet) |
| `DEVHUB_BIND_IP` | `0.0.0.0` (`127.0.0.1` en compose) | IP sur laquelle les ports projets sont publiés |
| `DEVHUB_SELF_CONTAINER` | — (`devhub` en compose) | nom du conteneur DevHub ; active le proxy par réseau Docker |
| `DEVHUB_DATA` | `./data` | base SQLite |
| `DEVHUB_APPS_DOMAIN` | parent de `DEVHUB_DOMAIN` | domaine des sous-domaines personnalisés (`<nom>.tweastly.com`) |
| `DEVHUB_GITHUB_TOKEN` | — | jeton lecture seule pour la mise à jour (dépôt privé) |
| `DEVHUB_REPO` | `/repo` | où le dépôt est monté dans le conteneur |
| `DEVHUB_PUBLIC_HOST` | `DEVHUB_DOMAIN` | nom affiché pour rejoindre les ports TCP (ex. `mc.tweastly.com`) |

## Dev local

```bash
DEVHUB_ORIGIN=http://localhost:8080 cargo run
```
(les navigateurs acceptent les passkeys sur `localhost` sans HTTPS ; les sous-domaines `projet.localhost:8080` fonctionnent pour les projets publics)
