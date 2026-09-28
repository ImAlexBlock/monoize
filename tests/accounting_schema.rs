#[path = "../src/accounting/schema.rs"]
mod schema;

use schema::{
    AccountingQueryResultExt, MONETARY_TABLES, SchemaLayout, SqlPart, column, render_sql,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;
use std::collections::BTreeSet;

#[test]
fn currency_layout_maps_only_declared_accounting_names() {
    for (legacy, current) in [
        ("balance_nano_usd", "balance_nano"),
        ("delta_nano_usd", "delta_nano"),
        ("balance_after_nano_usd", "balance_after_nano"),
        ("grant_amount_nano_usd", "grant_amount_nano"),
        ("spend_limit_total_nano_usd", "spend_limit_total_nano"),
        ("spend_limit_hourly_nano_usd", "spend_limit_hourly_nano"),
        ("spend_limit_daily_nano_usd", "spend_limit_daily_nano"),
        ("charge_nano_usd", "charge_nano"),
        ("total_charge_nano_usd", "total_charge_nano"),
        ("owner_balance_nano_usd", "owner_balance_nano"),
        ("original_nano_usd", "original_nano"),
        ("reserved_nano_usd", "reserved_nano"),
        ("recovered_nano_usd", "recovered_nano"),
        ("maximum_nano_usd", "maximum_nano"),
        ("actual_nano_usd", "actual_nano"),
        ("amount_nano_usd", "amount_nano"),
        ("sub_account_balance_nano", "sub_account_balance_nano"),
        ("unit_price_nano_usd", "unit_price_nano_usd"),
        ("unrecognized_nano_usd", "unrecognized_nano_usd"),
    ] {
        assert_eq!(column(SchemaLayout::LegacyUsd, legacy), legacy);
        assert_eq!(column(SchemaLayout::CurrencyEpoch, legacy), current);
    }
}

#[test]
fn manifest_has_no_table_or_target_column_collisions() {
    let mut tables = BTreeSet::new();
    for table in MONETARY_TABLES {
        assert!(tables.insert(table.name), "duplicate table {}", table.name);
        assert!(!table.columns.is_empty());
        let mut legacy = BTreeSet::new();
        let mut current = BTreeSet::new();
        for field in table.columns {
            assert!(
                legacy.insert(field.legacy),
                "duplicate source in {}",
                table.name
            );
            assert!(
                current.insert(field.current),
                "duplicate target in {}",
                table.name
            );
            assert_eq!(
                column(SchemaLayout::CurrencyEpoch, field.legacy),
                field.current
            );
        }
    }
    assert!(tables.contains("studio_bridge_ops"));
    for excluded in [
        "billing_rate_records",
        "model_metadata_records",
        "studio_steps",
        "studio_runs",
        "studio_templates",
    ] {
        assert!(
            !tables.contains(excluded),
            "non-accounting table {excluded}"
        );
    }
}

#[tokio::test]
async fn manifest_covers_existing_accounting_columns_without_retired_tables() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    monoize::migration::Migrator::up(&db, None).await.unwrap();
    let tables = db
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_schema WHERE type = 'table'",
        ))
        .await
        .unwrap();
    let mut actual = BTreeSet::new();
    for table in tables {
        let name: String = table.try_get("", "name").unwrap();
        let fields = db
            .query_all(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT name FROM pragma_table_info(?)",
                [name.clone().into()],
            ))
            .await
            .unwrap();
        for field in fields {
            let field: String = field.try_get("", "name").unwrap();
            if (field.ends_with("_nano_usd") || field == "sub_account_balance_nano")
                && field != "unit_price_nano_usd"
            {
                actual.insert((name.clone(), field));
            }
        }
    }
    let declared = MONETARY_TABLES
        .iter()
        .flat_map(|table| {
            table
                .columns
                .iter()
                .map(move |field| (table.name.to_owned(), field.legacy.to_owned()))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(declared, actual);
}

#[test]
fn rendering_changes_explicit_identifiers_and_keeps_literal_bytes() {
    let parts = [
        SqlPart::Text("SELECT u."),
        SqlPart::Column {
            table: "users",
            legacy_name: "balance_nano_usd",
        },
        SqlPart::Text(" AS "),
        SqlPart::Alias {
            legacy_name: "owner_balance_nano_usd",
        },
        SqlPart::Text(
            ", 'balance_nano_usd' AS evidence FROM users u WHERE u.id = $1 -- balance_nano_usd",
        ),
    ];
    assert_eq!(
        render_sql(SchemaLayout::LegacyUsd, &parts).unwrap(),
        "SELECT u.\"balance_nano_usd\" AS \"owner_balance_nano_usd\", 'balance_nano_usd' AS evidence FROM users u WHERE u.id = $1 -- balance_nano_usd"
    );
    assert_eq!(
        render_sql(SchemaLayout::CurrencyEpoch, &parts).unwrap(),
        "SELECT u.\"balance_nano\" AS \"owner_balance_nano\", 'balance_nano_usd' AS evidence FROM users u WHERE u.id = $1 -- balance_nano_usd"
    );
}

#[test]
fn rendering_rejects_unknown_or_wrong_table_identifiers() {
    for part in [
        SqlPart::Column {
            table: "users",
            legacy_name: "charge_nano_usd",
        },
        SqlPart::Column {
            table: "billing_rate_records",
            legacy_name: "unit_price_nano_usd",
        },
        SqlPart::Column {
            table: "users",
            legacy_name: "balance_nano_usd; DROP TABLE users",
        },
        SqlPart::Alias {
            legacy_name: "balance_nano_usd\"; SELECT 1; --",
        },
    ] {
        for layout in [SchemaLayout::LegacyUsd, SchemaLayout::CurrencyEpoch] {
            assert!(render_sql(layout, &[part]).is_err());
        }
    }
}

#[tokio::test]
async fn row_access_uses_the_frozen_layout_without_legacy_fallback() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT '11' AS balance_nano_usd, '22' AS balance_nano,
                    '33' AS joined_total_charge_nano_usd,
                    '44' AS joined_total_charge_nano, 'unchanged' AS label",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get_accounting::<String>(SchemaLayout::LegacyUsd, "", "balance_nano_usd")
            .unwrap(),
        "11"
    );
    assert_eq!(
        row.try_get_accounting::<String>(SchemaLayout::CurrencyEpoch, "", "balance_nano_usd")
            .unwrap(),
        "22"
    );
    assert_eq!(
        row.try_get_accounting::<String>(
            SchemaLayout::CurrencyEpoch,
            "joined_",
            "total_charge_nano_usd"
        )
        .unwrap(),
        "44"
    );
    assert_eq!(
        row.try_get_accounting::<String>(SchemaLayout::CurrencyEpoch, "", "label")
            .unwrap(),
        "unchanged"
    );

    let legacy_only = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT '55' AS balance_nano_usd",
        ))
        .await
        .unwrap()
        .unwrap();
    assert!(
        legacy_only
            .try_get_accounting::<String>(SchemaLayout::CurrencyEpoch, "", "balance_nano_usd")
            .is_err()
    );
}
