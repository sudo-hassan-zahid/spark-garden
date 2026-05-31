use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::path::Path;

use redb::{Database, ReadableTable, TableDefinition, TableError};

use crate::Entry;

const ENTRIES: TableDefinition<&str, &str> = TableDefinition::new("entries");

type DbResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn load_entries(path: &Path) -> DbResult<Vec<Entry>> {
    let db = Database::create(path)?;
    let read_txn = db.begin_read()?;
    let table = match read_txn.open_table(ENTRIES) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(err) => return Err(Box::new(err)),
    };

    let mut entries = Vec::new();
    for row in table.iter()? {
        let (_, value) = row?;
        if let Some(entry) = decode_entry(value.value()) {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| entry.ts);
    Ok(entries)
}

pub fn append_entry(path: &Path, entry: &Entry) -> DbResult<()> {
    let db = Database::create(path)?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(ENTRIES)?;
        let key = entry_key(entry);
        let value = encode_entry(entry);
        table.insert(key.as_str(), value.as_str())?;
    }
    write_txn.commit()?;
    Ok(())
}

fn entry_key(entry: &Entry) -> String {
    format!("{:020}-{:016x}", entry.ts, hash_entry(entry))
}

fn hash_entry(entry: &Entry) -> u64 {
    let mut hasher = DefaultHasher::new();
    entry.kind.hash(&mut hasher);
    entry.mood.hash(&mut hasher);
    entry.color.hash(&mut hasher);
    entry.text.hash(&mut hasher);
    entry.ts.hash(&mut hasher);
    hasher.finish()
}

fn encode_entry(entry: &Entry) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        entry.ts,
        escape_field(&entry.kind),
        escape_field(&entry.mood),
        escape_field(&entry.color),
        escape_field(&entry.text)
    )
}

fn decode_entry(line: &str) -> Option<Entry> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 5 {
        return None;
    }

    Some(Entry {
        ts: parts[0].parse().ok()?,
        kind: unescape_field(parts[1]),
        mood: unescape_field(parts[2]),
        color: unescape_field(parts[3]),
        text: unescape_field(parts[4]),
    })
}

fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn unescape_field(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}
