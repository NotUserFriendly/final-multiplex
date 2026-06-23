use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Surgical read-write handle on scene.toml (ADR-0010).
///
/// Reads are via serde/toml as before. This type owns the toml_edit document
/// for writes so comments, ordering, and alignment are preserved — only the
/// changed value is rewritten.
///
/// Writes are debounced by the caller (see App::Tick). On drop, any pending
/// dirty state is flushed synchronously so a clean exit persists the last edit.
pub struct ConfigPersist {
    path: PathBuf,
    doc: DocumentMut,
    dirty: bool,
}

impl ConfigPersist {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let doc: DocumentMut = text.parse()?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
            dirty: false,
        })
    }

    /// Persist `offset_ms` for the source with the given id.
    /// Finds the [[source]] entry by id and updates (or inserts) the field.
    pub fn set_source_offset(&mut self, source_id: &str, offset_ms: i64) {
        self.set_source_field(source_id, "offset_ms", toml_edit::value(offset_ms));
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Atomically write doc to disk if dirty; clears the dirty flag.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.doc.to_string())?;
        std::fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }

    fn set_source_field(&mut self, source_id: &str, field: &str, val: toml_edit::Item) {
        let sources = match self
            .doc
            .get_mut("source")
            .and_then(|v| v.as_array_of_tables_mut())
        {
            Some(s) => s,
            None => return,
        };
        for entry in sources.iter_mut() {
            if entry.get("id").and_then(|v| v.as_str()) == Some(source_id) {
                entry[field] = val;
                self.dirty = true;
                return;
            }
        }
    }
}

impl Drop for ConfigPersist {
    fn drop(&mut self) {
        if self.dirty {
            let _ = self.flush();
        }
    }
}
