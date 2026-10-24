//! Interface des applis : proposer une entrée pour la roue, la retirer, lister.
//!
//!     odysseus-roue proposer --id ID --nom NOM --commande CMD [--icone ICÔNE] [--description TEXTE]
//!     odysseus-roue retirer ID
//!     odysseus-roue liste
//!
//! Une proposition attend que l'utilisateur la place dans Réglages > Roue ;
//! une notification l'en avertit. Une appli peut aussi déposer elle-même un
//! fichier .desktop dans `~/.local/share/odysseus/roue` (ou
//! `/usr/share/odysseus/roue` à l'installation) : c'est le même format.

use common::roue;

const USAGE: &str = "usage :
  odysseus-roue                         ouvrir la roue au pointeur (Super+R)
  odysseus-roue proposer --id ID --nom NOM --commande CMD [--icone ICÔNE] [--description TEXTE]
  odysseus-roue retirer ID              retirer une proposition
  odysseus-roue liste                   contenu de la roue et propositions";

pub fn run(args: &[String]) -> i32 {
    let result = match args[0].as_str() {
        "proposer" => propose(&args[1..]),
        "retirer" => match args.get(1) {
            Some(id) => roue::withdraw(id).map(|_| println!("proposition « {id} » retirée")),
            None => Err(USAGE.into()),
        },
        "liste" => {
            list();
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("odysseus-roue : {e}");
            1
        }
    }
}

fn propose(args: &[String]) -> Result<(), String> {
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let (id, name, command) = (get("--id"), get("--nom"), get("--commande"));
    if id.is_empty() || name.is_empty() || command.is_empty() {
        return Err(USAGE.into());
    }
    let path = roue::propose(&id, &name, &command, &get("--icone"), &get("--description"))?;
    println!("{}", path.display());
    // L'utilisateur décide : une notification l'invite à la placer
    let notify = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("odysseus-notify")));
    if let Some(exe) = notify {
        let _ = std::process::Command::new(exe)
            .args([
                "--send",
                "Roue",
                &format!("« {name} » est proposé pour la roue. Réglages > Roue pour l'y placer."),
            ])
            .spawn();
    }
    Ok(())
}

fn list() {
    let apps = common::apps::load();
    let catalog = roue::catalog(&apps);
    let wheel = roue::load(&apps);
    println!("Roue (depuis le haut, sens des aiguilles d'une montre) :");
    for (i, place) in wheel.iter().enumerate() {
        let label = match place {
            None => "—".to_string(),
            Some(slot) => catalog
                .iter()
                .find(|it| &it.slot == slot)
                .map(|it| format!("{} ({})", it.name, slot.to_text()))
                .unwrap_or_else(|| format!("{} (introuvable)", slot.to_text())),
        };
        println!("  {}. {label}", i + 1);
    }
    let proposals = roue::proposals();
    if !proposals.is_empty() {
        println!("Propositions des applis :");
        for (id, p) in proposals {
            println!("  {id}\t{}\t{}", p.name, p.exec);
        }
    }
}
