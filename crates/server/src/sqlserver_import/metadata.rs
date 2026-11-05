use super::*;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Target {
    pub server: String,
    pub database: String,
    pub database_id: i32,
    pub login_sid: String,
    pub user_id: i32,
    pub user_sid: String,
    pub schema: String,
    pub schema_id: i32,
    pub schema_owner: i32,
    pub table: String,
    pub object_id: i32,
    pub created: String,
    pub modified: String,
    pub can_alter: bool,
    pub language: String,
    pub date_format: String,
    pub memory_optimized: bool,
    pub temporal: u8,
    pub file_table: bool,
    pub system_table: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Column {
    pub target: Target,
    pub column_id: i32,
    pub name: String,
    pub system_type: u8,
    pub user_type: i32,
    pub native_type_name: String,
    pub max_length: i32,
    pub precision: u8,
    pub scale: u8,
    pub collation: Option<String>,
    pub nullable: bool,
    pub identity: bool,
    pub computed: bool,
    pub generated: u8,
    pub hidden: bool,
    pub column_set: bool,
    pub seed: Option<String>,
    pub increment: Option<String>,
    pub default_id: i32,
    pub computed_hash: Option<String>,
    pub can_insert: bool,
    pub source_ordinal: Option<usize>,
    pub matches: u32,
}
impl Target {
    pub(super) fn sql(&self) -> String {
        format!(
            "{}.{}.{}",
            q(&self.database),
            q(&self.schema),
            q(&self.table)
        )
    }
    pub(super) fn path(&self) -> sift_protocol::ObjectPath {
        sift_protocol::ObjectPath {
            catalog: Some(self.database.clone()),
            schema: Some(self.schema.clone()),
            name: self.table.clone(),
            kind: Some(sift_protocol::ObjectKind::Table),
            routine_args: None,
        }
    }
}
pub(super) fn q(value: &str) -> String {
    format!("[{}]", value.replace(']', "]]"))
}
pub(super) async fn capture(ctx: &Context, input: &Input) -> ApiResult<Vec<Column>> {
    if input.columns.is_empty()
        || input.columns.len() > 2000
        || input.columns.iter().any(|c| {
            c.name.is_empty() || c.name.contains('\0') || c.name.encode_utf16().count() > 128
        })
    {
        return Err(bad(
            "Reviewed SQL Server imports require 1..2000 input columns with native-length names",
        ));
    }
    let mut params = vec![Value::Text(input.table.clone())];
    let mut ordinal = String::from("CASE");
    let mut matches = Vec::new();
    for (index, column) in input.columns.iter().enumerate() {
        params.push(Value::Text(column.name.clone()));
        let test = format!("c.name=CONVERT(nvarchar(128),@P{})", params.len());
        ordinal.push_str(&format!(" WHEN {test} THEN {index}"));
        matches.push(format!("CASE WHEN {test} THEN 1 ELSE 0 END"));
    }
    ordinal.push_str(" ELSE NULL END");
    let sql = include_str!("metadata.sql")
        .replace("{ordinal}", &ordinal)
        .replace("{matches}", &matches.join("+"));
    let rows = ctx.query(&sql, params).await?;
    let mut json = String::new();
    for row in rows {
        let [Value::Text(part)] = row.values.as_slice() else {
            return Err(bad("Unexpected SQL Server import catalog shape"));
        };
        if json.len().saturating_add(part.len()) > 8 * 1024 * 1024 {
            return Err(bad("SQL Server import catalog exceeds 8 MiB"));
        }
        json.push_str(part);
    }
    let columns: Vec<Column> = serde_json::from_str(&json)
        .map_err(|_| bad("Unexpected SQL Server import catalog shape"))?;
    if columns.is_empty() || columns.len() > 2000 {
        return Err(bad(
            "Target table was not found, is outside the current database, or exceeds 2000 columns",
        ));
    }
    let target = &columns[0].target;
    if target.system_table || target.memory_optimized || target.temporal != 0 || target.file_table {
        return Err(bad(
            "Reviewed import currently requires an ordinary disk-based user table",
        ));
    }
    if columns.iter().any(|c| &c.target != target || c.matches > 1) {
        return Err(bad(
            "Multiple input columns bind the same native target, or target observations changed",
        ));
    }
    if (0..input.columns.len()).any(|i| !columns.iter().any(|c| c.source_ordinal == Some(i))) {
        return Err(bad("An input column does not exist in the native target"));
    }
    if columns
        .iter()
        .any(|c| c.computed && c.computed_hash.is_none())
    {
        return Err(bad(
            "VIEW DEFINITION is required to review computed target columns",
        ));
    }
    Ok(columns)
}
pub(super) fn sql_type(c: &Column, typed: bool) -> ApiResult<String> {
    let ty = match c.system_type {
        127 => "bigint".into(),
        56 => "int".into(),
        52 => "smallint".into(),
        48 => "tinyint".into(),
        104 => "bit".into(),
        106 | 108 if c.precision > 0 && c.precision <= 38 && c.scale <= c.precision => {
            format!("decimal({},{})", c.precision, c.scale)
        }
        59 => "real".into(),
        62 => "float".into(),
        60 => "money".into(),
        122 => "smallmoney".into(),
        36 => "uniqueidentifier".into(),
        40 => "date".into(),
        58 => "smalldatetime".into(),
        61 => "datetime".into(),
        41..=43 if c.scale <= 7 => format!(
            "{}({})",
            match c.system_type {
                41 => "time",
                42 => "datetime2",
                _ => "datetimeoffset",
            },
            c.scale
        ),
        167 | 175 if c.max_length == -1 || (1..=8000).contains(&c.max_length) => format!(
            "{}({})",
            if c.system_type == 167 {
                "varchar"
            } else {
                "char"
            },
            if c.max_length == -1 {
                "max".into()
            } else {
                c.max_length.to_string()
            }
        ),
        231 | 239
            if c.max_length == -1
                || ((2..=8000).contains(&c.max_length) && c.max_length % 2 == 0) =>
        {
            format!(
                "{}({})",
                if c.system_type == 231 {
                    "nvarchar"
                } else {
                    "nchar"
                },
                if c.max_length == -1 {
                    "max".into()
                } else {
                    (c.max_length / 2).to_string()
                }
            )
        }
        165 | 173 if typed && (c.max_length == -1 || (1..=8000).contains(&c.max_length)) => {
            format!(
                "{}({})",
                if c.system_type == 165 {
                    "varbinary"
                } else {
                    "binary"
                },
                if c.max_length == -1 {
                    "max".into()
                } else {
                    c.max_length.to_string()
                }
            )
        }
        _ => return Err(bad(
            "Target type is not supported by reviewed import; binary values require typed Parquet",
        )),
    };
    Ok(ty)
}
