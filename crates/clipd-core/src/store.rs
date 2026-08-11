//! SQLite-backed history. Single writer, owned by the daemon.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clipd_ipc::{Item, Kind};
use rusqlite::{params, Connection, OptionalExtension};

use crate::detect;

/// Payloads at or above this go to a content-addressed file instead of the DB,
/// keeping the database small enough to stay in page cache.
const INLINE_LIMIT: usize = 256 * 1024;
const PREVIEW_CHARS: usize = 200;

/// One clipboard event: every flavor the source app offered, as one unit.
pub struct Captured {
    pub flavors: Vec<(String, Vec<u8>)>,
    pub source_app: Option<String>,
    /// Source advertised `x-kde-passwordManagerHint`.
    pub hinted_secret: bool,
}

impl Captured {
    /// The flavor that decides how this item renders, best first.
    pub fn best(&self) -> Option<&(String, Vec<u8>)> {
        const ORDER: &[&str] = &[
            "image/png", "image/jpeg", "text/uri-list", "text/html", "UTF8_STRING",
            "text/plain;charset=utf-8", "STRING", "TEXT",
        ];
        ORDER
            .iter()
            .find_map(|want| self.flavors.iter().find(|(m, _)| m == want))
            .or_else(|| self.flavors.first())
    }

    /// Longest text flavor, for preview/search/secret-scanning.
    pub fn text(&self) -> Option<String> {
        self.flavors
            .iter()
            .filter(|(m, _)| {
                m.starts_with("text/") || m == "UTF8_STRING" || m == "STRING" || m == "TEXT"
            })
            .filter_map(|(_, d)| String::from_utf8(d.clone()).ok())
            .max_by_key(|s| s.len())
    }
}

pub struct Store {
    conn: Connection,
    blobs: PathBuf,
    /// Hash of the item currently at the head. Phase 0 showed GNOME re-offers
    /// the same bytes when the owning app exits, so identical content arriving
    /// again is a re-offer, not a new copy. See docs/phase-0-findings.md.
    head_hash: Option<[u8; 32]>,
    last_deleted: Option<i64>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let blobs = dir.join("blobs");
        std::fs::create_dir_all(&blobs)?;

        let conn = Connection::open(dir.join("history.db"))?;
        // WAL so a reader never blocks the capture path; NORMAL because losing
        // the last few milliseconds of clipboard history on power loss is fine.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 268435456;
             PRAGMA cache_size = -8000;
             PRAGMA foreign_keys = ON;",
        )?;

        let mut store = Self { conn, blobs, head_hash: None, last_deleted: None };
        store.migrate()?;
        store.head_hash = store.load_head_hash()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS items (
                id           INTEGER PRIMARY KEY,
                kind         TEXT    NOT NULL,
                hash         BLOB    NOT NULL UNIQUE,
                preview      TEXT    NOT NULL,
                byte_size    INTEGER NOT NULL,
                created_at   INTEGER NOT NULL,
                last_used_at INTEGER NOT NULL,
                use_count    INTEGER NOT NULL DEFAULT 1,
                pinned       INTEGER NOT NULL DEFAULT 0,
                favorite     INTEGER NOT NULL DEFAULT 0,
                sensitive    INTEGER NOT NULL DEFAULT 0,
                source_app   TEXT
            );

            -- One row per MIME type, so a single copy keeps all its flavors and
            -- "paste as plain text" stays possible after the fact.
            CREATE TABLE IF NOT EXISTS flavors (
                item_id   INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                mime      TEXT    NOT NULL,
                data      BLOB,
                blob_path TEXT,
                PRIMARY KEY (item_id, mime)
            );

            CREATE INDEX IF NOT EXISTS idx_recent
                ON items(pinned DESC, last_used_at DESC);

            -- trigram so "npm inst" matches "npm install react"; external
            -- content so the text is not stored a second time.
            CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
                preview,
                content='items',
                content_rowid='id',
                tokenize='trigram'
            );
            "#,
        )?;
        Ok(())
    }

    fn load_head_hash(&self) -> Result<Option<[u8; 32]>> {
        let v: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT hash FROM items ORDER BY last_used_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|b| b.try_into().ok()))
    }

    /// Ingest a clipboard event.
    ///
    /// Returns `Ok(None)` when the copy was suppressed — either it is the
    /// content already at the head (a re-offer), or it is a secret we refuse to
    /// store. Otherwise returns the item, new or promoted.
    pub fn insert(&mut self, cap: Captured, store_secrets: bool) -> Result<Option<Item>> {
        let Some((best_mime, best_data)) = cap.best().cloned() else {
            return Ok(None);
        };
        if best_data.is_empty() {
            return Ok(None);
        }

        let hash = hash_of(&cap);

        // Phase 0 finding: byte-identical content arriving again is GNOME's
        // ownership hand-off re-offering the current clipboard, not a new copy.
        // Suppress it entirely — no new row, no use_count bump, no reorder.
        if self.head_hash == Some(hash) {
            return Ok(None);
        }

        let text = cap.text();
        let sensitive = cap.hinted_secret
            || text.as_deref().map(detect::is_sensitive).unwrap_or(false);
        if sensitive && !store_secrets {
            // Still update the head so the re-offer that follows is suppressed
            // too, otherwise the secret round-trips through here twice.
            self.head_hash = Some(hash);
            return Ok(None);
        }

        let kind = detect::kind_of(&best_mime, &best_data);
        let preview = match (&text, kind) {
            (Some(t), _) => detect::make_preview(t, PREVIEW_CHARS),
            (None, Kind::Image) => format!("Image · {}", human_size(best_data.len())),
            (None, _) => format!("{} · {}", best_mime, human_size(best_data.len())),
        };
        let byte_size: i64 = cap.flavors.iter().map(|(_, d)| d.len() as i64).sum();
        let now = now_ms();

        let tx = self.conn.transaction()?;

        // Content-addressed dedupe: re-copying something already in history
        // promotes the existing row rather than adding a duplicate.
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM items WHERE hash = ?1", params![&hash[..]], |r| r.get(0))
            .optional()?;

        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE items SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
                    params![now, id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO items
                       (kind, hash, preview, byte_size, created_at, last_used_at, sensitive, source_app)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
                    params![
                        kind_str(kind),
                        &hash[..],
                        preview,
                        byte_size,
                        now,
                        sensitive as i64,
                        cap.source_app,
                    ],
                )?;
                let id = tx.last_insert_rowid();

                for (mime, data) in &cap.flavors {
                    if data.len() >= INLINE_LIMIT {
                        let name = format!("{}", blake3::hash(data).to_hex());
                        let path = self.blobs.join(&name);
                        if !path.exists() {
                            std::fs::write(&path, data)?;
                        }
                        tx.execute(
                            "INSERT OR REPLACE INTO flavors (item_id, mime, blob_path)
                             VALUES (?1, ?2, ?3)",
                            params![id, mime, name],
                        )?;
                    } else {
                        tx.execute(
                            "INSERT OR REPLACE INTO flavors (item_id, mime, data)
                             VALUES (?1, ?2, ?3)",
                            params![id, mime, data],
                        )?;
                    }
                }

                // Secrets are never indexed — searching must not surface them.
                if !sensitive {
                    tx.execute(
                        "INSERT INTO items_fts (rowid, preview) VALUES (?1, ?2)",
                        params![id, preview],
                    )?;
                }
                id
            }
        };

        tx.commit()?;
        self.head_hash = Some(hash);
        self.get(id)
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let mut stmt = self.conn.prepare_cached(&format!("{SELECT_ITEM} WHERE i.id = ?1"))?;
        let item = stmt.query_row(params![id], row_to_item).optional()?;
        Ok(item)
    }

    /// Newest first, pinned pinned to the top.
    pub fn recent(&self, limit: u32) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "{SELECT_ITEM} ORDER BY i.pinned DESC, i.last_used_at DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit], row_to_item)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Substring search over previews via the trigram index.
    ///
    /// This is the *cold tail* path. The UI filters its in-memory hot list
    /// synchronously first, so a result here arriving a few ms later is merged
    /// in rather than awaited.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Item>> {
        let q = query.trim();
        // trigram needs >= 3 characters; below that fall back to LIKE so short
        // queries still return something rather than nothing.
        if q.is_empty() {
            return self.recent(limit);
        }
        if q.chars().count() < 3 {
            let mut stmt = self.conn.prepare_cached(&format!(
                "{SELECT_ITEM} WHERE i.sensitive = 0 AND i.preview LIKE ?1 ESCAPE '\\'
                 ORDER BY i.pinned DESC, i.last_used_at DESC LIMIT ?2"
            ))?;
            let pattern = format!("%{}%", escape_like(q));
            let rows = stmt.query_map(params![pattern, limit], row_to_item)?;
            return Ok(rows.collect::<Result<Vec<_>, _>>()?);
        }

        let mut stmt = self.conn.prepare_cached(&format!(
            "{SELECT_ITEM}
             JOIN items_fts f ON f.rowid = i.id
             WHERE items_fts MATCH ?1 AND i.sensitive = 0
             ORDER BY i.pinned DESC, i.last_used_at DESC
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![fts_quote(q), limit], row_to_item)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// All flavors for an item, blobs read back from disk.
    pub fn flavors(&self, id: i64) -> Result<Vec<(String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT mime, data, blob_path FROM flavors WHERE item_id = ?1")?;
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<Vec<u8>>>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (mime, data, blob) = row?;
            match (data, blob) {
                (Some(d), _) => out.push((mime, d)),
                (None, Some(name)) => {
                    if let Ok(d) = std::fs::read(self.blobs.join(name)) {
                        out.push((mime, d));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Mark an item as just pasted, so it returns to the head.
    pub fn touch(&mut self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        let h: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT hash FROM items WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?;
        self.head_hash = h.and_then(|b| b.try_into().ok());
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        Ok(())
    }

    pub fn set_favorite(&self, id: i64, favorite: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET favorite = ?1 WHERE id = ?2",
            params![favorite as i64, id],
        )?;
        Ok(())
    }

    /// Soft-delete semantics for the undo toast: the row goes, but we remember
    /// which one so `undo_delete` can be wired to a real restore later.
    pub fn delete(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM items_fts WHERE rowid = ?1", params![id])?;
        tx.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        tx.commit()?;
        self.last_deleted = Some(id);
        self.head_hash = self.load_head_hash()?;
        Ok(())
    }

    /// Everything except pinned items — pins survive "clear all" by design.
    pub fn clear_all(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM items_fts WHERE rowid IN (SELECT id FROM items WHERE pinned = 0)",
            [],
        )?;
        tx.execute("DELETE FROM items WHERE pinned = 0", [])?;
        tx.commit()?;
        self.head_hash = self.load_head_hash()?;
        Ok(())
    }

    /// Drop the oldest unpinned rows beyond `max`, and any blob no row references.
    pub fn prune(&mut self, max: u32) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM items WHERE pinned = 0
                 ORDER BY last_used_at DESC LIMIT -1 OFFSET ?1",
            )?;
            let rows = stmt.query_map(params![max], |r| r.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for id in &ids {
            tx.execute("DELETE FROM items_fts WHERE rowid = ?1", params![id])?;
            tx.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        }
        tx.commit()?;

        self.gc_blobs()?;
        if !ids.is_empty() {
            self.head_hash = self.load_head_hash()?;
        }
        Ok(ids.len())
    }

    /// Delete blob files no longer referenced by any flavor row.
    fn gc_blobs(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT blob_path FROM flavors WHERE blob_path IS NOT NULL")?;
        let live: std::collections::HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        for entry in std::fs::read_dir(&self.blobs)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if !live.contains(name) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?)
    }
}

const SELECT_ITEM: &str = "SELECT i.id, i.kind, i.preview, i.byte_size, i.created_at,
        i.last_used_at, i.use_count, i.pinned, i.favorite, i.sensitive, i.source_app,
        (SELECT GROUP_CONCAT(mime, char(31)) FROM flavors WHERE item_id = i.id)
   FROM items i";

fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<Item> {
    let mimes: Option<String> = r.get(11)?;
    Ok(Item {
        id: r.get(0)?,
        kind: parse_kind(&r.get::<_, String>(1)?),
        preview: r.get(2)?,
        byte_size: r.get(3)?,
        created_at: r.get(4)?,
        last_used_at: r.get(5)?,
        use_count: r.get(6)?,
        pinned: r.get::<_, i64>(7)? != 0,
        favorite: r.get::<_, i64>(8)? != 0,
        sensitive: r.get::<_, i64>(9)? != 0,
        source_app: r.get(10)?,
        mimes: mimes
            .map(|s| s.split('\u{1f}').map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Identity of a copy, for dedupe and head suppression.
///
/// Hashes the *content*, deliberately not the set of MIME types offered. GNOME's
/// ownership hand-off re-offers the identical copy with a different flavor list
/// (`text/plain;charset=utf-8` appears, `TEXT`/`STRING` disappear), so hashing
/// the flavor set makes every copy look new the second time it arrives — which
/// produced two rows per copy until this was fixed.
///
/// Images win over text so that an image with a text/plain filename caption is
/// identified by the pixels, not the caption.
fn hash_of(cap: &Captured) -> [u8; 32] {
    let mut h = blake3::Hasher::new();

    let image = cap
        .flavors
        .iter()
        .find(|(m, _)| m == "image/png")
        .or_else(|| cap.flavors.iter().find(|(m, _)| m.starts_with("image/")));

    if let Some((_, data)) = image {
        h.update(b"image\0");
        h.update(data);
    } else if let Some(text) = cap.text() {
        h.update(b"text\0");
        h.update(text.as_bytes());
    } else if let Some((mime, data)) = cap.best() {
        h.update(mime.as_bytes());
        h.update(&[0]);
        h.update(data);
    }
    *h.finalize().as_bytes()
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Text => "text",
        Kind::Url => "url",
        Kind::Code => "code",
        Kind::Color => "color",
        Kind::Html => "html",
        Kind::Image => "image",
        Kind::Files => "files",
    }
}

fn parse_kind(s: &str) -> Kind {
    match s {
        "url" => Kind::Url,
        "code" => Kind::Code,
        "color" => Kind::Color,
        "html" => Kind::Html,
        "image" => Kind::Image,
        "files" => Kind::Files,
        _ => Kind::Text,
    }
}

/// Wrap in double quotes for FTS5 so punctuation is a literal, not syntax.
fn fts_quote(q: &str) -> String {
    format!("\"{}\"", q.replace('"', "\"\""))
}

fn escape_like(q: &str) -> String {
    q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn human_size(n: usize) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Convenience for callers building a `Captured` from a single text flavor.
pub fn text_capture(text: &str) -> Captured {
    Captured {
        flavors: vec![("UTF8_STRING".into(), text.as_bytes().to_vec())],
        source_app: None,
        hinted_secret: false,
    }
}

#[allow(dead_code)]
fn _unused(_: HashMap<(), ()>) {}
