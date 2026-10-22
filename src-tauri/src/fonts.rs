//! Installed font families, for the font pickers in Appearance → Fonts.
use crate::error::{AppError, AppResult};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FontFamily {
    pub name: String,
    /// At least one face of the family is fixed-width.
    pub mono: bool,
}

/// Every font family installed on this computer, sorted by name.
#[cfg_attr(feature = "gui", tauri::command)]
pub async fn fonts_list() -> AppResult<Vec<FontFamily>> {
    #[cfg(feature = "gui")]
    return crate::rt::spawn_blocking(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        families(db.faces().filter_map(|f| f.families.first().map(|(name, _)| (name.as_str(), f.monospaced))))
    })
    .await
    .map_err(|e| AppError::msg(e.to_string()));
    #[cfg(not(feature = "gui"))]
    Err(AppError::msg("Listing fonts needs the DevCat app"))
}

/// One entry per family (faces are per weight / style), hidden system families (".SF NS") left out.
#[cfg(any(test, feature = "gui"))]
fn families<'a>(faces: impl Iterator<Item = (&'a str, bool)>) -> Vec<FontFamily> {
    let mut out: std::collections::BTreeMap<String, FontFamily> = Default::default();
    for (name, mono) in faces {
        let name = name.trim();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        out.entry(name.to_lowercase())
            .and_modify(|f| f.mono |= mono)
            .or_insert_with(|| FontFamily { name: name.to_string(), mono });
    }
    out.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_entry_per_family() {
        let faces = [("Inter", false), ("JetBrains Mono", true), ("JetBrains Mono", false), (".SF NS", false), ("inter", false), (" ", true)];
        assert_eq!(
            families(faces.into_iter()),
            vec![FontFamily { name: "Inter".into(), mono: false }, FontFamily { name: "JetBrains Mono".into(), mono: true }]
        );
    }
}
