use sea_orm::{DbErr, QueryResult, TryGetable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaLayout {
    LegacyUsd,
    CurrencyEpoch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonetaryColumn {
    pub legacy: &'static str,
    pub current: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonetaryTable {
    pub name: &'static str,
    pub columns: &'static [MonetaryColumn],
}

const BALANCE: MonetaryColumn = MonetaryColumn {
    legacy: "balance_nano_usd",
    current: "balance_nano",
};
const CHARGE: MonetaryColumn = MonetaryColumn {
    legacy: "charge_nano_usd",
    current: "charge_nano",
};
const TOTAL_LIMIT: MonetaryColumn = MonetaryColumn {
    legacy: "spend_limit_total_nano_usd",
    current: "spend_limit_total_nano",
};
const HOURLY_LIMIT: MonetaryColumn = MonetaryColumn {
    legacy: "spend_limit_hourly_nano_usd",
    current: "spend_limit_hourly_nano",
};
const DAILY_LIMIT: MonetaryColumn = MonetaryColumn {
    legacy: "spend_limit_daily_nano_usd",
    current: "spend_limit_daily_nano",
};
const MAXIMUM: MonetaryColumn = MonetaryColumn {
    legacy: "maximum_nano_usd",
    current: "maximum_nano",
};
const ACTUAL: MonetaryColumn = MonetaryColumn {
    legacy: "actual_nano_usd",
    current: "actual_nano",
};
const AMOUNT: MonetaryColumn = MonetaryColumn {
    legacy: "amount_nano_usd",
    current: "amount_nano",
};

/// Scalar accounting fields in the current schema. This catalog excludes source
/// prices, external payment amounts, CNY-fen quotas, and embedded historical JSON.
pub const MONETARY_TABLES: &[MonetaryTable] = &[
    MonetaryTable {
        name: "users",
        columns: &[BALANCE],
    },
    MonetaryTable {
        name: "api_keys",
        columns: &[
            MonetaryColumn {
                legacy: "sub_account_balance_nano",
                current: "sub_account_balance_nano",
            },
            TOTAL_LIMIT,
            HOURLY_LIMIT,
            DAILY_LIMIT,
        ],
    },
    MonetaryTable {
        name: "orgs",
        columns: &[TOTAL_LIMIT, HOURLY_LIMIT, DAILY_LIMIT],
    },
    MonetaryTable {
        name: "org_members",
        columns: &[TOTAL_LIMIT, HOURLY_LIMIT, DAILY_LIMIT],
    },
    MonetaryTable {
        name: "billing_plans",
        columns: &[MonetaryColumn {
            legacy: "grant_amount_nano_usd",
            current: "grant_amount_nano",
        }],
    },
    MonetaryTable {
        name: "billing_ledger",
        columns: &[
            MonetaryColumn {
                legacy: "delta_nano_usd",
                current: "delta_nano",
            },
            MonetaryColumn {
                legacy: "balance_after_nano_usd",
                current: "balance_after_nano",
            },
        ],
    },
    MonetaryTable {
        name: "request_logs",
        columns: &[CHARGE],
    },
    MonetaryTable {
        name: "admin_revenue_daily_summaries",
        columns: &[MonetaryColumn {
            legacy: "total_charge_nano_usd",
            current: "total_charge_nano",
        }],
    },
    MonetaryTable {
        name: "admin_revenue_daily_model_rows",
        columns: &[CHARGE],
    },
    MonetaryTable {
        name: "admin_revenue_daily_user_rows",
        columns: &[CHARGE],
    },
    MonetaryTable {
        name: "admin_revenue_daily_user_model_rows",
        columns: &[CHARGE],
    },
    MonetaryTable {
        name: "store_order_reward_recoveries",
        columns: &[
            MonetaryColumn {
                legacy: "original_nano_usd",
                current: "original_nano",
            },
            MonetaryColumn {
                legacy: "reserved_nano_usd",
                current: "reserved_nano",
            },
            MonetaryColumn {
                legacy: "recovered_nano_usd",
                current: "recovered_nano",
            },
        ],
    },
    MonetaryTable {
        name: "store_order_recovery_claims",
        columns: &[AMOUNT],
    },
    MonetaryTable {
        name: "store_quota_reservations",
        columns: &[MAXIMUM, ACTUAL],
    },
    MonetaryTable {
        name: "store_admission_tokens",
        columns: &[MAXIMUM],
    },
    MonetaryTable {
        name: "store_admission_terminal_receipts",
        columns: &[ACTUAL],
    },
    MonetaryTable {
        name: "studio_bridge_ops",
        columns: &[AMOUNT],
    },
];

const MONETARY_ALIASES: &[MonetaryColumn] = &[MonetaryColumn {
    legacy: "owner_balance_nano_usd",
    current: "owner_balance_nano",
}];

fn known_column(legacy_name: &str) -> Option<&'static MonetaryColumn> {
    MONETARY_TABLES
        .iter()
        .flat_map(|table| table.columns.iter())
        .chain(MONETARY_ALIASES.iter())
        .find(|field| field.legacy == legacy_name)
}

/// Resolve one unqualified accounting column or result alias. Unknown names are
/// returned unchanged; this function does not validate SQL or infer currency.
pub fn column(layout: SchemaLayout, legacy_name: &str) -> &str {
    match layout {
        SchemaLayout::LegacyUsd => legacy_name,
        SchemaLayout::CurrencyEpoch => {
            known_column(legacy_name).map_or(legacy_name, |field| field.current)
        }
    }
}

/// Compose trusted SQL fragments with explicitly selected accounting identifiers.
/// Bind user values through statement parameters, never through `Text`.
#[derive(Debug, Clone, Copy)]
pub enum SqlPart {
    Text(&'static str),
    Column {
        table: &'static str,
        legacy_name: &'static str,
    },
    Alias {
        legacy_name: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("unknown accounting SQL column {table}.{legacy_name}")]
    UnknownColumn {
        table: &'static str,
        legacy_name: &'static str,
    },
    #[error("unknown accounting SQL alias {legacy_name}")]
    UnknownAlias { legacy_name: &'static str },
}

/// Render identifiers for one frozen layout without interpreting literals,
/// comments, bind parameters, or any other text in the trusted SQL fragments.
/// `Column` checks table membership but emits only the quoted column name; put
/// a trusted qualifier such as `u.` in the preceding `Text` fragment.
pub fn render_sql(layout: SchemaLayout, parts: &[SqlPart]) -> Result<String, SchemaError> {
    let mut sql = String::new();
    for part in parts {
        let field = match *part {
            SqlPart::Text(text) => {
                sql.push_str(text);
                continue;
            }
            SqlPart::Column { table, legacy_name } => MONETARY_TABLES
                .iter()
                .find(|entry| entry.name == table)
                .and_then(|entry| {
                    entry
                        .columns
                        .iter()
                        .find(|field| field.legacy == legacy_name)
                })
                .ok_or(SchemaError::UnknownColumn { table, legacy_name })?,
            SqlPart::Alias { legacy_name } => {
                known_column(legacy_name).ok_or(SchemaError::UnknownAlias { legacy_name })?
            }
        };
        sql.push('"');
        sql.push_str(match layout {
            SchemaLayout::LegacyUsd => field.legacy,
            SchemaLayout::CurrencyEpoch => field.current,
        });
        sql.push('"');
    }
    Ok(sql)
}

pub trait AccountingQueryResultExt {
    /// Decode a mapped result name using the operation's frozen layout. A missing
    /// or invalid current column returns an error; legacy fallback is forbidden.
    fn try_get_accounting<T: TryGetable>(
        &self,
        layout: SchemaLayout,
        prefix: &str,
        legacy_name: &str,
    ) -> Result<T, DbErr>;
}

impl AccountingQueryResultExt for QueryResult {
    fn try_get_accounting<T: TryGetable>(
        &self,
        layout: SchemaLayout,
        prefix: &str,
        legacy_name: &str,
    ) -> Result<T, DbErr> {
        self.try_get(prefix, column(layout, legacy_name))
    }
}
