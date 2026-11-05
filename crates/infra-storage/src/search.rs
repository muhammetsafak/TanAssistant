//! Full-text search over transcripts and notes (SQLite FTS5).
//!
//! The FTS tokenizer is `unicode61 remove_diacritics 0`, so `ç ğ ö ş ü` stay distinct letters.
//! That tokenizer does not understand Turkish dotted/dotless `i`: it would lower-case `I` to
//! `i` (so `PLANI` would not match `planı`) and leave `İ` alone. To make "İŞ PLANI" find
//! "iş planı", text is **folded** before it is indexed and before a query is built: `İ`, `I`
//! and `ı` all become `i`, everything else is lower-cased. The folded copy is only used for
//! matching; hits show the original text.

use tan_application::ports::{SearchHit, SearchIndex, SearchSource};
use tan_application::RepositoryError;
use tan_domain::SessionId;

use crate::db::{u64_to_sql, Database, DbError};

/// How many characters of the original text a hit carries.
const EXCERPT_CHARS: usize = 200;

pub(crate) const SOURCE_TRANSCRIPT: &str = "transcript";
pub(crate) const SOURCE_NOTE: &str = "note";

/// Turkish-aware folding used for both indexing and querying.
pub(crate) fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'İ' | 'I' | 'ı' => out.push('i'),
            other => out.extend(other.to_lowercase()),
        }
    }
    out
}

/// Builds an FTS5 `MATCH` expression from free text: every word becomes a quoted prefix term,
/// all of which must match. Returns `None` when the text has no searchable word.
pub(crate) fn build_match_query(text: &str) -> Option<String> {
    let folded = fold(text);
    let terms: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        // Terms are purely alphanumeric, so quoting needs no escaping; the quotes only keep
        // FTS5 from reading words such as AND/OR/NEAR as operators.
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn excerpt(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(EXCERPT_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Removes every indexed document of `source` that belongs to `session_id`.
pub(crate) fn remove_session_docs(
    conn: &rusqlite::Connection,
    session_id: &SessionId,
    source: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM search_docs WHERE session_id = ?1 AND source = ?2",
        rusqlite::params![session_id.as_str(), source],
    )
    .map(|_| ())
}

/// Removes the indexed documents of one source entity (e.g. one note), whichever session they
/// were indexed under: an entity that moved between sessions must not leave a stale entry.
pub(crate) fn remove_ref_docs(
    conn: &rusqlite::Connection,
    source: &str,
    ref_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM search_docs WHERE source = ?1 AND ref_id = ?2",
        rusqlite::params![source, ref_id],
    )
    .map(|_| ())
}

/// Indexes one text. Blank text is skipped.
pub(crate) fn add_doc(
    conn: &rusqlite::Connection,
    session_id: &SessionId,
    source: &str,
    ref_id: &str,
    start_ms: Option<u64>,
    text: &str,
) -> rusqlite::Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let start = start_ms.map(u64_to_sql).transpose()?;
    conn.execute(
        "INSERT INTO search_docs (session_id, source, ref_id, start_ms, original, folded)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![session_id.as_str(), source, ref_id, start, text, fold(text)],
    )
    .map(|_| ())
}

/// SQLite-backed [`SearchIndex`].
#[derive(Debug, Clone)]
pub struct SqliteSearchIndex {
    db: Database,
}

impl SqliteSearchIndex {
    /// Creates the index adapter over `db`.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl SearchIndex for SqliteSearchIndex {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, RepositoryError> {
        let Some(expression) = build_match_query(query) else {
            return Ok(Vec::new());
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT d.session_id, d.source, d.start_ms, d.original
                 FROM search_fts
                 JOIN search_docs d ON d.id = search_fts.rowid
                 WHERE search_fts MATCH ?1
                 ORDER BY search_fts.rank, d.id
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![expression, limit], |row| {
                let source: String = row.get(1)?;
                let source = match source.as_str() {
                    SOURCE_TRANSCRIPT => SearchSource::Transcript,
                    SOURCE_NOTE => SearchSource::Note,
                    other => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            format!("unknown search source {other:?}").into(),
                        ))
                    }
                };
                let original: String = row.get(3)?;
                Ok(SearchHit {
                    session_id: SessionId::new(row.get::<_, String>(0)?),
                    source,
                    start_ms: row
                        .get::<_, Option<i64>>(2)?
                        .map(|v| {
                            u64::try_from(v)
                                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, v))
                        })
                        .transpose()?,
                    excerpt: excerpt(&original),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{build_match_query, excerpt, fold};

    #[test]
    fn fold_unifies_turkish_i_variants_and_case() {
        assert_eq!(fold("İŞ PLANI"), "iş plani");
        assert_eq!(fold("iş planı"), "iş plani");
        assert_eq!(fold("GÖRÜŞME"), "görüşme");
        assert_eq!(fold("Toplantı"), "toplanti");
    }

    #[test]
    fn match_query_quotes_every_word_as_a_prefix_term() {
        assert_eq!(build_match_query("iş planı").unwrap(), "\"iş\"* \"plani\"*");
        // Operators and punctuation in user input are neutralised.
        assert_eq!(
            build_match_query("a AND \"b\" OR (c) *").unwrap(),
            "\"a\"* \"and\"* \"b\"* \"or\"* \"c\"*"
        );
        assert_eq!(build_match_query("  ?!  "), None);
        assert_eq!(build_match_query(""), None);
    }

    #[test]
    fn excerpt_truncates_on_char_boundaries() {
        let long = "ş".repeat(300);
        let e = excerpt(&long);
        assert_eq!(e.chars().count(), 201);
        assert!(e.ends_with('…'));
        assert_eq!(excerpt("kısa"), "kısa");
    }
}
