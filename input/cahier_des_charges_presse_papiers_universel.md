# Cahier des charges — Presse-papiers universel multiplateforme

**Version :** 1.0 — Document initial  
**Date :** 21 octobre 2026  
**Statut :** Base de travail pour l'étude technique et le développement  
**Nom du projet :** À définir

## 1. Présentation du projet

### 1.1 Contexte

L'utilisation simultanée de plusieurs ordinateurs rend le copier-coller peu pratique. Un élément copié sur un ordinateur n'est pas systématiquement accessible sur les autres, et les historiques de presse-papiers restent souvent limités à une seule machine.

Le projet vise à développer une application permettant à un utilisateur de retrouver et d'utiliser son presse-papiers sur tous ses ordinateurs, indépendamment du système d'exploitation et du réseau utilisé.

### 1.2 Objectif principal

Développer un **presse-papiers personnel universel, synchronisé, sécurisé et multiplateforme**, associé à un compte utilisateur unique.

L'application doit permettre de :
- Copier un élément sur un ordinateur et le retrouver sur un autre.
- Consulter un historique commun de tous les éléments copiés.
- Choisir le nombre d'éléments conservés.
- Coller rapidement un élément récent ou ancien grâce à des raccourcis clavier.
- Utiliser l'application sans installer manuellement Java, Python ou un autre environnement d'exécution.
- Utiliser plusieurs ordinateurs, y compris lorsqu'ils sont situés sur des réseaux différents.

L'application sera développée en priorité pour Windows, tout en prévoyant dès le départ une architecture compatible avec macOS et Linux.

### 1.3 Public cible

La première version s'adresse à un utilisateur individuel qui possède plusieurs ordinateurs.

La gestion des équipes, des comptes partagés et des espaces collaboratifs n'est pas prévue dans le périmètre initial.

## 2. Périmètre fonctionnel

### 2.1 Gestion du compte utilisateur

Chaque utilisateur dispose d'un compte personnel permettant d'associer plusieurs appareils.

Fonctionnalités requises :
- Création et connexion à un compte.
- Association de plusieurs ordinateurs au même compte.
- Identification de chaque appareil.
- Consultation des appareils connectés.
- Possibilité de déconnecter ou de révoquer l'accès d'un appareil.
- Synchronisation automatique des paramètres compatibles entre appareils.

L'architecture devra permettre d'ajouter ultérieurement d'autres méthodes d'authentification.

### 2.2 Synchronisation du presse-papiers

La synchronisation constitue la fonctionnalité centrale de l'application.

Lorsqu'un utilisateur copie un élément sur un ordinateur connecté, l'application doit détecter cet élément et le rendre disponible sur ses autres appareils.

**Exemple d'utilisation :**
1. L'utilisateur copie une adresse URL sur son PC fixe Windows.
2. L'application détecte le nouvel élément.
3. L'élément est transmis de manière sécurisée au système de synchronisation.
4. Son historique est mis à jour.
5. L'utilisateur passe sur son ordinateur portable.
6. Il peut coller le dernier élément synchronisé ou retrouver un élément plus ancien.

La synchronisation doit fonctionner entre appareils situés sur des réseaux différents, sans exiger qu'ils soient connectés au même réseau local.

Elle doit être automatique et ne pas nécessiter de manipulation supplémentaire après la configuration initiale.

#### Comportement en cas de déconnexion

Si un ordinateur est temporairement hors ligne :
- L'application continue de fonctionner localement.
- Les nouveaux éléments sont enregistrés dans son historique local.
- La synchronisation reprend automatiquement au retour de la connexion.
- Les doublons et les conflits doivent être traités de manière prévisible.

Le comportement exact en cas de copies simultanées sur plusieurs appareils devra être défini pendant la conception technique.

### 2.3 Historique universel

L'application doit proposer un historique partagé regroupant les éléments copiés sur les appareils associés au compte.

#### Paramètres de conservation

L'utilisateur doit pouvoir choisir la taille de son historique, avec les valeurs suivantes proposées :
- 50 éléments.
- 100 éléments.
- 250 éléments.
- 500 éléments.
- Historique plus important ou illimité, si les contraintes de stockage le permettent.

La valeur par défaut recommandée pour la première version est de **100 éléments**.

L'historique doit présenter, dans la mesure du possible :
- Le contenu ou un aperçu de l'élément.
- Sa date et son heure de copie.
- Le type de contenu.
- L'appareil d'origine.
- Un aperçu des images et des autres contenus pris en charge.

Les éléments doivent être classés du plus récent au plus ancien.

#### Recherche

Une recherche textuelle doit permettre de retrouver rapidement un élément dans l'historique.

Exemples :
- Retrouver une URL copiée la veille.
- Rechercher un extrait de code.
- Retrouver un texte contenant un mot précis.

La recherche avancée par date, type de contenu ou appareil pourra être ajoutée dans une version ultérieure.

#### Gestion des éléments

L'utilisateur doit pouvoir :
- Sélectionner un élément et le coller.
- Copier de nouveau un élément ancien.
- Supprimer un élément.
- Vider l'historique.
- Épingler des éléments importants, fonctionnalité souhaitable pour une version ultérieure.

La suppression d'un élément dans l'historique partagé devra être répercutée sur les appareils connectés. Le traitement des copies locales déjà présentes dans le presse-papiers système devra être spécifié séparément.

## 3. Raccourcis clavier

La rapidité d'utilisation est une exigence majeure.

L'application doit fonctionner sans obliger l'utilisateur à ouvrir sa fenêtre principale à chaque copier-coller.

### 3.1 Raccourcis principaux

| Raccourci | Comportement attendu | Priorité |
|---|---|---|
| `Ctrl + C` | Copier normalement dans l'application active | P0 |
| `Ctrl + V` | Coller le dernier élément du presse-papiers système, actualisé par la synchronisation | P0 |
| `Ctrl + Alt + V` | Ouvrir l'historique partagé | P0 |
| `Échap` | Fermer le panneau d'historique | P0 |
| `Entrée` | Sélectionner l'élément et tenter de le coller dans l'application précédente | P0 |
| Raccourci configurable | Ouvrir ou fermer l'historique | P1 |

**P0 : indispensable pour la première version.**  
**P1 : important, mais pouvant être livré après le premier prototype.**

Les raccourcis devront être personnalisables afin d'éviter les conflits avec d'autres logiciels.

### 3.2 Fonctionnement du panneau d'historique

Le panneau doit être léger, rapide à afficher et accessible depuis n'importe quelle application compatible.

Il doit permettre de :
- Visualiser les derniers éléments copiés.
- Rechercher un élément.
- Sélectionner un élément.
- Le coller dans l'application précédemment utilisée.
- Fermer le panneau sans perturber le travail en cours.

Le panneau ne doit pas obliger l'utilisateur à changer manuellement de fenêtre pour effectuer un collage.

Le comportement du collage automatique devra être testé dans les principaux logiciels Windows.

### 3.3 Particularités multiplateformes

Les raccourcis ne sont pas identiques sur tous les systèmes.

L'application devra donc adapter ses raccourcis :
- Windows : `Ctrl + C` et `Ctrl + V`.
- macOS : `Cmd + C` et `Cmd + V`.
- Linux : raccourcis conventionnels compatibles avec l'environnement de bureau utilisé.

Le raccourci d'ouverture de l'historique devra être configurable sur chaque plateforme.

## 4. Types de contenus pris en charge

### 4.1 Première version

| Type | Exigence | Priorité |
|---|---|---|
| Texte brut | Copier, synchroniser et coller | P0 |
| Adresses URL | Synchroniser et coller correctement | P0 |
| Texte multiligne | Préserver les retours à la ligne | P0 |
| Extraits de code | Préserver le texte et les caractères spéciaux | P0 |
| Images | Synchroniser et coller dans les applications compatibles | P1 |
| Texte enrichi | Préserver autant que possible le formatage | P1 |
| Fichiers | Étudier la synchronisation du contenu et des références de fichiers | P2 |

La première version devra être fiable sur les contenus textuels avant d'étendre la prise en charge aux autres formats.

### 4.2 Particularités des fichiers

Le copier-coller d'un fichier ne fonctionne pas toujours comme celui d'un texte. Selon le système, le presse-papiers peut contenir une référence vers un fichier local plutôt que son contenu.

Il faudra donc décider si l'application doit :
- Synchroniser uniquement les informations de référence.
- Transférer également le fichier.
- Ou proposer une règle configurable en fonction de sa taille.

Pour le premier prototype, la synchronisation du texte sera prioritaire.

## 5. Confidentialité et sécurité

Le presse-papiers peut contenir des informations sensibles. La sécurité doit donc être intégrée dès la conception.

### 5.1 Chiffrement

Les échanges entre les appareils et le service de synchronisation doivent être chiffrés.

Le projet devra étudier un chiffrement de bout en bout permettant de protéger le contenu du presse-papiers contre un accès non autorisé, y compris au niveau du serveur.

Les clés de chiffrement devront être gérées de façon sécurisée.

### 5.2 Authentification et appareils

Le système devra :
- Authentifier les connexions.
- Limiter l'accès aux appareils associés au compte.
- Permettre de révoquer un appareil.
- Protéger les jetons d'authentification.
- Éviter de transmettre des données sensibles dans les journaux techniques.

### 5.3 Exclusion des données sensibles

Une fonctionnalité permettant d'exclure certains contenus est souhaitable. L'application pourra notamment étudier :
- L'exclusion des gestionnaires de mots de passe.
- L'exclusion des champs de saisie identifiés comme sensibles.
- Une durée de conservation limitée pour certains éléments.
- La possibilité de suspendre temporairement la synchronisation.
- Un mode privé qui n'enregistre pas les éléments copiés.

La détection automatique de contenus sensibles devra être évaluée : elle ne peut pas garantir qu'aucune information confidentielle ne sera enregistrée.

### 5.4 Suppression des données

L'utilisateur doit pouvoir supprimer son historique local et partagé.

Une suppression définitive dans le stockage distant devra être prévue, avec une politique de gestion des sauvegardes et des copies temporaires clairement définie.

## 6. Exigences techniques

### 6.1 Application autonome

Il s'agit d'une exigence ferme du projet.

**L'utilisateur ne doit pas avoir à installer manuellement Java, Python, Node.js ou un autre environnement de développement pour utiliser l'application.**

Les composants nécessaires au fonctionnement devront être embarqués dans l'application ou gérés automatiquement par son installateur.

Les exigences sont les suivantes :
- Fournir un installateur Windows simple.
- Installer les composants requis sans intervention technique de l'utilisateur.
- Éviter les dépendances externes non documentées.
- Ne pas exiger l'installation préalable d'un environnement de développement.
- Prévoir des paquets adaptés à macOS et Linux.
- Fournir un mécanisme de mise à jour sécurisé.

Il faut toutefois distinguer l'absence de dépendances à installer manuellement de l'absence absolue de dépendances système : certains composants peuvent être fournis par le système d'exploitation.

### 6.2 Technologies à évaluer

Deux approches méritent d'être comparées avant de fixer le choix technique.

**Option A — Tauri avec Rust**

Avantages :
- Cœur applicatif compilé en Rust.
- Applications Windows, macOS et Linux.
- Possibilité de produire des installateurs adaptés à chaque système.
- Potentiel de réduire la taille de l'application.

Point d'attention : sous Windows, Tauri utilise WebView2. L'installateur peut gérer son installation si nécessaire, ou embarquer son runtime au prix d'une taille nettement supérieure. Il faut donc vérifier la configuration exacte pour respecter l'exigence d'autonomie.

**Option B — Flutter**

Avantages :
- Compilation pour Windows, macOS et Linux.
- Base de code commune pour les interfaces.
- Accès aux fonctions natives via des plugins ou du code spécifique à chaque plateforme.

Point d'attention : il faudra mesurer la taille des paquets, vérifier les bibliothèques de presse-papiers et de raccourcis globaux et confirmer la compatibilité avec les exigences de distribution.

### 6.3 Recommandation provisoire

Je recommande de réaliser un petit prototype comparatif avant de choisir définitivement.

Le prototype devra vérifier :
1. La détection fiable du presse-papiers.
2. Les raccourcis clavier globaux.
3. Le collage dans une autre application.
4. La compilation et l'installation sans dépendance manuelle.
5. La facilité de portage sur macOS et Linux.

Le choix final devra privilégier la fiabilité du presse-papiers, la maîtrise des dépendances et la facilité de maintenance plutôt que la seule taille de l'installateur.

## 7. Architecture de synchronisation

L'application sera composée de trois éléments principaux.

```text
┌──────────────────────────┐
│      PC Windows A        │
│ Presse-papiers + Historique
└────────────┬─────────────┘
             │ Synchronisation chiffrée
             ▼
┌──────────────────────────┐
│ Service de synchronisation
│ Compte + Métadonnées
│ Historique chiffré
└────────────┬─────────────┘
             │ Synchronisation chiffrée
             ▼
┌──────────────────────────┐
│      PC Windows B        │
│ Presse-papiers + Historique
└──────────────────────────┘

             ...
             │
┌──────────────────────────┐
│       Mac / Linux        │
│ Presse-papiers + Historique
└──────────────────────────┘
```

Ce schéma représente une architecture de référence, et non une architecture déjà validée.

Le service de synchronisation devra gérer :
- Les comptes et les appareils autorisés.
- Les métadonnées nécessaires à la synchronisation.
- La transmission des nouveaux éléments.
- L'historique partagé.
- La révocation des appareils.
- Les conflits et les reconnexions.

Le contenu du presse-papiers devra rester chiffré de bout en bout si cette exigence est retenue dans la conception finale.

Le système devra aussi limiter les transferts inutiles et éviter qu'une synchronisation provoque une boucle de copies entre appareils.

## 8. Performances et fiabilité

Les objectifs suivants sont proposés comme critères initiaux de validation. Ils devront être mesurés et ajustés pendant le développement.

| Indicateur | Objectif initial |
|---|---|
| Ouverture du panneau d'historique | Moins de 200 ms sur une machine de référence |
| Synchronisation d'un petit texte | Généralement moins d'une seconde lorsque le réseau le permet |
| Fonctionnement hors ligne | Oui |
| Reprise après reconnexion | Automatique |
| Doublons causés par la synchronisation | Évités |
| Consommation au repos | Faible et mesurée |
| Stabilité | Aucun plantage lors des tests de charge prévus |

Les délais de synchronisation dépendent du réseau et ne peuvent pas être garantis en toutes circonstances.

L'application devra également :
- Fonctionner en arrière-plan.
- Démarrer automatiquement si l'utilisateur l'active.
- Ne pas interrompre les applications en cours.
- Gérer les coupures réseau.
- Préserver l'historique local en cas d'indisponibilité du service distant.

## 9. Interface utilisateur

L'interface doit être simple et discrète.

### 9.1 Fenêtre principale

Elle devra proposer :
- Un historique des éléments copiés.
- Une barre de recherche.
- Un accès aux paramètres.
- Un état de synchronisation.
- La liste des appareils connectés.
- Des commandes pour supprimer ou épingler des éléments.

### 9.2 Menu système

L'application devra pouvoir rester active en arrière-plan.

Son menu système permettra notamment :
- D'ouvrir l'historique.
- De consulter l'état de la synchronisation.
- De suspendre temporairement la synchronisation.
- D'accéder aux paramètres.
- De quitter l'application.

### 9.3 Expérience d'installation

L'installation devra être aussi simple que possible :
1. Télécharger l'installateur officiel.
2. Installer l'application.
3. Se connecter au compte.
4. Associer l'appareil.
5. Utiliser immédiatement les raccourcis.

Les autorisations nécessaires devront être expliquées clairement, en particulier sur macOS et Linux lorsque l'accès au presse-papiers ou les raccourcis globaux dépendent de permissions ou de l'environnement de bureau.

## 10. Périmètre des versions

### Version 1 — MVP Windows

Le produit minimum viable devra inclure :
- Installation autonome.
- Création et connexion à un compte.
- Association de plusieurs appareils Windows.
- Synchronisation du texte.
- Historique partagé configurable.
- Raccourci d'ouverture de l'historique.
- Recherche simple.
- Collage d'un élément sélectionné.
- Fonctionnement hors ligne.
- Synchronisation sécurisée.
- Suppression des éléments.
- Tests de fiabilité et de performance.

L'objectif est d'obtenir une version réellement utilisable avant d'ajouter des fonctionnalités secondaires.

### Version 2 — Multiplateforme

Fonctionnalités prévues :
- Compatibilité macOS.
- Compatibilité Linux.
- Synchronisation entre Windows, macOS et Linux.
- Adaptation des raccourcis à chaque système.
- Tests du collage dans les principales applications.
- Amélioration de la gestion des images et du texte enrichi.

### Version 3 — Améliorations

Fonctionnalités possibles :
- Historique étendu.
- Épinglage et favoris.
- Recherche avancée.
- Gestion plus fine des contenus sensibles.
- Synchronisation optimisée des fichiers.
- Paramètres avancés de conservation.
- Importation et exportation de l'historique.

Les versions 2 et 3 ne devront pas retarder la validation du fonctionnement central du produit.

## 11. Tests et critères d'acceptation

### Test A — Synchronisation entre deux PC
1. Installer l'application sur deux PC Windows.
2. Connecter les deux appareils au même compte.
3. Copier un texte sur le premier.
4. Vérifier sa présence dans l'historique du second.
5. Coller le texte dans une application différente.

**Résultat attendu :** le texte est disponible et peut être collé sans intervention manuelle supplémentaire.

### Test B — Historique partagé
1. Copier plusieurs éléments sur les deux PC.
2. Ouvrir l'historique sur chacun.
3. Vérifier que les éléments synchronisés apparaissent dans un ordre cohérent.
4. Rechercher un élément ancien.

**Résultat attendu :** les appareils présentent un historique partagé cohérent et consultable.

### Test C — Raccourci clavier
1. Ouvrir un éditeur de texte.
2. Utiliser le raccourci de l'historique.
3. Sélectionner un élément ancien.
4. Le coller dans l'éditeur.

**Résultat attendu :** le panneau apparaît rapidement et l'élément sélectionné est collé dans l'application active, dans les limites des possibilités du système.

### Test D — Installation autonome
1. Utiliser une machine de test sans Java ni Python installés.
2. Lancer l'installateur.
3. Installer et démarrer l'application.

**Résultat attendu :** aucune installation manuelle de Java, Python ou d'un environnement de développement n'est nécessaire.

### Test E — Déconnexion réseau
1. Déconnecter un appareil du réseau.
2. Copier plusieurs éléments.
3. Rétablir la connexion.
4. Vérifier la synchronisation.

**Résultat attendu :** l'application conserve les données locales et reprend la synchronisation sans perdre les éléments qui doivent être conservés.

### Test F — Sécurité
1. Vérifier que les communications sont chiffrées.
2. Vérifier qu'un appareil révoqué ne peut plus synchroniser de nouveaux éléments.
3. Vérifier les procédures de suppression.
4. Contrôler que les journaux techniques ne contiennent pas de contenu sensible.

**Résultat attendu :** les contrôles de sécurité définis dans la spécification sont validés avant la diffusion publique.

## 12. Points à décider avant le développement

| Sujet | Proposition initiale | Statut |
|---|---|---|
| Système prioritaire | Windows | Validé |
| Compatibilité future | macOS et Linux | Validé |
| Utilisateurs | Un utilisateur et ses appareils | Validé |
| Installation autonome | Oui, sans dépendance manuelle | Validé |
| Historique par défaut | 100 éléments | À confirmer |
| Taille maximale | 500 éléments, extensible | À confirmer |
| Types de contenus prioritaires | Texte et URL | Proposition |
| Synchronisation distante | Oui, entre réseaux différents | Exigence fonctionnelle |
| Chiffrement de bout en bout | À privilégier | À confirmer techniquement |
| Service distant | Service hébergé ou autre solution adaptée | À décider |
| Technologie | Prototype comparatif Tauri/Rust et Flutter | À étudier |
| Modèle économique | Usage personnel au départ | À décider |
| Nom du produit | À définir | En attente |

## 13. Risques techniques à anticiper

### Compatibilité des raccourcis
Les raccourcis globaux et le collage automatique ne se comportent pas exactement de la même manière sur Windows, macOS et Linux. Il faudra tester chaque système et éviter de promettre une compatibilité universelle avant d'avoir validé les scénarios.

### Synchronisation du presse-papiers système
Le presse-papiers système est local à chaque machine. Pour rendre `Ctrl + V` cohérent entre appareils, l'application devra synchroniser les éléments et mettre à jour le presse-papiers local. La latence réseau signifie qu'un élément copié très récemment sur un autre appareil peut ne pas être immédiatement disponible.

### Taille des données
Les textes sont généralement peu volumineux, mais les images et les fichiers peuvent rapidement augmenter les besoins de stockage et de transfert. Des limites et des règles de conservation seront nécessaires.

### Sécurité
Une application qui collecte automatiquement le presse-papiers peut enregistrer involontairement des données sensibles. Les exclusions, la suppression et le chiffrement doivent être traités dès le MVP.

### Distribution autonome
Une application peut embarquer son propre environnement d'exécution, mais certains composants du système d'exploitation peuvent rester nécessaires. Il faudra vérifier ce point pour chaque installateur.

## 14. Plan de réalisation proposé

**Phase 1 — Prototype technique**
- Détection du presse-papiers Windows.
- Historique local.
- Raccourci global.
- Collage d'un élément ancien.
- Test de l'installation autonome.

**Phase 2 — Synchronisation**
- Comptes utilisateurs.
- Association des appareils.
- Transmission sécurisée.
- Historique partagé.
- Gestion des déconnexions.

**Phase 3 — MVP Windows**
- Interface finalisée.
- Paramètres de conservation.
- Recherche.
- Gestion des éléments.
- Tests de sécurité et de performance.
- Installateur prêt à distribuer.

**Phase 4 — Portage multiplateforme**
- macOS.
- Linux.
- Tests croisés entre systèmes.
- Adaptation des raccourcis et des permissions.

**Phase 5 — Améliorations**
- Images.
- Texte enrichi.
- Favoris.
- Recherche avancée.
- Optimisation de la consommation et de la synchronisation.

## 15. Conclusion

Le projet est techniquement envisageable, mais plusieurs solutions proposent déjà une partie des fonctionnalités. La valeur du produit devra reposer sur la combinaison de quatre exigences :

1. Une expérience extrêmement simple, avec des raccourcis naturels.
2. Un historique réellement partagé, utilisable sur tous les ordinateurs de l'utilisateur.
3. Une installation autonome, sans environnement de développement à installer manuellement.
4. Une synchronisation fiable et sécurisée, y compris entre des réseaux différents.

La priorité est de valider ces quatre points sur Windows avant d'étendre le produit à macOS et Linux.

La prochaine étape logique est de transformer les exigences en architecture technique et en tâches de développement, en commençant par un prototype Windows qui vérifie le presse-papiers, les raccourcis globaux et l'installation autonome.
