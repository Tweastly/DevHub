# DevHub : règles pour Claude

## Où tourne DevHub
- DevHub tourne **uniquement dans des conteneurs Docker** : le panel (`devhub`), Caddy (`caddy`), les projets (`devhub-<nom>`), et les tâches ponctuelles (installation, mise à jour) dans des conteneurs temporaires.
- Rien d'autre ne s'installe ni ne tourne sur le VPS que Docker lui-même : pas de binaire, de service systemd, de paquet ou de règle système ajoutés par DevHub. Une nouvelle fonction qui a besoin de l'hôte passe par un montage ou par le socket Docker, jamais par un programme installé sur le VPS.
- Sur le VPS, seulement des fichiers : `/opt/devhub` (code, `.env`, `data/`), montés dans les conteneurs.
- Installation recommandée : `docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock -v /opt/devhub:/opt/devhub ghcr.io/tweastly/devhub:install` (voir `installer/Dockerfile` et `install.sh`).

## Avec ou sans nom de domaine
- Avec un domaine : passkeys, un sous-domaine par projet (Caddy en bridge, `Caddyfile`).
- Sans domaine, mode « IP seule » (`DEVHUB_DOMAIN` = une IP, `cfg.ip_mode`) : connexion par mot de passe (les passkeys refusent une IP), un port par projet web (`https://IP:20000`…), Caddy sur le réseau du serveur (`docker-compose.ip.yml`, `caddy/Caddyfile.ip`). Ou `dev.<IP>.sslip.io`, qui se comporte comme un vrai domaine.

## Accès au VPS
- DevHub a la main sur le VPS depuis son conteneur, pour piloter les projets (socket Docker) et lire les statistiques du serveur (CPU, mémoire, disque, durée de fonctionnement…).
- Le MCP (assistants IA) n'a pas cet accès à l'hôte : ses outils restent limités aux projets et aux réglages de DevHub. Pas de commande, de fichier ni de contrôle sur le VPS lui-même par le MCP.

## Façon de travailler
- Répondre à l'utilisateur en français.
- Commits : auteur `TT Royal <78960032+TTRoyal@users.noreply.github.com>`, sans lignes `Co-Authored-By` ni `Claude-Session`, sans identifiant de modèle.
- Ne jamais lire ni commiter `.env` ni `data/`. Le dépôt est public : aucun secret, jeton ou domaine réel (utiliser `example.com`).
- Pas de force-push ni de réécriture d'historique sans demande explicite.
- Ne pas lancer la mise à jour de DevHub en production (`devhub_update` « apply ») sans demande : pousser sur `main`, l'utilisateur installe.
- Images publiées par GitHub dans un seul paquet : `ghcr.io/tweastly/devhub` (tags `latest`, `caddy`, `install`).
