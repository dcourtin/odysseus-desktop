//! Petits outils de texte partagés.

/// Minuscules sans accents, pour chercher « ete » dans « Été »
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' | 'ì' => 'i',
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ÿ' => 'y',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fold_accents() {
        assert_eq!(super::fold("Été à Noël, ÇA"), "ete a noel, ca");
    }
}
