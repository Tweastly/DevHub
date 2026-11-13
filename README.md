# DevHub

Panel auto-hébergé (Rust) pour faire tourner tes projets sur un VPS : un conteneur Docker isolé par projet, un sous-domaine HTTPS automatique, une connexion uniquement par passkey, et un accès MCP pour que **Claude travaille directement dans les conteneurs**.

```
Navigateur ──passkey──▶ [ conteneur devhub : Caddy (HTTPS) ──▶ DevHub (Rust, axum) ] ──socket──▶ Docker
Claude ──https://dev.example.com/mcp (OAuth)─┘                                                    ├─ devhub-projet-a (+ volume)
                                                                                                  └─ devhub-projet-b (+ volume)
```

DevHub tient **entièrement dans Docker** : un seul conteneur, `devhub` (le panel et son Caddy pour le HTTPS, que DevHub lance lui-même), sa base dans le volume `devhub-data`, ses certificats dans `devhub-caddy`, ses réglages dans le conteneur. Rien n'est écrit sur le serveur en dehors de Docker. Chaque projet a son propre conteneur.

Dans ce README, `dev.example.com` est le domaine du panel et `example.com` son domaine parent : remplace-les par les tiens.

## Fonctionnalités

### Projets
- **Création** à partir d'un modèle (Page web, Node, Rust, Python, Go, Debian, Minecraft Java, Minecraft Bedrock) ou de n'importe quelle image Docker, avec un dépôt Git optionnel cloné dans `/workspace`.
- **Deux modes de lancement** pour une image perso : *Service* (la commande de l'image tourne, la console écrit sur son entrée standard) ou *Environnement de dev* (le conteneur reste allumé et lance `/workspace/start.sh` s'il existe, logs dans `/tmp/start.log`).
- **Confirmations** : un dialogue DevHub (neutre, attention, danger) remplace ceux du navigateur ; les suppressions de données irréversibles demandent de taper le nom.
- **Actions sans rechargement** : chaque bouton s'exécute en arrière-plan, la page se met à jour sur place et le résultat s'affiche en notification.
- **Liste des projets** en cartes ou en liste : état, CPU et RAM en direct, badges (accès, ports, domaines, durée), filtres et tri. **Sélection multiple** pour démarrer, arrêter, redémarrer, recréer ou supprimer plusieurs projets d'un coup.
- **Tous les conteneurs du serveur** dans cette liste (administrateurs) : ceux créés hors de DevHub (compose, `docker run`, autres outils) apparaissent sous les projets, avec leur état, CPU et RAM en direct, leur origine et leurs ports, une page avec leurs informations et leurs journaux, et Démarrer / Arrêter / Redémarrer. Le conteneur de DevHub lui-même y figure, sans ces boutons.
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
- **Français / English** : le panel, le guide d'installation et la page de connexion passent en anglais d'un clic (menu du profil, ou le lien en bas de la connexion et en haut du guide). Le choix est gardé dans le navigateur ; les pages des projets ne sont jamais touchées.

### Accès
- **Passkeys** (WebAuthn), sans mot de passe, plusieurs appareils par profil.
- **Mon profil** : tes passkeys, ta photo et ton **compte GitHub**. Une fois connecté (« Se connecter avec GitHub » ou un jeton collé), DevHub télécharge les images ghcr.io privées, clone les dépôts privés, lit son propre dépôt pour les mises à jour et propose tes dépôts dans « Nouveau projet ».
- **Claude (MCP)** : une URL fixe, `https://dev.example.com/mcp`, à ajouter dans Claude (Réglages → Connecteurs → Ajouter un connecteur personnalisé). On se connecte avec son passkey et on clique sur « Autoriser » (OAuth 2.1 + PKCE). Claude agit avec les droits du profil qui l'a autorisé. Un interrupteur coupe l'accès sans changer l'URL, et chaque connexion se révoque.
- **MCP par projet** : une URL secrète par projet, limitée à ce seul conteneur (`run_command`, `read_file`, `write_file`, `edit_file`, `list_dir`, `project_info`), régénérable.

### Mises à jour
- **Page Mise à jour** : une seule carte selon l'état (à jour, nouveautés à installer, installation en 4 étapes en direct, échec avec la raison et « Réessayer »), le déploiement automatique, la source et l'historique des déploiements (durée, résultat).
- **Progression en continu et temps restant** : pendant l'installation, la barre suit les octets réellement téléchargés, puis la durée habituelle du redémarrage (DevHub retient la durée de chaque phase des dernières mises à jour). Elle avance aussi pendant les quelques secondes où DevHub redémarre, sans jamais reculer, et affiche « Fin dans environ 30 s ».
- DevHub vérifie GitHub toutes les 30 min et propose « Mettre à jour » : il télécharge l'image que GitHub a construite pour le nouveau commit, puis un conteneur temporaire de cette image remplace le conteneur `devhub` (mêmes volumes, mêmes réglages ; l'ancien est remis en route si le nouveau ne démarre pas). Le panel et les sites des projets sont coupés quelques secondes ; les projets eux-mêmes continuent de tourner.
- La page de mise à jour suit le redémarrage et reprend toute seule dès que le panel répond.
- **Déploiement automatique** (option) : chaque commit poussé est déployé environ 20 s après.

## Déploiement sur le VPS

1. **Sur le VPS**, une seule commande (root ou sudo), qui ne pose aucune question :
   ```bash
   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub
   ```
   C'est l'image de DevHub elle-même : lancée ainsi, sans autre réglage, elle s'installe. Elle n'utilise que le Docker du serveur : elle télécharge l'image de DevHub, crée les volumes `devhub-data` et `devhub-caddy` et le conteneur `devhub`, démarre DevHub sur l'**adresse IP du serveur** et affiche l'adresse du guide et le **code d'installation**. Aucun fichier n'est écrit sur le serveur. Relancé, il met DevHub à jour en gardant ses données et ses réglages.
   Sans Docker sur le serveur, le même script l'installe d'abord : `curl -fsSL https://raw.githubusercontent.com/Tweastly/DevHub/main/install.sh | sh`.
2. **Dans ton navigateur**, ouvre `https://<IP du serveur>/setup` (le navigateur peut avertir du certificat la première fois : continue) et tape le code. L'**assistant d'installation** fait tout le reste :
   - **Nom de domaine** : si tu en as un, il affiche les deux enregistrements DNS à créer (`dev.example.com` et `*.dev.example.com` vers l'IP du serveur) — ou les crée lui-même chez Cloudflare avec un jeton « Zone › DNS › Modifier » collé une fois et jamais gardé —, les vérifie, puis fait redémarrer DevHub sur le domaine et t'y emmène avec ton code. Sinon, DevHub reste sur l'adresse IP (voir plus bas).
   - **Ton compte** : un passkey avec un domaine, un mot de passe sans.
   - Puis le HTTPS, les ressources, GitHub, les mises à jour, les assistants IA et ton premier projet.

   Le code ne sert qu'une fois, et il survit au redémarrage sur le domaine (après 20 essais faux, un nouveau s'affiche dans `docker logs devhub`). Une fois l'installation finie, le guide ne revient plus : chaque réglage se change sur sa page : **Adresse et domaine** (passer d'une IP à un domaine et inversement), Ressources, Mise à jour, Assistants IA, Mon profil (GitHub, connexion).
3. **Pare-feu** : 80 et 443 doivent être ouverts (plus 20000-20099 sans domaine, et les ports des jeux si tu en héberges). DevHub ne touche pas au pare-feu du serveur.

Les options s'ajoutent à la fin de la commande, à l'installation comme plus tard pour changer un réglage : `--domain dev.example.com` démarre directement sur le domaine, `--ip` revient à l'adresse IP, `--cloudflare-token` active le certificat joker, `--env CLÉ=VALEUR` règle n'importe quelle variable de la table plus bas (`--help` les liste). Exemple : `docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub --domain dev.example.com`.

<details><summary>Sans installateur : une seule commande qui crée directement DevHub</summary>

La commande courte lance un installateur temporaire qui crée le conteneur `devhub` puis disparaît (`--rm`). Celle-ci crée directement le conteneur `devhub`, avec tout ce qu'il lui faut :

```bash
docker run -d --name devhub --restart unless-stopped --network host --init \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v devhub-data:/data -v devhub-caddy:/caddy \
  ghcr.io/tweastly/devhub
```

Nommée `devhub`, l'image se reconnaît comme le panel : réseau du serveur, Caddy, DevHub joignable seulement par Caddy, et l'adresse IP publique du serveur trouvée toute seule (sinon `-e DEVHUB_PUBLIC_IP=…`, ou `-e DEVHUB_DOMAIN=dev.example.com` pour un domaine). Le code d'installation : `docker logs devhub | grep CODE`. Les mises à jour se font ensuite depuis le panel, comme avec la commande courte.
</details>

**Installé avant, avec docker compose dans `/opt/devhub` ?** La mise à jour suivante fait passer DevHub dans les volumes Docker tout seul (sa base et ses certificats sont copiés, ses réglages repris du conteneur), ou la page Mise à jour propose « Passer dans Docker ». Relancer l'installateur fait de même. Le dossier reste en place : supprime-le une fois que tout va bien.

**Sauvegarde** de la base : `docker run --rm -v devhub-data:/d -v "$PWD":/b debian tar czf /b/devhub-data.tgz -C /d .`

L'image de DevHub (`ghcr.io/tweastly/devhub:latest`, Caddy et l'installateur compris, une étiquette par commit) est construite par GitHub à chaque push ; `:install` en est un alias qui démarre directement sur l'installateur (anciennes commandes). Le paquet doit être public pour qu'un serveur les télécharge sans compte (GitHub › Packages › devhub › Package settings › Change visibility).

### Sans nom de domaine

C'est le choix « Non » de la première étape du guide, et le mode de départ de toute installation :

| | Avec un domaine | Sans domaine |
|---|---|---|
| Adresse du panel | `https://dev.example.com` | `https://203.0.113.7` |
| Connexion | passkey | mot de passe (12 caractères min.) |
| Projets web | `https://projet.dev.example.com` | `https://203.0.113.7:20000`, `:20001`… (un port par projet) |
| Certificat | Let's Encrypt par projet, ou joker | Let's Encrypt pour l'IP (6 jours, renouvelé seul), sinon celui de Caddy avec un avertissement du navigateur |
| Ports à ouvrir | 80, 443 | 80, 443 et 20000-20099 |

- Sans domaine, les passkeys sont impossibles (WebAuthn refuse une adresse IP), d'où le mot de passe, limité à 5 essais faux par adresse et par quart d'heure. Les projets privés restent protégés : il faut être connecté à DevHub pour les ouvrir. Un projet peut quand même avoir son propre nom de domaine (onglet Réseau : un enregistrement A vers l'IP, HTTPS à la première visite). DevHub le reconnaît à l'IP dans `DEVHUB_DOMAIN` et lance Caddy avec `caddy/Caddyfile.ip`.
- **Passer plus tard à un domaine** (une fois le compte créé) : **Adresse et domaine › Passer à un nom de domaine**, dans le navigateur. Les mêmes enregistrements DNS à créer (ou à faire créer par Cloudflare), leur vérification, puis DevHub redémarre sur le domaine en gardant les projets, et la page t'y emmène créer ton passkey. Sur le serveur, l'installateur avec `--domain dev.example.com` fait de même, et `--ip` le chemin inverse.

### Certificat wildcard (recommandé)
Par défaut, chaque projet reçoit son certificat à la première visite, et un nom inconnu provoque une erreur TLS dans le navigateur. Avec un certificat `*.dev.example.com`, tout sous-domaine a un HTTPS valide : les noms inconnus affichent la page 404 de DevHub et les nouveaux projets n'attendent plus.
1. Cloudflare → Mon profil → Jetons d'API → Créer un jeton : **Zone → DNS → Modifier**, zone `example.com`.
2. Colle-le dans l'assistant d'installation (étape « Adresse et HTTPS › Passer au certificat joker »), ou relance l'installateur avec `--cloudflare-token …`. DevHub redémarre avec son Caddy en quelques secondes. L'état est visible par MCP (`server_resources` → `https`, sans jamais montrer le jeton).

## Configuration

Les réglages sont les variables du conteneur `devhub` : l'installateur les pose (`--env CLÉ=VALEUR`, `--domain`, `--cloudflare-token`) et les garde d'une mise à jour à l'autre.

| Variable | Défaut | Rôle |
|---|---|---|
| `DEVHUB_DOMAIN` | `localhost` | domaine du panel (aussi l'identifiant WebAuthn des passkeys) |
| `DEVHUB_ORIGIN` | `https://$DOMAIN` | origine exacte du panel |
| `DEVHUB_PORT_RANGE` | `20000-20999` (`20000-20099` sans domaine) | plage des ports hôte des projets |
| `DEVHUB_MEMORY_MB` / `DEVHUB_CPUS` | `4096` / `2` | limites par défaut des projets (modifiables dans Ressources) |
| `DEVHUB_TLS` / `CLOUDFLARE_API_TOKEN` | `ondemand` / — | `wildcard` + un jeton Cloudflare : un certificat pour `*.DEVHUB_DOMAIN` |
| `DEVHUB_BIND_IP` | `0.0.0.0` (`127.0.0.1` installé) | IP où les ports web des projets sont publiés |
| `DEVHUB_SELF_CONTAINER` | — (`devhub` installé) | nom du conteneur DevHub ; active le proxy vers les conteneurs des projets |
| `DEVHUB_LISTEN` | `0.0.0.0:8080` (`127.0.0.1:8080` installé) | adresse de DevHub derrière Caddy |
| `DEVHUB_CADDY` | — (`on` installé) | DevHub lance Caddy dans son conteneur |
| `DEVHUB_DATA` | `./data` (`/data`, le volume `devhub-data`, dans l'image) | base SQLite |
| `DEVHUB_APPS_DOMAIN` | parent de `DEVHUB_DOMAIN` | domaine des sous-domaines personnalisés |
| `DEVHUB_GITHUB_TOKEN` | — | jeton lecture seule pour les mises à jour (dépôt privé) ; sinon le compte GitHub de Mon profil |
| `DEVHUB_GITHUB_CLIENT_ID` / `DEVHUB_GITHUB_CLIENT_SECRET` | — | OAuth App GitHub pour « Se connecter avec GitHub » (retour : `https://$DOMAIN/github/callback`) |
| `DEVHUB_REVISION` / `DEVHUB_GITHUB_REPO` | posés par GitHub dans l'image | commit de l'image et dépôt suivi pour les mises à jour |
| `DEVHUB_PUBLIC_HOST` | `DEVHUB_DOMAIN` | nom affiché pour rejoindre les ports de jeu (ex. `mc.example.com`) |
| `HTTP_PORT` | `80` | port HTTP de Caddy s'il est déjà pris |

## Sécurité et secrets

Le dépôt ne contient aucun secret : tout ce qui est sensible vit **uniquement dans le Docker du serveur**.

| Où | Contenu | Publié ? |
|---|---|---|
| variables du conteneur `devhub` | domaine, jeton GitHub, jeton Cloudflare, secret de l'OAuth App | non |
| volume `devhub-data` (`devhub.sqlite`) | jeton du compte GitHub connecté, passkeys (clés publiques), sessions, profils, jetons OAuth (hachés), URL MCP des projets, journal, variables d'environnement des projets | non |
| autres volumes Docker | code et données des projets, certificats | non |
| `.env.example` | modèle sans valeurs (anciennes installations par compose) | oui |

Avant de publier ou de pousser :
- ne mets jamais de vraie valeur dans `.env.example`, le README ou le code ;
- `git status` ne doit jamais lister `.env` ni `data/` (dev local, anciennes installations) ;
- si un jeton a été partagé ailleurs (chat, capture, ticket), révoque-le et recrée-le.

Les URL MCP par projet contiennent un jeton de 256 bits : traite-les comme des mots de passe et régénère-les si elles fuient.

## Dev local

```bash
DEVHUB_ORIGIN=http://localhost:8080 cargo run
```
Les navigateurs acceptent les passkeys sur `localhost` sans HTTPS, et les sous-domaines `projet.localhost:8080` fonctionnent pour les projets publics.
