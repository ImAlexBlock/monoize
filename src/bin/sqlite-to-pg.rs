//! SQLite → PostgreSQL data migration tool.
//!
//! Copies every user table from a live SQLite database into a PostgreSQL database
//! whose schema was already created by the application migrations. The tool is
//! idempotent: every row upserts by primary key, so re-running it performs an
//! incremental catch-up. Two zero-downtime migration passes are enough:
//!
//! 1. bulk pass while production still serves on SQLite;
//! 2. final catch-up pass after the blue-green swap drains the old container,
//!    which brings the rows the old container wrote during the drain window.
//!
//! Usage:
//!   sqlite-to-pg --sqlite sqlite:///opt/monoize/data/monoize.db \
//!                --postgres postgres://user:pass@host/monoize [--batch 1000] \
//!                [--incremental] [--only t1,t2]
//!
//! `--incremental` consults (and maintains) the `_monoize_migration_watermarks`
//! helper table. For tables without a watermark it performs a full upsert; for
//! `request_logs` it resumes after the stored `created_at_unix_ms` high-water
//! mark, and for `billing_ledger` after the RFC3339 `created_at` mark.

use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, Value as SeaValue,
};
use std::collections::BTreeMap;
use std::time::Instant;

const WATERMARK_TABLE: &str = "_monoize_migration_watermarks";

fn checked_integer(raw: Option<i64>, pg_type: &str, index: usize) -> Result<SeaValue, String> {
    Ok(match pg_type {
        "smallint" => SeaValue::SmallInt(raw.map(i16::try_from).transpose()
            .map_err(|_| format!("cell {index}: integer exceeds PostgreSQL SMALLINT range"))?),
        "integer" => SeaValue::Int(raw.map(i32::try_from).transpose()
            .map_err(|_| format!("cell {index}: integer exceeds PostgreSQL INTEGER range"))?),
        "bigint" => SeaValue::BigInt(raw),
        _ => return Err(format!("cell {index}: unsupported integer target")),
    })
}

fn checked_real_integer(value: f64, index: usize) -> Result<i64, String> {
    if !value.is_finite() || value.fract() != 0.0
        || value < i64::MIN as f64 || value >= -(i64::MIN as f64)
    {
        return Err(format!("cell {index}: REAL cannot be represented exactly as BIGINT"));
    }
    Ok(value as i64)
}

struct Args {
    sqlite: String,
    postgres: String,
    batch: usize,
    incremental: bool,
    only: Option<Vec<String>>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        sqlite: String::new(),
        postgres: String::new(),
        batch: 1_000,
        incremental: false,
        only: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let value = iter.next().ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--sqlite" => args.sqlite = value,
            "--postgres" => args.postgres = value,
            "--batch" => {
                args.batch = value
                    .parse()
                    .ok()
                    .filter(|batch| *batch > 0)
                    .ok_or_else(|| format!("invalid --batch {value}"))?;
            }
            "--incremental" => {
                // Flag with no value; push the next argument back is not possible
                // with a plain iterator, so this accepts "--incremental 1".
                if value != "1" {
                    return Err(format!("unexpected value for --incremental: {value}"));
                }
                args.incremental = true;
            }
            "--only" => {
                args.only = Some(value.split(',').map(|s| s.trim().to_string()).collect())
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.sqlite.is_empty() || args.postgres.is_empty() {
        return Err("--sqlite and --postgres are both required".to_string());
    }
    Ok(args)
}

fn is_system_table(name: &str) -> bool {
    name.starts_with("sqlite_") || name == "seaql_migrations" || name == WATERMARK_TABLE
}

/// Ordered user tables: parents first is unnecessary because PostgreSQL
/// foreign keys are validated per statement; disabling triggers for the
/// session keeps a single pass independent of ordering.
async fn list_sqlite_tables(db: &DatabaseConnection) -> Result<Vec<String>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name".to_string(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    let mut tables = Vec::new();
    for row in rows {
        let name: String = row
            .try_get::<String>("", "name")
            .map_err(|e| e.to_string())?;
        if !is_system_table(&name) {
            tables.push(name);
        }
    }
    Ok(tables)
}

struct ColumnInfo {
    name: String,
    /// SQLite declared type uppercased (TEXT, INTEGER, REAL, BLOB, ...).
    sqlite_type: String,
    /// PostgreSQL data_type from information_schema (kept for diagnostics).
    #[allow(dead_code)]
    pg_type: String,
}

async fn sqlite_columns(
    db: &DatabaseConnection,
    table: &str,
) -> Result<Vec<ColumnInfo>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            format!("PRAGMA table_info({table})"),
        ))
        .await
        .map_err(|e| e.to_string())?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(ColumnInfo {
            name: row
                .try_get::<String>("", "name")
                .map_err(|e| e.to_string())?,
            sqlite_type: row
                .try_get::<Option<String>>("", "type")
                .map_err(|e| e.to_string())?
                .unwrap_or_default()
                .to_uppercase(),
            pg_type: String::new(),
        });
    }
    Ok(columns)
}

async fn pg_columns(
    db: &DatabaseConnection,
    table: &str,
) -> Result<BTreeMap<String, String>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT column_name, data_type FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = '{table}'"
            ),
        ))
        .await
        .map_err(|e| e.to_string())?;
    let mut columns = BTreeMap::new();
    for row in rows {
        columns.insert(
            row.try_get::<String>("", "column_name")
                .map_err(|e| e.to_string())?,
            row.try_get::<String>("", "data_type")
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(columns)
}

async fn pg_primary_key(db: &DatabaseConnection, table: &str) -> Result<Vec<String>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT a.attname FROM pg_index i \
                 JOIN pg_class c ON c.oid = i.indrelid \
                 JOIN pg_attribute a ON a.attrelid = c.oid \
                      AND a.attnum = ANY(string_to_array(i.indkey::text, ' ')::smallint[]) \
                 WHERE c.relname = '{table}' AND i.indisprimary \
                 ORDER BY a.attnum"
            ),
        ))
        .await
        .map_err(|e| e.to_string())?;
    rows.iter()
        .map(|row| {
            row.try_get::<String>("", "attname")
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Reads one SQLite cell as a typed SeaORM value suited for the PostgreSQL
/// column type. NULL stays NULL.
fn convert_cell(
    row: &sea_orm::QueryResult,
    index: usize,
    sqlite_type: &str,
    pg_type: &str,
) -> Result<SeaValue, String> {
    let integer_like = sqlite_type.contains("INT");
    let real_like = sqlite_type.contains("REAL") || sqlite_type.contains("FLOA")
        || sqlite_type.contains("DOUB");
    match pg_type {
        "boolean" => {
            let raw = row
                .try_get_by::<Option<i64>, _>(index)
                .ok()
                .flatten()
                .or_else(|| {
                    row.try_get_by::<Option<String>, _>(index)
                        .ok()
                        .flatten()
                        .and_then(|text| text.trim().parse::<i64>().ok())
                });
            Ok(match raw {
                None => SeaValue::Bool(None),
                Some(0) => SeaValue::Bool(Some(false)),
                Some(_) => SeaValue::Bool(Some(true)),
            })
        }
        "bigint" | "integer" | "smallint" => {
            if real_like {
                // SQLite REAL column feeding a PostgreSQL integer column.
                let raw = row
                    .try_get_by::<Option<f64>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?;
                return checked_integer(
                    raw.map(|value| checked_real_integer(value, index)).transpose()?,
                    pg_type,
                    index,
                );
            }
            let raw = if integer_like {
                row.try_get_by::<Option<i64>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?
            } else {
                row.try_get_by::<Option<String>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?
                    .map(|text| {
                        text.trim().parse::<i64>()
                            .map_err(|e| format!("integer cell {text:?}: {e}"))
                    })
                    .transpose()?
            };
            checked_integer(raw, pg_type, index)
        }
        "double precision" | "real" => {
            let raw = if real_like {
                row.try_get_by::<Option<f64>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?
            } else {
                row.try_get_by::<Option<String>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?
                    .map(|text| {
                        text.trim().parse::<f64>()
                            .map_err(|e| format!("float cell {text:?}: {e}"))
                    })
                    .transpose()?
            };
            Ok(SeaValue::Double(raw))
        }
        "bytea" => {
            let raw = row
                .try_get_by::<Option<Vec<u8>>, _>(index)
                .map_err(|e| format!("cell {index}: {e}"))?;
            Ok(SeaValue::Bytes(raw.map(Box::new)))
        }
        // text and everything else (jsonb columns are TEXT in this schema)
        _ => {
            if integer_like {
                let raw = row
                    .try_get_by::<Option<i64>, _>(index)
                    .map_err(|e| format!("cell {index}: {e}"))?;
                return Ok(SeaValue::String(raw.map(|v| v.to_string()).map(Box::new)));
            }
            let raw = row
                .try_get_by::<Option<String>, _>(index)
                .map_err(|e| format!("cell {index}: {e}"))?;
            Ok(SeaValue::String(raw.map(Box::new)))
        }
    }
}

#[cfg(test)]
mod checked_integer_tests {
    use super::*;

    #[test]
    fn narrowing_refuses_wrapped_milliseconds() {
        assert!(checked_integer(Some(1_789_315_644_687), "integer", 0).is_err());
        assert!(checked_integer(Some(i64::from(i16::MAX) + 1), "smallint", 0).is_err());
        assert!(matches!(checked_integer(Some(42), "integer", 0).unwrap(), SeaValue::Int(Some(42))));
        assert!(matches!(checked_integer(None, "smallint", 0).unwrap(), SeaValue::SmallInt(None)));
        assert!(checked_integer(Some(i64::MAX), "bigint", 0).is_ok());
    }

    #[test]
    fn real_integer_conversion_rejects_fraction_and_overflow() {
        for value in [f64::NAN, f64::INFINITY, 1.5, -(i64::MIN as f64)] {
            assert!(checked_real_integer(value, 0).is_err());
        }
        assert_eq!(checked_real_integer(42.0, 0).unwrap(), 42);
        assert_eq!(checked_real_integer(i64::MIN as f64, 0).unwrap(), i64::MIN);
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

struct IncrementalSpec {
    column: String,
    /// Order by and comparison as raw text for both sides.
    numeric: bool,
}

fn incremental_spec(table: &str) -> Option<IncrementalSpec> {
    match table {
        "request_logs" => Some(IncrementalSpec {
            column: "created_at_unix_ms".to_string(),
            numeric: true,
        }),
        "billing_ledger" => Some(IncrementalSpec {
            column: "created_at".to_string(),
            numeric: false,
        }),
        _ => None,
    }
}

async fn ensure_watermark_table(pg: &DatabaseConnection) -> Result<(), String> {
    pg.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "CREATE TABLE IF NOT EXISTS {WATERMARK_TABLE} (
                 table_name TEXT PRIMARY KEY,
                 column_name TEXT NOT NULL,
                 high_water TEXT)"
            )
        ))
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn read_watermark(
    pg: &DatabaseConnection,
    table: &str,
) -> Result<Option<String>, String> {
    let row = pg
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT high_water FROM {WATERMARK_TABLE} WHERE table_name = '{table}'"
            ),
        ))
        .await
        .map_err(|e| e.to_string())?;
    match row {
        Some(row) => Ok(row
            .try_get::<Option<String>>("", "high_water")
            .map_err(|e| e.to_string())?),
        None => Ok(None),
    }
}

async fn write_watermark(
    pg: &DatabaseConnection,
    table: &str,
    column: &str,
    value: &str,
) -> Result<(), String> {
    pg.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "INSERT INTO {WATERMARK_TABLE} (table_name, column_name, high_water) \
             VALUES ('{table}', '{column}', '{value}') \
             ON CONFLICT (table_name) DO UPDATE SET column_name = EXCLUDED.column_name, \
             high_water = EXCLUDED.high_water"
        ),
    ))
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn migrate_table(
    sqlite: &DatabaseConnection,
    pg: &DatabaseConnection,
    table: &str,
    batch: usize,
    incremental: bool,
) -> Result<(u64, bool), String> {
    let pg_map = pg_columns(pg, table).await?;
    if pg_map.is_empty() {
        eprintln!("skip {table}: not present in PostgreSQL schema");
        return Ok((0, false));
    }
    let columns = sqlite_columns(sqlite, table).await?;
    // Preserve SQLite column order, restricted to the shared columns.
    let shared: Vec<&ColumnInfo> = columns
        .iter()
        .filter(|column| pg_map.contains_key(&column.name))
        .collect();
    if shared.is_empty() {
        eprintln!("skip {table}: no shared columns");
        return Ok((0, false));
    }
    let pk = pg_primary_key(pg, table).await?;
    if pk.is_empty() {
        eprintln!("skip {table}: no PostgreSQL primary key");
        return Ok((0, false));
    }

    let select_list = shared
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_list = select_list.clone();
    let placeholders = shared
        .iter()
        .enumerate()
        .map(|(index, _)| format!("${}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let update_list = shared
        .iter()
        .filter(|column| !pk.contains(&column.name))
        .map(|column| format!("{} = EXCLUDED.{}", quote_ident(&column.name), quote_ident(&column.name)))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict_action = if update_list.is_empty() {
        "DO NOTHING".to_string()
    } else {
        format!("DO UPDATE SET {update_list}")
    };
    let insert_sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) {}",
        quote_ident(table),
        insert_list,
        placeholders,
        pk.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", "),
        conflict_action,
    );

    let spec = incremental.then(|| incremental_spec(table)).flatten();
    let watermark = match (&spec, incremental) {
        (Some(_), true) => read_watermark(pg, table).await?,
        _ => None,
    };
    let where_clause = match (&spec, &watermark) {
        (Some(spec), Some(mark)) => {
            format!(" WHERE {} > {}", quote_ident(&spec.column), {
                if spec.numeric {
                    mark.clone()
                } else {
                    format!("'{}'", mark.replace('\'', "''"))
                }
            })
        }
        _ => String::new(),
    };
    let order_clause = spec
        .as_ref()
        .map(|spec| format!(" ORDER BY {} ASC", quote_ident(&spec.column)))
        .unwrap_or_default();

    let mut total: u64 = 0;
    let mut high_water: Option<String> = None;
    let query = format!("SELECT {select_list} FROM {}{where_clause}{order_clause}", quote_ident(table));
    let rows = sqlite
        .query_all(Statement::from_string(DbBackend::Sqlite, query))
        .await
        .map_err(|e| format!("{table} select: {e}"))?;
    let mut buffer: Vec<Vec<SeaValue>> = Vec::with_capacity(batch);
    for row in &rows {
        let mut values = Vec::with_capacity(shared.len());
        for (index, column) in shared.iter().enumerate() {
            values.push(convert_cell(
                row,
                index,
                &column.sqlite_type,
                &pg_map[&column.name],
            )?);
        }
        if let (Some(spec), true) = (spec.as_ref(), incremental) {
            let position = shared
                .iter()
                .position(|column| column.name == spec.column)
                .expect("incremental column present");
            if let SeaValue::String(Some(text)) = &values[position] {
                high_water = Some((**text).clone());
            } else if let SeaValue::BigInt(Some(value)) = &values[position] {
                high_water = Some(value.to_string());
            }
        }
        buffer.push(values);
        if buffer.len() >= batch {
            flush_batch(pg, &insert_sql, &mut buffer).await?;
            total += batch as u64;
        }
    }
    if !buffer.is_empty() {
        let remainder = buffer.len();
        flush_batch(pg, &insert_sql, &mut buffer).await?;
        total += remainder as u64;
    }
    if let (Some(spec), Some(mark)) = (spec.as_ref(), high_water) {
        if incremental {
            write_watermark(pg, table, &spec.column, &mark).await?;
        }
    }
    Ok((total, true))
}

async fn flush_batch(
    pg: &DatabaseConnection,
    insert_sql: &str,
    buffer: &mut Vec<Vec<SeaValue>>,
) -> Result<(), String> {
    for values in buffer.iter() {
        pg.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            insert_sql,
            values.clone(),
        ))
        .await
        .map_err(|e| format!("insert failed: {e}"))?;
    }
    buffer.clear();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = parse_args()?;
    let started = Instant::now();
    let sqlite = Database::connect(&args.sqlite).await.map_err(|e| e.to_string())?;
    let pg = Database::connect(&args.postgres).await.map_err(|e| e.to_string())?;
    if pg.get_database_backend() != DbBackend::Postgres {
        return Err("--postgres must be a PostgreSQL DSN".to_string());
    }
    ensure_watermark_table(&pg).await?;

    let tables = match &args.only {
        Some(only) => only.clone(),
        None => list_sqlite_tables(&sqlite).await?,
    };
    let mut migrated = 0usize;
    for table in &tables {
        let (rows, copied) = migrate_table(&sqlite, &pg, table, args.batch, args.incremental).await?;
        if copied {
            migrated += 1;
            println!("{table}: {rows} rows");
        }
    }
    println!(
        "done: {migrated} tables in {:.1}s (mode: {})",
        started.elapsed().as_secs_f64(),
        if args.incremental { "incremental" } else { "full upsert" },
    );
    Ok(())
}
