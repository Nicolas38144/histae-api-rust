# Compiler OpenSSL pour Histae sous Windows

## À quoi sert OpenSSL ici ?

OpenSSL est une bibliothèque native de cryptographie et de TLS. Dans Histae, elle est utilisée indirectement par `webauthn-rs-core` pour vérifier les clés COSE et les signatures des cérémonies WebAuthn administrateur.

Le projet active la feature Cargo `vendored` du crate `openssl`. Cargo télécharge donc les sources d’OpenSSL et les compile avec le projet : il n’est pas nécessaire d’installer séparément une DLL ou un SDK OpenSSL. Cette compilation native exige toutefois :

- un Perl Windows natif ;
- les Microsoft C++ Build Tools et le Windows SDK ;
- `nmake`, fourni par les Build Tools.

Le Perl livré avec Git for Windows est un Perl MSYS/Cygwin. Il produit des chemins Unix et ne peut pas configurer OpenSSL pour la cible Rust `x86_64-pc-windows-msvc`. Il faut utiliser Strawberry Perl et vérifier que `osname` vaut `MSWin32`.

## 1. Installer Strawberry Perl x64

Depuis PowerShell :

```powershell
winget install --exact --id StrawberryPerl.StrawberryPerl
```

Une installation graphique est également disponible sur <https://strawberryperl.com/>. Utiliser la version MSI 64 bits, puis fermer et rouvrir PowerShell pour recharger `PATH`.

Vérifier l’installation :

```powershell
where.exe perl
perl -V:osname
```

Le premier chemin doit normalement être `C:\Strawberry\perl\bin\perl.exe` et la seconde commande doit afficher :

```text
osname='MSWin32';
```

Si `C:\Program Files\Git\usr\bin\perl.exe` apparaît en premier, corriger la session courante :

```powershell
$env:Path = "C:\Strawberry\perl\bin;C:\Strawberry\c\bin;$env:Path"
where.exe perl
perl -V:osname
```

## 2. Installer les outils C++ Microsoft

Ouvrir **Visual Studio Installer**, modifier l’installation existante ou installer **Build Tools for Visual Studio**, puis sélectionner la charge de travail **Desktop development with C++**. Vérifier que les composants suivants sont inclus :

- MSVC Build Tools x64/x86 ;
- Windows 10 ou Windows 11 SDK ;
- MSBuild.

Après l’installation, ouvrir **x64 Native Tools Command Prompt for VS** ou **Developer PowerShell for VS** et vérifier :

```powershell
where.exe cl
where.exe nmake
```

Les deux commandes doivent retourner un chemin.

## 3. Compiler et tester S10

Dans le dépôt Rust :

```powershell
Set-Location C:\Users\nicol\Nicolas_Germani\Programmation\Histae\histae-api-rust
$env:CARGO_BUILD_JOBS = "1"
cargo test --locked --features webauthn-probe
cargo clippy --locked --all-targets --features webauthn-probe,postgres-integration -- -D warnings
```

La compilation statique d'OpenSSL consomme beaucoup de mémoire sous Windows. Limiter Cargo à un job évite
l'erreur système 1455 (`Le fichier de pagination est insuffisant`) observée lorsque plusieurs éditions de liens
sont lancées en parallèle. Cette limite ralentit la première compilation, mais les artefacts suivants sont mis en
cache par Cargo.

Si Cargo réutilise l’échec précédent, supprimer seulement les artefacts du crate natif puis relancer :

```powershell
cargo clean -p openssl-sys
cargo test --locked --features webauthn-probe
```

## Diagnostic rapide

- `perl is not recognized` : Strawberry Perl n’est pas dans `PATH` ou le terminal n’a pas été rouvert.
- `This perl implementation doesn't produce Windows like paths` : le Perl de Git/MSYS est encore sélectionné ; `perl -V:osname` doit afficher `MSWin32`.
- `nmake is not recognized` ou `cl is not recognized` : lancer la commande depuis un terminal développeur Visual Studio ou installer la charge de travail C++.
- `os error 1455` ou `Le fichier de pagination est insuffisant` : définir `$env:CARGO_BUILD_JOBS = "1"` puis relancer la commande.
- `LNK4099: PDB 'ossl_static.pdb' n'a pu être trouvé` : avertissement de symboles de débogage du build vendored ; la bibliothèque reste correctement liée et utilisable.
- erreur de lien après une mise à jour d’outils : exécuter `cargo clean -p openssl-sys`, puis relancer le test avec la même cible MSVC.

Ne définir ni `OPENSSL_DIR` ni `OPENSSL_LIB_DIR` pour ce projet tant que la feature `vendored` reste activée : ces variables servent à pointer vers une installation OpenSSL système et changeraient le mode de compilation attendu.
