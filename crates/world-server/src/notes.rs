//! Player annotations: the one layer of this world that is authored, not
//! derived. Notes are anchored to coordinates and keyed by seed only — a
//! generator upgrade may redraw a coastline, but it does not erase what
//! the player wrote about the place.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct Note {
    pub id: i64,
    pub lat: f64,
    pub lng: f64,
    pub title: String,
    pub body: String,
}

pub struct NoteStore {
    db: Mutex<Connection>,
    seed: i64,
}

impl NoteStore {
    pub fn open(path: &Path, seed: u64) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                seed INTEGER NOT NULL,
                lat REAL NOT NULL,
                lng REAL NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS notes_seed ON notes (seed);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            db: Mutex::new(db),
            seed: seed as i64,
        })
    }

    pub fn list(&self) -> Vec<Note> {
        let db = self.db.lock().unwrap();
        let mut stmt = match db.prepare(
            "SELECT id, lat, lng, title, body FROM notes WHERE seed = ?1 ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([self.seed], |row| {
            Ok(Note {
                id: row.get(0)?,
                lat: row.get(1)?,
                lng: row.get(2)?,
                title: row.get(3)?,
                body: row.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn add(&self, lat: f64, lng: f64, title: &str, body: &str) -> Result<Note, String> {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
            return Err("that place is off the map".into());
        }
        let title = title.trim();
        let body = body.trim();
        if title.is_empty() {
            return Err("a mark needs a name".into());
        }
        if title.len() > 120 || body.len() > 4000 {
            return Err("too long for the margin of a map".into());
        }
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO notes (seed, lat, lng, title, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![self.seed, lat, lng, title, body],
        )
        .map_err(|e| e.to_string())?;
        Ok(Note {
            id: db.last_insert_rowid(),
            lat,
            lng,
            title: title.to_string(),
            body: body.to_string(),
        })
    }

    pub fn delete(&self, id: i64) -> bool {
        self.db
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM notes WHERE id = ?1 AND seed = ?2",
                rusqlite::params![id, self.seed],
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (NoteStore, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "fractal-notes-test-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        (NoteStore::open(&path, 42).unwrap(), path)
    }

    #[test]
    fn notes_roundtrip_and_stay_seed_scoped() {
        let (s, path) = store();
        let n = s.add(12.5, -30.25, "The Broken Tower", "Found it at dusk.").unwrap();
        assert_eq!(s.list().len(), 1);

        // Another seed sees nothing; the same seed sees everything.
        let other = NoteStore::open(&path, 1337).unwrap();
        assert!(other.list().is_empty());
        assert!(!other.delete(n.id), "cannot delete across seeds");

        let again = NoteStore::open(&path, 42).unwrap();
        assert_eq!(again.list()[0].title, "The Broken Tower");
        assert!(again.delete(n.id));
        assert!(again.list().is_empty());

        // Validation.
        assert!(s.add(95.0, 0.0, "x", "").is_err(), "off the map");
        assert!(s.add(0.0, 0.0, "  ", "").is_err(), "unnamed");
        assert!(s.add(0.0, 0.0, &"x".repeat(200), "").is_err(), "too long");
        let _ = std::fs::remove_file(&path);
    }
}
