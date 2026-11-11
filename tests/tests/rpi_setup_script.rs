//! tune-server-rust#7958 — `dist/rpi/setup.sh`, lancé comme la documentation
//! le dit : `curl -sL …/setup.sh | sudo bash`.
//!
//! 1. L'entrée standard est alors le SCRIPT lui-même : le menu du DAC lisait
//!    la ligne suivante du script (« Choix invalide »), et le choix tapé par
//!    l'utilisateur partait au shell (« orden no encontrada »).
//! 2. La compilation visait `--bin oaat`, une cible qui n'existe plus.
//!
//! Le script est exécuté pour de vrai par `bash`, depuis un tube, sans
//! terminal de contrôle (`setsid`). Il s'arrête avant toute opération
//! système (le choix du DAC précède l'étape 1). Ignoré sous root, où un
//! script régressé irait plus loin.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn racine() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("racine du dépôt")
        .to_path_buf()
}

fn script() -> PathBuf {
    racine().join("dist/rpi/setup.sh")
}

fn sous_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(true)
}

fn setsid_disponible() -> bool {
    Command::new("setsid")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// `curl … | bash` : le texte du script sur l'entrée standard de bash, suivi
/// de ce que l'utilisateur tape (« 2 »), sans terminal de contrôle.
fn par_un_tube(tape: &str) -> (i32, String) {
    let mut entree = std::fs::read_to_string(script()).expect("setup.sh lisible");
    entree.push_str(tape);
    let mut enfant = Command::new("setsid")
        .args(["--wait", "bash", "-s"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("setsid bash");
    enfant
        .stdin
        .take()
        .unwrap()
        .write_all(entree.as_bytes())
        .unwrap();
    let sortie = enfant.wait_with_output().unwrap();
    let texte = format!(
        "{}{}",
        String::from_utf8_lossy(&sortie.stdout),
        String::from_utf8_lossy(&sortie.stderr)
    );
    (sortie.status.code().unwrap_or(-1), texte)
}

#[test]
fn par_un_tube_sans_terminal_le_menu_ne_lit_pas_le_script_et_nomme_dac() {
    if sous_root() || !setsid_disponible() {
        eprintln!("ignoré : sous root ou sans setsid");
        return;
    }
    let (code, texte) = par_un_tube("2\n");
    assert_eq!(
        code, 1,
        "le script doit s'arrêter au choix du DAC :\n{texte}"
    );
    assert!(
        !texte.contains("Choix invalide"),
        "le menu a lu la suite du SCRIPT comme réponse :\n{texte}"
    );
    assert!(
        texte.contains("--dac"),
        "sans terminal, le script doit dire de relancer avec --dac :\n{texte}"
    );
    assert!(
        !texte.contains("[1/7]"),
        "le script a dépassé le choix du DAC :\n{texte}"
    );
}

#[test]
fn par_un_tube_un_modele_inconnu_est_refuse_avant_toute_installation() {
    if sous_root() || !setsid_disponible() {
        eprintln!("ignoré : sous root ou sans setsid");
        return;
    }
    let mut entree = String::from("set -- --dac inconnu\n");
    entree.push_str(&std::fs::read_to_string(script()).unwrap());
    let sortie = Command::new("bash")
        .arg("-c")
        .arg(&entree)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let texte = String::from_utf8_lossy(&sortie.stdout);
    assert_eq!(sortie.status.code(), Some(1), "{texte}");
    assert!(texte.contains("inconnu"), "{texte}");
    assert!(!texte.contains("[1/7]"), "{texte}");
}

/// Les `--bin X` du script : chacun doit être une cible `[[bin]]` du
/// workspace (la compilation échouait sur `--bin oaat`).
#[test]
fn les_cibles_compilees_par_le_script_existent_dans_le_workspace() {
    let texte = std::fs::read_to_string(script()).unwrap();
    let mut cibles = std::collections::BTreeSet::new();
    for entree in std::fs::read_dir(racine().join("crates")).unwrap() {
        let manifeste = entree.unwrap().path().join("Cargo.toml");
        let Ok(contenu) = std::fs::read_to_string(&manifeste) else {
            continue;
        };
        let mut dans_bin = false;
        for ligne in contenu.lines().map(str::trim) {
            if ligne.starts_with('[') {
                dans_bin = ligne == "[[bin]]";
            } else if dans_bin && let Some(nom) = ligne.strip_prefix("name = ") {
                cibles.insert(nom.trim_matches('"').to_string());
            }
        }
    }
    let demandees: Vec<&str> = texte
        .split("--bin ")
        .skip(1)
        .filter_map(|reste| reste.split(|c: char| c.is_whitespace() || c == '"').next())
        .collect();
    assert!(!demandees.is_empty(), "le script ne compile plus rien ?");
    for cible in demandees {
        assert!(
            cibles.contains(cible),
            "`--bin {cible}` : cible absente du workspace ({cibles:?})"
        );
        assert!(
            texte.contains(&format!("target/release/{cible}")),
            "le binaire copié n'est pas celui compilé ({cible})"
        );
    }
}
