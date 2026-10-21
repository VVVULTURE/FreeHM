//! Installed-font discovery for the settings window (the background app only ever loads
//! the single file path stored in the configuration).

use freehm_core::config::FontWeight;

pub struct SystemFonts {
    db: fontdb::Database,
    /// Sorted, de-duplicated family names of fonts with a regular (upright) face.
    pub families: Vec<String>,
}

fn weight_value(w: FontWeight) -> u16 {
    match w {
        FontWeight::Regular => 400,
        FontWeight::Medium => 500,
        FontWeight::SemiBold => 600,
        FontWeight::Bold => 700,
    }
}

impl SystemFonts {
    /// Scan the system font directories (a few hundred milliseconds; run off the UI thread).
    pub fn scan() -> SystemFonts {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut families: Vec<String> = db
            .faces()
            .filter(|f| f.style == fontdb::Style::Normal && matches!(f.source, fontdb::Source::File(_)))
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        families.sort_by_key(|s| s.to_lowercase());
        families.dedup();
        SystemFonts { db, families }
    }

    /// File for `family` closest to `weight`, as `path` or `path#index` for collections.
    pub fn resolve(&self, family: &str, weight: FontWeight) -> Option<String> {
        let q = fontdb::Query { families: &[fontdb::Family::Name(family)], weight: fontdb::Weight(weight_value(weight)), ..Default::default() };
        let id = self.db.query(&q)?;
        let face = self.db.face(id)?;
        match &face.source {
            fontdb::Source::File(p) => {
                let p = p.display().to_string();
                Some(if face.index == 0 { p } else { format!("{p}#{}", face.index) })
            }
            _ => None,
        }
    }
}
