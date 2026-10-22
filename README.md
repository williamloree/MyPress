# MyPress — presse-papiers universel

Prototype des phases 1 et 2 du [cahier des charges](input/cahier_des_charges_presse_papiers_universel.md) :
application Windows (Tauri 2 + Rust), synchronisation **pair à pair** entre vos ordinateurs, boîte aux
lettres optionnelle, texte uniquement, chiffrement de bout en bout.

## Organisation

| Dossier | Rôle |
|---|---|
| `crates/proto` | Formats échangés + chiffrement (clé de groupe, XChaCha20-Poly1305, X25519) |
| `app/src-tauri` | Client : presse-papiers, historique local, raccourci, collage, P2P, boîte aux lettres |
| `app` | Interface (TypeScript, Vite) |
| `server` | Boîte aux lettres optionnelle (Axum + SQLite) |

## Fonctionnement

**Groupe d'appareils.** Pas de compte. Le premier ordinateur crée un groupe (clé aléatoire). Pour en ajouter
un autre : *Paramètres › Ajouter un appareil* affiche un code `MP1-…` ; sur le nouvel ordinateur,
*Rejoindre un autre ordinateur* puis coller le code. Le code est à usage unique, valable 15 minutes, et
l'ordinateur qui l'a créé doit être allumé. Les historiques des deux ordinateurs sont fusionnés.

**P2P (iroh).** Chaque ordinateur est identifié par une clé Ed25519 et joint les autres membres par cet
identifiant : connexion directe (perçage NAT) quand c'est possible, sinon via les relais publics de n0.
Les connexions QUIC sont chiffrées et authentifiées ; seuls les membres du groupe sont acceptés.
À la connexion, chaque appareil envoie la liste de ses éléments et reçoit ce qui lui manque ; ensuite chaque
copie est transmise aussitôt. Sans boîte aux lettres, deux ordinateurs ne se synchronisent que lorsqu'ils
sont allumés en même temps.

**Boîte aux lettres (optionnelle).** Un serveur qui garde les éléments chiffrés pour les appareils éteints.
Elle ne connaît ni comptes ni contenu : l'accès se fait avec un secret dérivé de la clé du groupe.
L'adresse se règle sur un appareil et s'applique à tout le groupe.

**Retirer un appareil.** Une nouvelle clé de groupe est créée et transmise aux appareils restants : en P2P,
et via l'ancienne boîte aux lettres, chiffrée pour chacun (X25519). L'appareil retiré est prévenu,
déconnecté, et ne peut plus lire les nouveaux éléments.

**Utilisation.** `Ctrl+Alt+V` ouvre le panneau. Flèches pour choisir, `Entrée` colle dans l'application
précédente, `Suppr` efface, `Échap` ferme. Quand le dernier élément vient d'un autre appareil, il est placé
dans le presse-papiers : `Ctrl+V` le colle directement. Le menu de l'icône (barre des tâches) donne le mode
privé et la pause de la synchronisation.

**Sécurité.**
- Les clés de l'appareil sont dans le Gestionnaire d'identification Windows ; l'état du groupe (qui contient
  la clé du groupe) est chiffré dans la base locale avec une clé de ce coffre.
- Relais iroh et boîte aux lettres ne voient que du contenu chiffré. Les relais n0 voient en revanche
  quels appareils communiquent et quand.
- Les copies venant des gestionnaires de mots de passe (format `ExcludeClipboardContentFromMonitorProcessing`)
  sont ignorées. Le code d'invitation copié depuis l'application n'entre pas dans l'historique.

## Développement

Prérequis : Rust, Node.js et pnpm (uniquement pour développer ; l'utilisateur final n'installe rien).

```sh
cd app
pnpm install
pnpm tauri dev
```

Tests : `cargo test --workspace`. Le test `app/src-tauri/tests/devices.rs` fait tourner trois appareils et une
boîte aux lettres dans un même processus (appairage, fusion, suppression, hors ligne, boîte aux lettres,
révocation).

Second appareil en ligne de commande, sur le vrai réseau iroh :

```sh
cargo run -p mypress --example peer -- MP1-... "texte à copier"
```

## Installateur Windows

```sh
cd app
pnpm tauri build
```

Produit `target/release/bundle/nsis/MyPress_0.1.0_x64-setup.exe`. Installation sans droits administrateur ;
WebView2 n'est téléchargé que s'il manque (il est présent sur Windows 10/11 à jour). Pour une installation
totalement hors ligne, passer `webviewInstallMode` à `offlineInstaller` dans `tauri.conf.json`.

## Boîte aux lettres : déploiement

```sh
docker build -f server/Dockerfile -t mypress-server .
docker run -d -p 8787:8787 -v mypress-data:/data mypress-server
```

Placer un proxy HTTPS devant (Caddy, Traefik, nginx). Variables : `MYPRESS_BIND`, `MYPRESS_DB`, `RUST_LOG`.
Elle conserve au plus 1000 éléments par groupe et les marqueurs de suppression 60 jours.

## Limites connues

- Texte uniquement (images, texte enrichi et fichiers : versions suivantes).
- Dépendance aux services publics de n0 (DNS de découverte, relais) ; on peut héberger les siens plus tard.
- Tout membre du groupe peut ajouter ou retirer des appareils. Un appareil retiré pourrait encore tromper un
  membre qui n'a pas encore appris la révocation, ou effacer l'ancienne boîte aux lettres (déni de service).
- Si deux appareils modifient les paramètres en même temps, une seule modification est gardée.
- Raccourci `Ctrl+Alt+V` fixe ; collage automatique sous Windows seulement ; refusé par les applications
  lancées en administrateur (protection UIPI).
- L'ordre de l'historique se base sur l'horloge de chaque appareil.
- Installateur non signé (avertissement SmartScreen), pas de mise à jour automatique.
