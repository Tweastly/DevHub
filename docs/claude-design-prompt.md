# Prompt Claude Design — DevHub

Conçois l'interface complète de **DevHub**, un panel d'administration personnel et auto-hébergé. Il sert à gérer des projets de développement, chacun dans son propre conteneur Docker, sur un VPS. Je veux un design system cohérent et toutes les pages décrites ci-dessous, en **desktop et mobile (375 px)**, en **thème clair et sombre**. L'interface est **en français**.

---

## 1. Le produit

DevHub est utilisé par **une seule personne**, le propriétaire, développeur. Il héberge sur son VPS des projets web : sites, API, bots, serveurs de jeu. Chaque projet tourne dans un **conteneur Docker isolé** : réseau dédié, limites RAM/CPU, code dans `/workspace`.

Ce que DevHub permet :
- **Créer un projet** à partir d'un modèle (Node.js, Rust, Python, Go, Debian, « Page web » prête à l'emploi) ou d'une image Docker perso, avec un dépôt Git optionnel cloné automatiquement.
- **Piloter les conteneurs** : démarrer, arrêter, redémarrer, reconstruire, supprimer.
- **Exposer les projets sur le web** :
  - chaque projet est servi automatiquement sur `projet.dev.tweastly.com` (premier port) et `projet-5173.dev.tweastly.com` (port précis) ;
  - on peut ajouter des **sous-domaines personnalisés** `<nom>.tweastly.com` ;
  - le HTTPS est automatique ;
  - un projet est **privé par défaut** (il faut être connecté) et peut être rendu public.
- **Travailler dans le conteneur depuis le navigateur** : terminal web, gestionnaire de fichiers avec éditeur de texte, logs.
- **Régler les ressources** : RAM max et CPU max par projet, appliqués à chaud.
- **Connecter Claude (IA)** via MCP :
  - une URL secrète par projet, qui donne accès à un seul conteneur ;
  - une URL « admin » optionnelle, qui donne accès à tout le panel.
- **Se mettre à jour en un clic** quand une nouvelle version est publiée sur GitHub.
- **Sécurité** :
  - connexion **uniquement par passkey** (WebAuthn, sans mot de passe) ;
  - journal d'audit de toutes les actions, du propriétaire comme de Claude.

**Ton recherché** : un outil de dev sérieux, sobre et dense en informations, mais chaleureux. Inspirations : **Pocket ID** pour les écrans d'authentification (carte centrée, minimaliste), **Vercel / Railway / Coolify / Linear** pour le panel. Pas de fioritures marketing : c'est un outil de travail quotidien.

---

## 2. Identité visuelle existante (à faire évoluer, pas à jeter)

- **Logo** : un hexagone `⬢` blanc dans un carré arrondi (rayon ~14 px) à dégradé diagonal **#6ea8ff → #b78cff** (bleu → violet). Nom : « DevHub ».
- **Neutres actuels** : palette type *zinc*.
  - Clair : fond `#f4f4f5`, cartes `#ffffff`, bordures `#e4e4e7`, texte `#18181b`, texte secondaire `#71717a`.
  - Sombre : fond `#09090b`, cartes `#18181b`, bordures `#27272a`, texte `#fafafa`, secondaire `#a1a1aa`.
- **Accent** : bleu `#6ea8ff` en sombre, `#2f6fed` en clair.
- **Couleurs d'état** : succès / running `#3ecf8e`, erreur / arrêté `#ff6b6b`, avertissement `#f5b84a`.
- **Typographie** : system-ui pour l'interface, ui-monospace pour le code, les chemins, les URLs, les logs et le terminal.
- **Formes** : cartes à bordure 1 px, rayon 10–16 px, ombre très légère. Boutons rayon 6–10 px.
- Le thème suit `prefers-color-scheme`.

Livre des **tokens** (couleurs, espacements, rayons, ombres, typo) en clair et en sombre, avec des contrastes WCAG AA.

---

## 3. Contraintes techniques (importantes pour que le design soit implémentable)

- Le HTML est **rendu côté serveur** (Rust + maud). Les actions sont des **formulaires POST classiques** suivis d'une redirection : pas de SPA.
- **JavaScript minimal**, seulement là où c'est indispensable : passkey (WebAuthn), terminal (xterm.js), éditeur de texte (Tab, Ctrl+S), confirmations (`confirm()`). Tout le reste doit fonctionner sans JS.
- Pas de framework CSS lourd : du CSS maison avec des variables. Tu peux proposer des composants, mais ils doivent rester réalisables en HTML + CSS simple.
- Les pages se chargent en quelques millisecondes : pas besoin de skeletons partout. Prévois quand même les **états vides** et les **états d'erreur**.
- Le panel doit rester parfaitement utilisable sur **mobile** : je gère mon serveur depuis mon téléphone.
- Icônes : un set simple et cohérent (type Lucide), en SVG inline.

---

## 4. Navigation globale (panel connecté)

En-tête persistant :
- à gauche, logo + « DevHub » ;
- liens : **Projets**, **Passkeys**, **Claude**, **Journal**, **Mise à jour** ;
- à droite, bouton **Déconnexion**.

Sur mobile, un menu compact (burger ou barre d'onglets en bas, à toi de proposer).

**Bandeau de mise à jour**, affiché en haut de toutes les pages du panel quand c'est pertinent :
- « 🆕 **3 nouvelle(s) version(s)** disponible(s) sur GitHub. » + lien « Voir les changements » + bouton **Mettre à jour** (avec confirmation).
- Variante pendant la mise à jour : « ⏳ Mise à jour en cours… DevHub va redémarrer. » + lien « Suivre ».

---

## 5. Écrans d'authentification et pages publiques (style Pocket ID)

Tous partagent le même gabarit :
- une **carte centrée** (max ~400 px) sur fond neutre ;
- le logo en haut, puis un titre, un sous-titre et **un bouton principal pleine largeur** (noir en clair, blanc en sombre) avec une icône de clé ;
- en pied de page, en petit : « Protégé par DevHub · dev.tweastly.com ».

### 5.1 Connexion
- Titre « Se connecter à DevHub ».
- Sous-titre : « Utilise ton passkey pour continuer. » ou, si l'utilisateur arrive d'un projet, « pour accéder à **mon-site.dev.tweastly.com** ».
- Bouton « 🔑 Se connecter avec un passkey ».
- Zone de message d'erreur discrète (rouge) sous le bouton, par ex. « Erreur : opération annulée ».

### 5.2 Première configuration (aucun passkey encore)
- Titre « Bienvenue sur DevHub ».
- Texte : « Colle le setup token affiché dans les logs du serveur, puis crée ton passkey. »
- Champs : **Setup token**, **Nom de l'appareil** (pré-rempli « Mon appareil »).
- Bouton « Créer mon passkey ».

### 5.3 Projet privé (visiteur non connecté sur un sous-domaine de projet)
- Titre « Projet privé ».
- Texte : « **mon-site** est réservé au propriétaire. Connecte-toi pour continuer. »
- Bouton « Se connecter avec un passkey ».

### 5.4 Pages d'erreur du proxy
Même carte, avec un petit code HTTP en monospace au-dessus du titre (`503`, `502`, `404`, `403`) :
- **503 · Projet arrêté** : « **test** n'est pas démarré pour le moment. »
- **502 · Le serveur ne répond pas** : « Rien n'écoute sur le port **8000** dans **test**. Lance ton serveur en écoutant sur `0.0.0.0`. »
  - Pour le propriétaire seulement, un bloc monospace avec le détail technique (« Connection refused (os error 111) »).
- **404 · Introuvable** : « Aucun projet ne correspond à « nope ». »
- **403 · Lien expiré** : « Ce lien de connexion n'est plus valable. Recharge la page du projet. »

Boutons : « ↻ Réessayer » (principal) et, pour le propriétaire, « Ouvrir dans DevHub » (secondaire, contour).

---

## 6. Dashboard — liste des projets

### 6.1 Tableau / cartes des projets
Pour chaque projet :
- **Nom** (lien vers sa page) et modèle en dessous, en gris (« node », « site », « custom »…).
- **État** : pastille `running` (vert), `exited` (rouge), `created`, `absent` (projet en cours de construction ou conteneur manquant).
- **CPU** en %, par ex. « 3.4 % ».
- **Mémoire** : « 212 Mo / 1024 Mo » avec une fine barre de progression.
- **Accès** : lien « ouvrir ↗ » vers le sous-domaine, liste des ports (« 3000 5173 »), pastille « public » si le projet est public.
- **Actions rapides** : Stop / Restart si le projet tourne, Start sinon.

États à prévoir :
- **vide** : « Aucun projet. Crée le premier ci-dessous. » ;
- 1 projet ;
- une quinzaine de projets.

Sur mobile, le tableau devient une liste de cartes.

### 6.2 Création de projet
Formulaire « Nouveau projet » :
- **Nom** : a-z, 0-9, tirets, 32 caractères max. Exemple : `mon-portfolio`.
- **Modèle**, liste :
  - Page web (modèle avec le nom du projet) ;
  - Node.js 22 ;
  - Rust stable ;
  - Python 3.12 ;
  - Go 1.23 ;
  - Debian + outils ;
  - Image personnalisée…
  
  Propose quelque chose de plus visuel qu'un `<select>`, comme des tuiles avec une icône par techno, tout en restant faisable sans JS (radio buttons stylés).
- **Image perso** : seulement si « Image personnalisée », ex. `ghcr.io/moi/env:latest`.
- **Dépôt Git** (optionnel), ex. `https://github.com/moi/projet.git`.
- **Ports à exposer**, ex. `3000, 5173`. Le modèle « Page web » prend 8000 par défaut.
- Bouton « Créer le projet ».

Phrase d'aide : « Le modèle choisit l'image Docker de base (les outils déjà installés). Le dépôt Git, optionnel, est cloné dans /workspace au premier démarrage. »

---

## 7. Page d'un projet

C'est la page la plus riche. Elle doit rester **lisible et hiérarchisée** : propose des sections, ou des onglets si tu juges que c'est mieux (attention, onglets sans JS = liens vers des sous-pages).

### 7.1 En-tête
- Nom du projet (gros), pastille d'état, image Docker en gris (`node:22-bookworm`).
- Ligne de stats : « CPU 3.4 % · Mémoire 212 Mo / 1024 Mo ».
- Actions : **Start** (principal), **Stop**, **Restart**, **Rebuild**. Rebuild a une info-bulle : « Recrée le conteneur, garde /workspace ».
- Raccourcis : **Fichiers →**, **Terminal →**, **Logs →**.
- Lien principal du projet : `https://mon-site.dev.tweastly.com` ↗.

### 7.2 Connexion Claude Desktop (MCP)
- Texte : « Dans Claude Desktop : **Réglages → Connecteurs → Ajouter un connecteur personnalisé**, colle cette URL : »
- L'URL longue et secrète, en monospace, dans un bloc avec un bouton **Copier** : `https://dev.tweastly.com/mcp/4l1dZXFr3FyDT6n7lQ6ua6Xn6mQOONjDczYlpKNT6u4`.
- Avertissement discret : « Cette URL est secrète et ne donne accès qu'à ce conteneur. Si elle fuite, régénère-la. »
- Bouton secondaire « Régénérer l'URL » (avec confirmation).

### 7.3 Ports & sous-domaines
- Tableau : **port du conteneur** | **URLs** (`https://mon-site.dev.tweastly.com` pour le premier port, puis `https://mon-site-3000.dev.tweastly.com`) | **port hôte** en gris (20000).
- Champ « Ports conteneur (séparés par des virgules) » + bouton « Appliquer (rebuild) ».
- Interrupteur d'accès : « Accès : **privé (passkey)** » ↔ « **public** », avec le bouton « Rendre public » / « Rendre privé ». Ce choix est important, il doit être bien visible.

### 7.4 Sous-domaines personnalisés
- Texte : « Sers ce projet sur `<nom>.tweastly.com`. Il faut un enregistrement DNS `*.tweastly.com` vers l'IP du VPS (une seule fois pour tous). Le certificat HTTPS est créé à la première visite. »
- Liste des sous-domaines : `new.tweastly.com` → port 8000 [Retirer].
- Formulaire d'ajout : un champ `new` **collé à un suffixe fixe `.tweastly.com`** (input group), un sélecteur de port et le bouton « Ajouter ».
- Messages d'erreur possibles :
  - « new.tweastly.com est déjà utilisé par le projet app » ;
  - « sous-domaine invalide : indique seulement le nom, par ex. « new » pour new.tweastly.com » ;
  - « dev.tweastly.com est déjà géré automatiquement ».

### 7.5 Ressources
- Deux jauges : **RAM utilisée** « 212 Mo / 1024 Mo » et **CPU utilisé** « 3.4 % / 100 % » (100 % = 1 cœur).
- Champs **RAM max (Mo)** (placeholder = défaut 4096) et **CPU max (cœurs)** (décimales, ex. 1.5 ; défaut 2).
- Bouton « Appliquer (sans redémarrage) ». Aide : « Laisse vide pour revenir au défaut. »
- Ligne serveur : « Serveur : 4 cœurs, 16091 Mo. Tous les projets réunis peuvent utiliser jusqu'à 9216 Mo et 7.5 cœurs. » + un avertissement ⚠ si c'est plus que la RAM du serveur.

### 7.6 Variables d'environnement
- Grande zone de texte monospace, une variable par ligne : `CLE=valeur`, `DATABASE_URL=postgres://…`.
- Bouton « Appliquer (rebuild) ».
- Propose de masquer les valeurs sensibles, ou une vue clé/valeur plus lisible, si c'est faisable sans JS lourd.

### 7.7 Zone dangereuse
- Case « Garder le volume de données ».
- Bouton rouge « Supprimer le projet », avec confirmation forte.

---

## 8. Gestionnaire de fichiers (`/projects/<nom>/files`)

- **Fil d'Ariane** cliquable : `workspace / src / components`.
- **Liste** : icône dossier, fichier ou lien ; nom ; taille (« 1.8 Ko ») ; date (« 2026-10-25 19:59 »).
  - Actions par ligne : Télécharger (⬇), Renommer (demande le nouveau nom), Supprimer (confirmation, « et tout son contenu » pour un dossier).
  - Ligne « .. » pour remonter.
  - État vide : « Dossier vide. »
- **Envoyer des fichiers** : sélection multiple, destination « Vers /workspace/src », « 200 Mo max par envoi. Les fichiers existants sont remplacés. » Propose une zone de glisser-déposer qui reste compatible avec un simple `<input type=file>`.
- **Créer** : champ Nom + boutons « Nouveau fichier » / « Nouveau dossier ».
- **Éditeur de fichier texte** :
  - nom du fichier, taille, lien retour au dossier ;
  - grande zone d'édition monospace (65 % de la hauteur d'écran), pas de retour à la ligne automatique, tabulations ;
  - bouton « Enregistrer » + indication « Ctrl+S pour enregistrer » + lien « Télécharger ».
- **Fichier non éditable** (binaire ou > 2 Mo) : « Ce fichier n'est pas modifiable ici (binaire ou plus de 2 Mo). » + Télécharger.

---

## 9. Terminal (`/projects/<nom>/terminal`)

- Titre « Terminal · mon-site », lien retour, état de connexion (« connexion… », « connecté », « déconnecté — recharge la page »).
- Grande zone terminal (xterm.js), fond noir, ~70 % de la hauteur d'écran. Exemple de contenu : `root@mon-site:/workspace# npm run dev`.
- Sur mobile, prévois une barre de touches utiles (Tab, Ctrl, Échap, flèches) si possible.

## 10. Logs (`/projects/<nom>/logs`)

- Titre « Logs · mon-site », lien retour, mention « rafraîchi toutes les 5 s ».
- Bloc monospace qui défile, avec horodatage en tête de ligne : `2026-10-25T13:52:59Z  GET / 200 3ms`.

---

## 11. Passkeys (`/security`)

- Liste des passkeys (« MacBook », « iPhone ») avec un bouton « Retirer ». Le retrait est impossible s'il n'en reste qu'un.
- Ajout d'un passkey : champ Nom (« Nouvel appareil ») + bouton « Ajouter » ; zone de message.

## 12. Claude (`/claude`)

- **Accès complet au panel** :
  - Explication : « Avec cette URL, Claude Desktop peut **tout faire dans DevHub** : créer, configurer, redémarrer et supprimer des projets, travailler dans n'importe quel conteneur, lire les logs et lancer la mise à jour. »
  - Note rassurante : « Il ne peut ni toucher à tes passkeys, ni modifier cette URL : tu gardes toujours la main. Toutes ses actions apparaissent dans le journal (acteur « claude-admin »). »
  - État **désactivé** : bouton « Activer l'accès complet ».
  - État **activé** : l'URL en monospace + Copier, puis « Régénérer l'URL » et « Désactiver » (rouge).
- **Accès limité à un seul projet** : tableau projet → URL MCP dédiée.

Cette page doit faire ressentir le niveau de pouvoir donné à l'IA, sans être anxiogène.

## 13. Journal d'audit (`/audit`)

Tableau : **Date (UTC)** | **Acteur** | **Action** | **Détail**.
- Acteurs, avec un badge de couleur par acteur : `owner`, `claude` (MCP projet), `claude-admin`, `system`, `?` (tentative échouée).
- Exemples :
  - `owner` · `project.create` · `mon-site (node:22-bookworm)`
  - `claude` · `mon-site:run_command` · `npm install`
  - `claude-admin` · `project.resources` · `app : RAM 1024 Mo, CPU 1`
  - `system` · `project.error` · `demo: pull … 429 Too Many Requests`
  - `owner` · `file.save` · `app:index.html`
  - `?` · `login.fail` · `…`
- Propose des filtres simples par acteur (liens).

## 14. Mise à jour (`/update`)

- « Version installée : `a137fbe` · branche `main` · Tweastly/DevHub ».
- « Dernière vérification : 2026-10-25 16:55 UTC (toutes les 30 min) ».
- Boutons « Vérifier maintenant » et « Mettre à jour ».
- Liste des changements à venir : `57df324` Dockerfile: copy templates/ into the build, …
- États :
  - « ✅ DevHub est à jour. » ;
  - erreur en rouge (« GitHub a répondu 401 Unauthorized (dépôt privé ? définis DEVHUB_GITHUB_TOKEN) ») ;
  - **mise à jour en cours**, avec les logs du conteneur de mise à jour dans un bloc monospace qui se rafraîchit.

## 15. Page d'accueil générée par le modèle « Page web »

Page d'exemple servie dans un projet neuf :
- au centre, une pastille « ● en ligne » ;
- le **nom du projet en très grand**, en dégradé bleu → violet ;
- « Ton projet est prêt. Cette page est servie depuis son conteneur DevHub. » ;
- une carte « Pour commencer » (modifier `/workspace/index.html`, `start.sh`…).

Elle doit être belle par défaut, parce qu'elle sert de vitrine. Clair et sombre.

---

## 16. Ce que j'attends en sortie

1. **Design system** : tokens clair/sombre, typographie, espacements, rayons, ombres, icônes.
2. **Composants**, avec tous leurs états (normal, survol, focus, désactivé, erreur) :
   - boutons : principal, secondaire contour, ghost, danger ;
   - champs : input, input group avec suffixe, select, textarea code, file drop ;
   - pastilles d'état et badges d'acteur ;
   - barres de jauge, cartes, tableaux et leur version mobile en cartes ;
   - bloc de code copiable, bandeau, fil d'Ariane, interrupteur public/privé, message d'erreur de formulaire.
3. **Tous les écrans** des sections 5 à 15, en desktop et en mobile, en clair et en sombre.
4. Les **états vides, d'erreur et de chargement** importants.
5. Une courte note sur la **hiérarchie** de la page projet : ce qui est en haut, ce qui est replié, et pourquoi.
