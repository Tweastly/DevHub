# DevHub

Panel auto-hébergé (Rust) pour faire tourner tes projets sur un VPS : un conteneur Docker isolé par projet, un sous-domaine HTTPS automatique, une connexion uniquement par passkey, et un accès MCP pour que **Claude travaille directement dans les conteneurs**.

```
Navigateur ──passkey──▶ Caddy (HTTPS) ──▶ devhub (Rust, axum) ──socket──▶ Docker
Claude ──https://dev.example.com/mcp (OAuth)──┘                    ├─ devhub-projet-a (+ volume)
                                                                   └─ devhub-projet-b (+ volume)
```

Dans ce README, `dev.example.com` est le domaine du panel et `example.com` son domaine parent : remplace-les par les tiens.

## Fonctionnalités

### Projets
- **Création** à partir d'un modèle (Page web, Node, Rust, Python, Go, Debian, Minecraft Java, Minecraft Bedrock) ou de n'importe quelle image Docker, avec un dépôt Git optionnel cloné dans `/workspace`.
- **Deux modes de lancement** pour une image perso : *Service* (la commande de l'image tourne, la console écrit sur son entrée standard) ou *Environnement de dev* (le conteneur reste allumé et lance `/workspace/start.sh` s'il existe, logs dans `/tmp/start.log`).
- **Confirmations** : un dialogue DevHub (neutre, attention, danger) remplace ceux du navigateur ; les suppressions de données irréversibles demandent de taper le nom.
- **Actions sans rechargement** : chaque bouton s'exécute en arrière-plan, la page se met à jour sur place et le résultat s'affiche en notification.
- **Liste des projets** en cartes ou en liste : état, CPU et RAM en direct, badges (accès, ports, domaines, durée), filtres et tri. **Sélection multiple** pour démarrer, arrêter, redémarrer, recréer ou supprimer plusieurs projets d'un coup.
- **Page projet** : même en-tête sur tous les onglets (état, accès, « Ouvrir le site », Redémarrer / Recréer / Arrêter) et six onglets :
  - **Aperçu** : CPU (historique 24 h), mémoire, adresses, console de l'app, activité récente.
  - **Réseau** : l'adresse d'abord (URL du site avec Privé / Public, ou adresse + port à taper dans le jeu), le tableau des ports avec leur état (« Répond » / « Rien n'écoute »), l'ajout d'un port (Site web / TCP / UDP), les sous-domaines personnalisés et l'état de leur certificat HTTPS.
  - **Fichiers** : explorateur de `/workspace` (ou `/data`) avec éditeur (Ctrl+S), envoi (200 Mo max), création, renommage, suppression, téléchargement.
  - **Terminal** : la console de l'app (commandes RCON pour Minecraft Java, entrée standard pour PocketMine et les services, bash dans `/workspace` sinon) et un shell interactif xterm.js.
  - **Journaux** : logs du conteneur en direct, téléchargeables.
  - **Paramètres** : ressources (RAM / CPU, à chaud), mode de lancement, variables d'environnement, volumes, suppression.
- **Volumes en plus** de `/workspace`, à la création ou dans Paramètres : un nouveau volume pour un dossier de l'image (ex. `/var/lib/postgresql/data`), ou un volume existant, comme les données d'un projet supprimé. Les images du catalogue peuvent proposer leurs dossiers par défaut.
- **Serveurs Minecraft** : Java (`itzg/minecraft-server`, Paper, port `25565/tcp`) et Bedrock (`ghcr.io/pmmp/pocketmine-mp`, port `19132/udp`). Joueurs en ligne, console, monde et plugins dans l'onglet Fichiers. La mémoire Java suit la limite RAM du projet. Ouvre le port dans le pare-feu (`ufw allow 19132/udp`).

### Réseau
- **Sous-domaines automatiques** : `projet.dev.example.com` pointe vers le premier port web, `projet-5173.dev.example.com` vers un port précis. Les WebSockets passent (HMR de Vite/Next).
- **Privé par défaut** : un projet ne s'ouvre qu'aux profils connectés qui y ont accès. Un clic le rend public. Le cookie DevHub n'est jamais transmis aux projets.
- **Sous-domaines personnalisés** : `<nom>.example.com` (tape juste `app`), avec HTTPS automatique. Les projets privés restent protégés grâce à un passage de session à usage unique.
- **Ports TCP / UDP publics** (jeux) : publiés directement sur l'IP du serveur, sans HTTPS ni passkey. Les ports web restent sur `127.0.0.1`, joignables seulement par les sous-domaines.
- **Page 404** pour un sous-domaine inconnu, si le certificat wildcard est activé (voir Configuration).

### Serveur
- **Vue d'ensemble** : projets en ligne, CPU du serveur (colonnes 24 h / 7 j / 30 j avec l'heure et la valeur au survol), RAM réservée, activité récente.
- **Images** : le catalogue « Mes images » (image, nom, ports, volumes et mode par défaut), proposé en tuiles à la création d'un projet, et les images Docker du serveur avec les projets qui les utilisent, leur taille, leur suppression et un nettoyage.
- **Volumes** : les données de chaque projet (`/workspace`, `/data` pour les jeux) et les volumes ajoutés, avec leur taille et où ils sont montés. Un volume orphelin (monté par aucun projet) s'attache à un projet ou se supprime.
- **Ressources** : limites RAM / CPU du conteneur DevHub lui-même et limites par défaut des projets, appliquées à chaud.
- **Isolation** : chaque projet a son réseau, ses limites RAM / CPU / PIDs, `no-new-privileges` et des capabilities retirées. Il n'a **aucun** accès au socket Docker.
- **Journal** : chaque action (y compris celles de Claude), filtres par acteur, projet et période, export CSV.

### Accès
- **Passkeys** (WebAuthn), sans mot de passe, plusieurs appareils par profil.
- **Mon profil** : tes passkeys, ta photo et ton **compte GitHub**. Une fois connecté (« Se connecter avec GitHub » ou un jeton collé), DevHub télécharge les images ghcr.io privées, clone les dépôts privés, lit son propre dépôt pour les mises à jour et propose tes dépôts dans « Nouveau projet ».
- **Claude (MCP)** : une URL fixe, `https://dev.example.com/mcp`, à ajouter dans Claude (Réglages → Connecteurs → Ajouter un connecteur personnalisé). On se connecte avec son passkey et on clique sur « Autoriser » (OAuth 2.1 + PKCE). Claude agit avec les droits du profil qui l'a autorisé. Un interrupteur coupe l'accès sans changer l'URL, et chaque connexion se révoque.
- **MCP par projet** : une URL secrète par projet, limitée à ce seul conteneur (`run_command`, `read_file`, `write_file`, `edit_file`, `list_dir`, `project_info`), régénérable.

### Mises à jour
- **Page Mise à jour** : une seule carte selon l'état (à jour, nouveautés à installer, installation en 5 étapes en direct, échec avec la raison et « Réessayer »), le déploiement automatique, la source et l'historique des déploiements (durée, résultat).
- DevHub vérifie GitHub toutes les 30 min et propose « Mettre à jour » (`git pull`, reconstruction de DevHub depuis un conteneur temporaire, puis rechargement de Caddy). Caddy n'est reconstruit que si `caddy/Dockerfile` change : les sites ne sont jamais coupés.
- **Pas de page d'erreur** pendant le redémarrage : Caddy affiche « DevHub redémarre… », qui se recharge toute seule dès que le panel répond.
- **Déploiement automatique** (option) : chaque commit poussé est déployé environ 20 s après.
- **Compilation sur le serveur** avec un cache : seul le code de DevHub est recompilé, le panel redémarre en environ une minute.

## Déploiement sur le VPS

1. **DNS** : des enregistrements `A` vers l'IP du VPS pour `dev.example.com` et `*.dev.example.com`. Pour les sous-domaines personnalisés, ajoute `*.example.com`.
2. **Sur le VPS**, avec Docker installé :
   ```bash
   git clone https://github.com/Tweastly/DevHub devhub && cd devhub
   cp .env.example .env    # puis mets ton domaine dans DEVHUB_DOMAIN
   docker compose up -d --build
   docker compose logs devhub | grep "SETUP TOKEN"
   ```
3. Ouvre `https://dev.example.com/login`, colle le setup token et crée ton passkey. Le token ne sert qu'une fois.
4. **Pare-feu** : ouvre 80 et 443, plus les ports des jeux (TCP / UDP) si tu en héberges.

### Dépôt privé
Les mises à jour ont besoin d'un jeton GitHub *fine-grained* limité au dépôt, avec la seule permission **Contents : Read-only**. Mets-le dans `.env` (`DEVHUB_GITHUB_TOKEN=…`) puis `docker compose up -d`. Il n'est jamais écrit ailleurs sur le disque.

### Certificat wildcard (recommandé)
Par défaut, chaque projet reçoit son certificat à la première visite, et un nom inconnu provoque une erreur TLS dans le navigateur. Avec un certificat `*.dev.example.com`, tout sous-domaine a un HTTPS valide : les noms inconnus affichent la page 404 de DevHub et les nouveaux projets n'attendent plus.
1. Cloudflare → Mon profil → Jetons d'API → Créer un jeton : **Zone → DNS → Modifier**, zone `example.com`.
2. Dans `.env` sur le VPS :
   ```bash
   DEVHUB_TLS=wildcard
   CLOUDFLARE_API_TOKEN=...
   ```
3. `docker compose up -d caddy`. L'état est visible par MCP (`server_resources` → `https`, sans jamais montrer le jeton).

## Configuration

| Variable | Défaut | Rôle |
|---|---|---|
| `DEVHUB_DOMAIN` | `localhost` | domaine du panel (aussi l'identifiant WebAuthn des passkeys) |
| `DEVHUB_ORIGIN` | `https://$DOMAIN` | origine exacte du panel |
| `DEVHUB_PORT_RANGE` | `20000-20999` | plage des ports hôte des projets |
| `DEVHUB_MEMORY_MB` / `DEVHUB_CPUS` | `4096` / `2` | limites par défaut des projets (modifiables dans Ressources) |
| `DEVHUB_TLS` / `CLOUDFLARE_API_TOKEN` | `ondemand` / — | `wildcard` + un jeton Cloudflare : un certificat pour `*.DEVHUB_DOMAIN` |
| `DEVHUB_BIND_IP` | `0.0.0.0` (`127.0.0.1` en compose) | IP où les ports web des projets sont publiés |
| `DEVHUB_SELF_CONTAINER` | — (`devhub` en compose) | nom du conteneur DevHub ; active le proxy par réseau Docker |
| `DEVHUB_DATA` | `./data` | base SQLite |
| `DEVHUB_APPS_DOMAIN` | parent de `DEVHUB_DOMAIN` | domaine des sous-domaines personnalisés |
| `DEVHUB_GITHUB_TOKEN` | — | jeton lecture seule pour les mises à jour (dépôt privé) ; sinon le compte GitHub de Mon profil |
| `DEVHUB_GITHUB_CLIENT_ID` / `DEVHUB_GITHUB_CLIENT_SECRET` | — | OAuth App GitHub pour « Se connecter avec GitHub » (retour : `https://$DOMAIN/github/callback`) |
| `DEVHUB_REPO` | `/repo` | où le dépôt est monté dans le conteneur |
| `DEVHUB_PUBLIC_HOST` | `DEVHUB_DOMAIN` | nom affiché pour rejoindre les ports de jeu (ex. `mc.example.com`) |
| `HTTP_PORT` | `80` | port HTTP de Caddy s'il est déjà pris |

## Sécurité et secrets

Le dépôt ne contient aucun secret : tout ce qui est sensible vit **uniquement sur le serveur** et est exclu par `.gitignore`.

| Où | Contenu | Publié ? |
|---|---|---|
| `.env` | domaine, jeton GitHub, jeton Cloudflare, secret de l'OAuth App | non (`.gitignore`) |
| `data/devhub.sqlite` | jeton du compte GitHub connecté, passkeys (clés publiques), sessions, profils, jetons OAuth (hachés), URL MCP des projets, journal, variables d'environnement des projets | non (`/data` ignoré) |
| volumes Docker | code et données des projets | non (hors du dépôt) |
| `.env.example` | modèle sans valeurs | oui |

Avant de publier ou de pousser :
- ne mets jamais de vraie valeur dans `.env.example`, le README ou le code ;
- `git status` ne doit jamais lister `.env` ni `data/` ;
- si un jeton a été partagé ailleurs (chat, capture, ticket), révoque-le et recrée-le.

Les URL MCP par projet contiennent un jeton de 256 bits : traite-les comme des mots de passe et régénère-les si elles fuient.

## Dev local

```bash
DEVHUB_ORIGIN=http://localhost:8080 cargo run
```
Les navigateurs acceptent les passkeys sur `localhost` sans HTTPS, et les sous-domaines `projet.localhost:8080` fonctionnent pour les projets publics.
