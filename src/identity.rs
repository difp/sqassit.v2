//! Stable anonymous identities, compatible with the legacy Python application.

const ANIMALS: [(&str, &str); 12] = [
    ("🦊", "Лиса"),
    ("🐻", "Медведь"),
    ("🐼", "Панда"),
    ("🦉", "Сова"),
    ("🦦", "Выдра"),
    ("🦔", "Ёж"),
    ("🦡", "Барсук"),
    ("🐧", "Пингвин"),
    ("🐰", "Кролик"),
    ("🐸", "Лягушка"),
    ("🐯", "Тигр"),
    ("🐨", "Коала"),
];

pub fn animal_identity(telegram_id: i64) -> (&'static str, &'static str) {
    ANIMALS[telegram_id.rem_euclid(ANIMALS.len() as i64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_match_python_modulo() {
        assert_eq!(animal_identity(0), ("🦊", "Лиса"));
        assert_eq!(animal_identity(42), ("🦡", "Барсук"));
        assert_eq!(animal_identity(-1), ("🐨", "Коала"));
    }
}
