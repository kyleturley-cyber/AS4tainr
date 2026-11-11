//! The catalog procedures of SYSIBM a JDBC or ODBC client calls for its
//! database metadata (SQLTABLES, SQLCOLUMNS, SQLPRIMARYKEYS, ...): each
//! a query of the QSYS2 catalog views that returns the result set the
//! JDBC API defines for the matching DatabaseMetaData method (its
//! columns, their order and names, as the java.sql.DatabaseMetaData
//! documentation lists them).
//!
//! The JDBC driver of the IBM Toolbox for Java calls them as
//! `CALL SYSIBM.SQLTABLES(?, ?, ?, ?, ?)` with the catalog, schema and
//! object names (or LIKE patterns with `\` as the escape) and an options
//! string (`DATATYPE='JDBC';GETSCHEMAS=1;...`). Privileges, special
//! columns, user-defined types, function columns and pseudo columns
//! answer with no rows (docs/fidelity-gaps.md).

/// The query that answers `CALL SYSIBM.<proc>(args)`, or None when the
/// procedure is not one of these. Arguments are SQL literals (`NULL`,
/// `'text'`, numbers).
pub fn query(proc: &str, args: &[String]) -> Option<String> {
    let name = proc.trim().to_ascii_uppercase().replace('/', ".");
    let name = name.strip_prefix("SYSIBM.")?;
    let a: Vec<Option<String>> = args.iter().map(|x| value(x)).collect();
    let arg = |i: usize| a.get(i).cloned().flatten();
    let opts = arg(a.len().saturating_sub(1))
        .unwrap_or_default()
        .to_ascii_uppercase();
    let opt = |k: &str| {
        opts.split(';')
            .filter_map(|o| o.split_once('='))
            .any(|(n, v)| n.trim() == k && !matches!(v.trim().trim_matches('\''), "" | "0"))
    };
    let cat = lit(&crate::report::system_name());
    Some(match name {
        "SQLTABLES" if opt("GETCATALOGS") => {
            format!("SELECT CAST({cat} AS VARCHAR(128)) AS TABLE_CAT FROM SYSIBM.SYSDUMMY1")
        }
        "SQLTABLES" if opt("GETSCHEMAS") => format!(
            "SELECT SCHEMA_NAME AS TABLE_SCHEM, CAST({cat} AS VARCHAR(128)) AS TABLE_CATALOG \
             FROM QSYS2.SYSSCHEMAS WHERE {} ORDER BY 1",
            like("SCHEMA_NAME", arg(1).as_deref())
        ),
        "SQLTABLES" if opt("GETTABLETYPES") => "SELECT CAST(T AS VARCHAR(128)) AS TABLE_TYPE \
             FROM (VALUES ('ALIAS'), ('MATERIALIZED QUERY TABLE'), ('SYSTEM TABLE'), ('TABLE'), \
             ('VIEW')) AS X(T) ORDER BY 1"
            .to_string(),
        "SQLTABLES" => {
            let types: Vec<String> = arg(3)
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().trim_matches('\'').to_ascii_uppercase())
                .filter(|t| !t.is_empty() && t != "%")
                .collect();
            let filter = if types.is_empty() {
                "1 = 1".to_string()
            } else {
                format!(
                    "TABLE_TYPE IN ({})",
                    types.iter().map(|t| lit(t)).collect::<Vec<_>>().join(", ")
                )
            };
            format!(
                "SELECT * FROM (SELECT CAST({cat} AS VARCHAR(128)) AS TABLE_CAT, \
                 TABLE_SCHEMA AS TABLE_SCHEM, TABLE_NAME, \
                 CAST(CASE WHEN SYSTEM_TABLE = 'Y' THEN 'SYSTEM TABLE' \
                 WHEN TABLE_TYPE IN ('T', 'P') THEN 'TABLE' \
                 WHEN TABLE_TYPE IN ('V', 'L') THEN 'VIEW' \
                 WHEN TABLE_TYPE = 'A' THEN 'ALIAS' \
                 WHEN TABLE_TYPE = 'M' THEN 'MATERIALIZED QUERY TABLE' \
                 ELSE 'TABLE' END AS VARCHAR(128)) AS TABLE_TYPE, \
                 CAST(TABLE_TEXT AS VARCHAR(2000)) AS REMARKS, \
                 CAST(NULL AS VARCHAR(128)) AS TYPE_CAT, CAST(NULL AS VARCHAR(128)) AS TYPE_SCHEM, \
                 CAST(NULL AS VARCHAR(128)) AS TYPE_NAME, \
                 CAST(NULL AS VARCHAR(128)) AS SELF_REFERENCING_COL_NAME, \
                 CAST(NULL AS VARCHAR(128)) AS REF_GENERATION \
                 FROM QSYS2.SYSTABLES WHERE {} AND {}) AS T WHERE {filter} \
                 ORDER BY TABLE_TYPE, TABLE_SCHEM, TABLE_NAME",
                like("TABLE_SCHEMA", arg(1).as_deref()),
                like("TABLE_NAME", arg(2).as_deref()),
            )
        }
        "SQLCOLUMNS" => format!(
            "SELECT CAST({cat} AS VARCHAR(128)) AS TABLE_CAT, TABLE_SCHEMA AS TABLE_SCHEM, \
             TABLE_NAME, COLUMN_NAME, {jdbc} AS DATA_TYPE, {tname} AS TYPE_NAME, \
             CAST({size} AS INTEGER) AS COLUMN_SIZE, CAST(STORAGE AS INTEGER) AS BUFFER_LENGTH, \
             CAST(COALESCE(NUMERIC_SCALE, DATETIME_PRECISION) AS INTEGER) AS DECIMAL_DIGITS, \
             CAST(NUMERIC_PRECISION_RADIX AS INTEGER) AS NUM_PREC_RADIX, \
             CAST(CASE WHEN IS_NULLABLE = 'Y' THEN 1 ELSE 0 END AS INTEGER) AS NULLABLE, \
             CAST(COLUMN_TEXT AS VARCHAR(2000)) AS REMARKS, \
             CAST(COLUMN_DEFAULT AS VARCHAR(2000)) AS COLUMN_DEF, \
             CAST(CASE WHEN DATA_TYPE IN ('DATE', 'TIME', 'TIMESTMP') THEN 9 ELSE {jdbc} END AS INTEGER) AS SQL_DATA_TYPE, \
             CAST(CASE DATA_TYPE WHEN 'DATE' THEN 1 WHEN 'TIME' THEN 2 WHEN 'TIMESTMP' THEN 3 END AS INTEGER) AS SQL_DATETIME_SUB, \
             CAST(CHARACTER_OCTET_LENGTH AS INTEGER) AS CHAR_OCTET_LENGTH, \
             CAST(ORDINAL_POSITION AS INTEGER) AS ORDINAL_POSITION, \
             CAST(CASE WHEN IS_NULLABLE = 'Y' THEN 'YES' ELSE 'NO' END AS VARCHAR(3)) AS IS_NULLABLE, \
             CAST(NULL AS VARCHAR(128)) AS SCOPE_CATALOG, CAST(NULL AS VARCHAR(128)) AS SCOPE_SCHEMA, \
             CAST(NULL AS VARCHAR(128)) AS SCOPE_TABLE, CAST(NULL AS SMALLINT) AS SOURCE_DATA_TYPE, \
             CAST(CASE WHEN IS_IDENTITY = 'YES' THEN 'YES' ELSE 'NO' END AS VARCHAR(3)) AS IS_AUTOINCREMENT, \
             CAST(CASE WHEN IS_IDENTITY = 'YES' THEN 'YES' ELSE 'NO' END AS VARCHAR(3)) AS IS_GENERATEDCOLUMN \
             FROM QSYS2.SYSCOLUMNS WHERE {} AND {} AND {} \
             ORDER BY TABLE_SCHEM, TABLE_NAME, ORDINAL_POSITION",
            like("TABLE_SCHEMA", arg(1).as_deref()),
            like("TABLE_NAME", arg(2).as_deref()),
            like("COLUMN_NAME", arg(3).as_deref()),
            jdbc = JDBC_TYPE,
            tname = TYPE_NAME,
            size = COLUMN_SIZE,
        ),
        "SQLPRIMARYKEYS" => format!(
            "SELECT CAST({cat} AS VARCHAR(128)) AS TABLE_CAT, K.TABLE_SCHEMA AS TABLE_SCHEM, \
             K.TABLE_NAME, K.COLUMN_NAME, CAST(K.ORDINAL_POSITION AS SMALLINT) AS KEY_SEQ, \
             K.CONSTRAINT_NAME AS PK_NAME \
             FROM QSYS2.SYSKEYCST K JOIN QSYS2.SYSCST C \
             ON C.CONSTRAINT_SCHEMA = K.CONSTRAINT_SCHEMA AND C.CONSTRAINT_NAME = K.CONSTRAINT_NAME \
             WHERE C.CONSTRAINT_TYPE = 'PRIMARY KEY' AND {} AND {} ORDER BY K.COLUMN_NAME",
            equal("K.TABLE_SCHEMA", arg(1).as_deref()),
            equal("K.TABLE_NAME", arg(2).as_deref()),
        ),
        // Unique: 0 only unique indexes, 1 all (the ODBC SQLStatistics
        // values the driver passes).
        "SQLSTATISTICS" => {
            let unique_only = arg(3).as_deref().map(str::trim) == Some("0");
            format!(
                "SELECT * FROM (\
                 SELECT CAST({cat} AS VARCHAR(128)) AS TABLE_CAT, I.TABLE_SCHEMA AS TABLE_SCHEM, \
                 I.TABLE_NAME, CAST(CASE WHEN I.IS_UNIQUE = 'U' THEN 0 ELSE 1 END AS SMALLINT) AS NON_UNIQUE, \
                 I.INDEX_SCHEMA AS INDEX_QUALIFIER, I.INDEX_NAME, CAST(3 AS SMALLINT) AS TYPE, \
                 CAST(K.ORDINAL_POSITION AS SMALLINT) AS ORDINAL_POSITION, K.COLUMN_NAME, \
                 CAST(K.ORDERING AS VARCHAR(1)) AS ASC_OR_DESC, CAST(NULL AS INTEGER) AS CARDINALITY, \
                 CAST(NULL AS INTEGER) AS PAGES, CAST(NULL AS VARCHAR(128)) AS FILTER_CONDITION \
                 FROM QSYS2.SYSINDEXES I JOIN QSYS2.SYSKEYS K \
                 ON K.INDEX_SCHEMA = I.INDEX_SCHEMA AND K.INDEX_NAME = I.INDEX_NAME \
                 WHERE {s1} AND {t1} \
                 UNION ALL \
                 SELECT CAST({cat} AS VARCHAR(128)), K.TABLE_SCHEMA, K.TABLE_NAME, CAST(0 AS SMALLINT), \
                 K.CONSTRAINT_SCHEMA, K.CONSTRAINT_NAME, CAST(3 AS SMALLINT), \
                 CAST(K.ORDINAL_POSITION AS SMALLINT), K.COLUMN_NAME, CAST('A' AS VARCHAR(1)), \
                 CAST(NULL AS INTEGER), CAST(NULL AS INTEGER), CAST(NULL AS VARCHAR(128)) \
                 FROM QSYS2.SYSKEYCST K JOIN QSYS2.SYSCST C \
                 ON C.CONSTRAINT_SCHEMA = K.CONSTRAINT_SCHEMA AND C.CONSTRAINT_NAME = K.CONSTRAINT_NAME \
                 WHERE C.CONSTRAINT_TYPE IN ('PRIMARY KEY', 'UNIQUE') AND {s2} AND {t2}) AS X \
                 WHERE {u} ORDER BY NON_UNIQUE, INDEX_QUALIFIER, INDEX_NAME, ORDINAL_POSITION",
                s1 = equal("I.TABLE_SCHEMA", arg(1).as_deref()),
                t1 = equal("I.TABLE_NAME", arg(2).as_deref()),
                s2 = equal("K.TABLE_SCHEMA", arg(1).as_deref()),
                t2 = equal("K.TABLE_NAME", arg(2).as_deref()),
                u = if unique_only { "NON_UNIQUE = 0" } else { "1 = 1" },
            )
        }
        // Rules (JDBC importedKey*): 0 CASCADE, 1 RESTRICT, 2 SET NULL,
        // 3 NO ACTION, 4 SET DEFAULT; deferrability 7 not deferrable.
        "SQLFOREIGNKEYS" => {
            let rule = |c: &str| {
                format!(
                    "CAST(CASE R.{c} WHEN 'CASCADE' THEN 0 WHEN 'RESTRICT' THEN 1 \
                     WHEN 'SET NULL' THEN 2 WHEN 'SET DEFAULT' THEN 4 ELSE 3 END AS SMALLINT)"
                )
            };
            format!(
                "SELECT CAST({cat} AS VARCHAR(128)) AS PKTABLE_CAT, P.TABLE_SCHEMA AS PKTABLE_SCHEM, \
                 P.TABLE_NAME AS PKTABLE_NAME, P.COLUMN_NAME AS PKCOLUMN_NAME, \
                 CAST({cat} AS VARCHAR(128)) AS FKTABLE_CAT, F.TABLE_SCHEMA AS FKTABLE_SCHEM, \
                 F.TABLE_NAME AS FKTABLE_NAME, F.COLUMN_NAME AS FKCOLUMN_NAME, \
                 CAST(F.ORDINAL_POSITION AS SMALLINT) AS KEY_SEQ, {} AS UPDATE_RULE, \
                 {} AS DELETE_RULE, R.CONSTRAINT_NAME AS FK_NAME, \
                 R.UNIQUE_CONSTRAINT_NAME AS PK_NAME, CAST(7 AS SMALLINT) AS DEFERRABILITY \
                 FROM QSYS2.SYSREFCST R \
                 JOIN QSYS2.SYSKEYCST F ON F.CONSTRAINT_SCHEMA = R.CONSTRAINT_SCHEMA \
                 AND F.CONSTRAINT_NAME = R.CONSTRAINT_NAME \
                 JOIN QSYS2.SYSKEYCST P ON P.CONSTRAINT_SCHEMA = R.UNIQUE_CONSTRAINT_SCHEMA \
                 AND P.CONSTRAINT_NAME = R.UNIQUE_CONSTRAINT_NAME \
                 AND P.ORDINAL_POSITION = F.ORDINAL_POSITION \
                 WHERE {} AND {} AND {} AND {} \
                 ORDER BY FKTABLE_SCHEM, FKTABLE_NAME, PKTABLE_SCHEM, PKTABLE_NAME, KEY_SEQ",
                rule("UPDATE_RULE"),
                rule("DELETE_RULE"),
                equal("P.TABLE_SCHEMA", arg(1).as_deref()),
                equal("P.TABLE_NAME", arg(2).as_deref()),
                equal("F.TABLE_SCHEMA", arg(4).as_deref()),
                equal("F.TABLE_NAME", arg(5).as_deref()),
            )
        }
        // Procedure type: 1 no result, 2 returns a result.
        "SQLPROCEDURES" => format!(
            "SELECT CAST({cat} AS VARCHAR(128)) AS PROCEDURE_CAT, ROUTINE_SCHEMA AS PROCEDURE_SCHEM, \
             ROUTINE_NAME AS PROCEDURE_NAME, CAST(IN_PARMS + INOUT_PARMS AS INTEGER) AS NUM_INPUT_PARAMS, \
             CAST(OUT_PARMS + INOUT_PARMS AS INTEGER) AS NUM_OUTPUT_PARAMS, \
             CAST(MAX_DYNAMIC_RESULT_SETS AS INTEGER) AS NUM_RESULT_SETS, \
             CAST(LONG_COMMENT AS VARCHAR(2000)) AS REMARKS, \
             CAST(CASE WHEN MAX_DYNAMIC_RESULT_SETS > 0 THEN 2 ELSE 1 END AS SMALLINT) AS PROCEDURE_TYPE, \
             SPECIFIC_NAME FROM QSYS2.SYSROUTINES \
             WHERE ROUTINE_TYPE = 'PROCEDURE' AND {} AND {} \
             ORDER BY PROCEDURE_SCHEM, PROCEDURE_NAME, SPECIFIC_NAME",
            like("ROUTINE_SCHEMA", arg(1).as_deref()),
            like("ROUTINE_NAME", arg(2).as_deref()),
        ),
        // Function type: 1 no table, 2 returns a table.
        "SQLFUNCTIONS" => format!(
            "SELECT CAST({cat} AS VARCHAR(128)) AS FUNCTION_CAT, ROUTINE_SCHEMA AS FUNCTION_SCHEM, \
             ROUTINE_NAME AS FUNCTION_NAME, CAST(LONG_COMMENT AS VARCHAR(2000)) AS REMARKS, \
             CAST(CASE WHEN FUNCTION_TYPE = 'T' THEN 2 ELSE 1 END AS SMALLINT) AS FUNCTION_TYPE, \
             SPECIFIC_NAME FROM QSYS2.SYSROUTINES \
             WHERE ROUTINE_TYPE = 'FUNCTION' AND {} AND {} \
             ORDER BY FUNCTION_SCHEM, FUNCTION_NAME, SPECIFIC_NAME",
            like("ROUTINE_SCHEMA", arg(1).as_deref()),
            like("ROUTINE_NAME", arg(2).as_deref()),
        ),
        // Column type: 1 IN, 2 INOUT, 4 OUT.
        "SQLPROCEDURECOLS" => format!(
            "SELECT CAST({cat} AS VARCHAR(128)) AS PROCEDURE_CAT, P.SPECIFIC_SCHEMA AS PROCEDURE_SCHEM, \
             R.ROUTINE_NAME AS PROCEDURE_NAME, P.PARAMETER_NAME AS COLUMN_NAME, \
             CAST(CASE P.PARAMETER_MODE WHEN 'IN' THEN 1 WHEN 'INOUT' THEN 2 ELSE 4 END AS SMALLINT) AS COLUMN_TYPE, \
             {pj} AS DATA_TYPE, CAST(P.DATA_TYPE AS VARCHAR(128)) AS TYPE_NAME, \
             CAST(COALESCE(P.NUMERIC_PRECISION, P.CHARACTER_MAXIMUM_LENGTH) AS INTEGER) AS PRECISION, \
             CAST(COALESCE(P.CHARACTER_OCTET_LENGTH, P.NUMERIC_PRECISION) AS INTEGER) AS LENGTH, \
             CAST(P.NUMERIC_SCALE AS SMALLINT) AS SCALE, CAST(P.NUMERIC_PRECISION_RADIX AS SMALLINT) AS RADIX, \
             CAST(1 AS SMALLINT) AS NULLABLE, CAST(P.LONG_COMMENT AS VARCHAR(2000)) AS REMARKS, \
             CAST(NULL AS VARCHAR(2000)) AS COLUMN_DEF, {pj} AS SQL_DATA_TYPE, \
             CAST(NULL AS INTEGER) AS SQL_DATETIME_SUB, \
             CAST(P.CHARACTER_OCTET_LENGTH AS INTEGER) AS CHAR_OCTET_LENGTH, \
             CAST(P.ORDINAL_POSITION AS INTEGER) AS ORDINAL_POSITION, \
             CAST('YES' AS VARCHAR(3)) AS IS_NULLABLE, P.SPECIFIC_NAME \
             FROM QSYS2.SYSPARMS P JOIN QSYS2.SYSROUTINES R \
             ON R.SPECIFIC_SCHEMA = P.SPECIFIC_SCHEMA AND R.SPECIFIC_NAME = P.SPECIFIC_NAME \
             WHERE R.ROUTINE_TYPE = 'PROCEDURE' AND {} AND {} AND {} \
             ORDER BY PROCEDURE_SCHEM, PROCEDURE_NAME, P.SPECIFIC_NAME, ORDINAL_POSITION",
            like("P.SPECIFIC_SCHEMA", arg(1).as_deref()),
            like("R.ROUTINE_NAME", arg(2).as_deref()),
            like("COALESCE(P.PARAMETER_NAME, '')", arg(3).as_deref()),
            pj = PARM_JDBC_TYPE,
        ),
        "SQLGETTYPEINFO" => type_info(arg(0).as_deref()),
        "SQLTABLEPRIVILEGES" => none(&[
            "TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "GRANTOR", "GRANTEE", "PRIVILEGE",
            "IS_GRANTABLE",
        ]),
        "SQLCOLPRIVILEGES" => none(&[
            "TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "COLUMN_NAME", "GRANTOR", "GRANTEE",
            "PRIVILEGE", "IS_GRANTABLE",
        ]),
        // The best row identifier (ODBC SQL_BEST_ROWID, 1): the primary
        // key's columns, for the session (scope 2), not pseudo columns
        // (1). Row version columns: none.
        "SQLSPECIALCOLUMNS" if arg(0).as_deref().map(str::trim) == Some("1") => format!(
            "SELECT CAST(2 AS SMALLINT) AS SCOPE, K.COLUMN_NAME, {jdbc} AS DATA_TYPE, \
             {tname} AS TYPE_NAME, CAST({size} AS INTEGER) AS COLUMN_SIZE, \
             CAST(STORAGE AS INTEGER) AS BUFFER_LENGTH, \
             CAST(COALESCE(NUMERIC_SCALE, DATETIME_PRECISION) AS SMALLINT) AS DECIMAL_DIGITS, \
             CAST(1 AS SMALLINT) AS PSEUDO_COLUMN \
             FROM QSYS2.SYSKEYCST K JOIN QSYS2.SYSCST C \
             ON C.CONSTRAINT_SCHEMA = K.CONSTRAINT_SCHEMA AND C.CONSTRAINT_NAME = K.CONSTRAINT_NAME \
             JOIN QSYS2.SYSCOLUMNS L ON L.TABLE_SCHEMA = K.TABLE_SCHEMA \
             AND L.TABLE_NAME = K.TABLE_NAME AND L.COLUMN_NAME = K.COLUMN_NAME \
             WHERE C.CONSTRAINT_TYPE = 'PRIMARY KEY' AND {} AND {} ORDER BY K.ORDINAL_POSITION",
            equal("K.TABLE_SCHEMA", arg(2).as_deref()),
            equal("K.TABLE_NAME", arg(3).as_deref()),
            jdbc = JDBC_TYPE,
            tname = TYPE_NAME,
            size = COLUMN_SIZE,
        ),
        "SQLSPECIALCOLUMNS" => none(&[
            "SCOPE:S", "COLUMN_NAME", "DATA_TYPE:I", "TYPE_NAME", "COLUMN_SIZE:I",
            "BUFFER_LENGTH:I", "DECIMAL_DIGITS:S", "PSEUDO_COLUMN:S",
        ]),
        "SQLUDTS" => none(&[
            "TYPE_CAT", "TYPE_SCHEM", "TYPE_NAME", "CLASS_NAME", "DATA_TYPE:I", "REMARKS",
            "BASE_TYPE:S",
        ]),
        "SQLFUNCTIONCOLS" => none(&[
            "FUNCTION_CAT", "FUNCTION_SCHEM", "FUNCTION_NAME", "COLUMN_NAME", "COLUMN_TYPE:S",
            "DATA_TYPE:I", "TYPE_NAME", "PRECISION:I", "LENGTH:I", "SCALE:S", "RADIX:S",
            "NULLABLE:S", "REMARKS", "CHAR_OCTET_LENGTH:I", "ORDINAL_POSITION:I", "IS_NULLABLE",
            "SPECIFIC_NAME",
        ]),
        "SQLPSEUDOCOLUMNS" => none(&[
            "TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "COLUMN_NAME", "DATA_TYPE:I", "COLUMN_SIZE:I",
            "DECIMAL_DIGITS:I", "NUM_PREC_RADIX:I", "COLUMN_USAGE", "REMARKS",
            "CHAR_OCTET_LENGTH:I", "IS_NULLABLE",
        ]),
        _ => return None,
    })
}

/// A SYSCOLUMNS column's java.sql.Types code.
const JDBC_TYPE: &str = "CAST(CASE DATA_TYPE WHEN 'CHAR' THEN 1 WHEN 'VARCHAR' THEN 12 \
    WHEN 'DECIMAL' THEN 3 WHEN 'NUMERIC' THEN 2 WHEN 'SMALLINT' THEN 5 WHEN 'INTEGER' THEN 4 \
    WHEN 'BIGINT' THEN -5 WHEN 'FLOAT' THEN CASE WHEN LENGTH = 4 THEN 7 ELSE 8 END \
    WHEN 'REAL' THEN 7 WHEN 'DOUBLE' THEN 8 WHEN 'DATE' THEN 91 WHEN 'TIME' THEN 92 \
    WHEN 'TIMESTMP' THEN 93 WHEN 'BINARY' THEN -2 WHEN 'VARBIN' THEN -3 WHEN 'GRAPHIC' THEN -15 \
    WHEN 'VARG' THEN -9 WHEN 'BLOB' THEN 2004 WHEN 'CLOB' THEN 2005 WHEN 'DBCLOB' THEN 2011 \
    WHEN 'XML' THEN 2009 WHEN 'BOOLEAN' THEN 16 WHEN 'ROWID' THEN -8 WHEN 'DATALINK' THEN 70 \
    ELSE 1111 END AS INTEGER)";

/// A SYSCOLUMNS column's type as SQL names it.
const TYPE_NAME: &str = "CAST(CASE DATA_TYPE WHEN 'TIMESTMP' THEN 'TIMESTAMP' \
    WHEN 'VARBIN' THEN 'VARBINARY' WHEN 'VARG' THEN 'VARGRAPHIC' \
    WHEN 'FLOAT' THEN CASE WHEN LENGTH = 4 THEN 'REAL' ELSE 'DOUBLE' END \
    ELSE DATA_TYPE END AS VARCHAR(128))";

/// A SYSCOLUMNS column's size as JDBC means it: precision of a number,
/// characters of a date, time or timestamp, else its length.
const COLUMN_SIZE: &str = "CASE WHEN NUMERIC_PRECISION IS NOT NULL THEN NUMERIC_PRECISION \
    WHEN DATA_TYPE = 'DATE' THEN 10 WHEN DATA_TYPE = 'TIME' THEN 8 \
    WHEN DATA_TYPE = 'TIMESTMP' THEN CASE WHEN COALESCE(DATETIME_PRECISION, 6) > 0 \
    THEN 20 + COALESCE(DATETIME_PRECISION, 6) ELSE 19 END ELSE LENGTH END";

/// A SYSPARMS parameter's java.sql.Types code (its type as written).
const PARM_JDBC_TYPE: &str = "CAST(CASE WHEN P.DATA_TYPE IN ('CHAR', 'CHARACTER') THEN 1 \
    WHEN P.DATA_TYPE IN ('VARCHAR', 'CHARACTER VARYING', 'CHAR VARYING') THEN 12 \
    WHEN P.DATA_TYPE IN ('DEC', 'DECIMAL') THEN 3 WHEN P.DATA_TYPE IN ('NUM', 'NUMERIC') THEN 2 \
    WHEN P.DATA_TYPE = 'SMALLINT' THEN 5 WHEN P.DATA_TYPE IN ('INT', 'INTEGER') THEN 4 \
    WHEN P.DATA_TYPE = 'BIGINT' THEN -5 WHEN P.DATA_TYPE = 'REAL' THEN 7 \
    WHEN P.DATA_TYPE IN ('DOUBLE', 'FLOAT', 'DOUBLE PRECISION') THEN 8 \
    WHEN P.DATA_TYPE = 'DATE' THEN 91 WHEN P.DATA_TYPE = 'TIME' THEN 92 \
    WHEN P.DATA_TYPE = 'TIMESTAMP' THEN 93 WHEN P.DATA_TYPE = 'BINARY' THEN -2 \
    WHEN P.DATA_TYPE = 'VARBINARY' THEN -3 WHEN P.DATA_TYPE = 'BLOB' THEN 2004 \
    WHEN P.DATA_TYPE = 'CLOB' THEN 2005 WHEN P.DATA_TYPE = 'XML' THEN 2009 \
    WHEN P.DATA_TYPE = 'BOOLEAN' THEN 16 ELSE 1111 END AS INTEGER)";

/// The types (JDBC getTypeInfo): name, java.sql.Types code, precision,
/// literal prefix and suffix, create parameters, minimum and maximum
/// scale, radix.
#[allow(clippy::type_complexity)]
fn type_info(only: Option<&str>) -> String {
    #[rustfmt::skip]
    const TYPES: &[(&str, i32, i32, &str, &str, &str, i32, i32, i32)] = &[
        ("BIGINT", -5, 19, "", "", "", 0, 0, 10),
        ("BINARY", -2, 32765, "X'", "'", "LENGTH", 0, 0, 0),
        ("BLOB", 2004, 2147483647, "", "", "LENGTH", 0, 0, 0),
        ("BOOLEAN", 16, 1, "", "", "", 0, 0, 0),
        ("CHAR", 1, 32765, "'", "'", "LENGTH", 0, 0, 0),
        ("CLOB", 2005, 2147483647, "'", "'", "LENGTH", 0, 0, 0),
        ("DATE", 91, 10, "'", "'", "", 0, 0, 0),
        ("DECFLOAT", 1111, 34, "", "", "PRECISION", 0, 0, 10),
        ("DECIMAL", 3, 63, "", "", "PRECISION,SCALE", 0, 63, 10),
        ("DOUBLE", 8, 53, "", "", "", 0, 0, 2),
        ("GRAPHIC", -15, 16382, "G'", "'", "LENGTH", 0, 0, 0),
        ("INTEGER", 4, 10, "", "", "", 0, 0, 10),
        ("NUMERIC", 2, 63, "", "", "PRECISION,SCALE", 0, 63, 10),
        ("REAL", 7, 24, "", "", "", 0, 0, 2),
        ("SMALLINT", 5, 5, "", "", "", 0, 0, 10),
        ("TIME", 92, 8, "'", "'", "", 0, 0, 0),
        ("TIMESTAMP", 93, 32, "'", "'", "", 0, 12, 0),
        ("VARBINARY", -3, 32739, "X'", "'", "LENGTH", 0, 0, 0),
        ("VARCHAR", 12, 32739, "'", "'", "LENGTH", 0, 0, 0),
        ("VARGRAPHIC", -9, 16369, "G'", "'", "LENGTH", 0, 0, 0),
        ("XML", 2009, 2147483647, "", "", "", 0, 0, 0),
    ];
    // ODBC SQL_ALL_TYPES is 0.
    let only: Option<i32> = only.and_then(|t| t.trim().parse().ok()).filter(|t| *t != 0);
    let rows: Vec<String> =
        TYPES
            .iter()
            .filter(|t| only.is_none_or(|o| o == t.1))
            .map(|(name, code, prec, pre, suf, create, min, max, radix)| {
                let opt = |s: &str| {
                    if s.is_empty() {
                        "NULL".to_string()
                    } else {
                        lit(s)
                    }
                };
                let radix = if *radix == 0 {
                    "NULL".to_string()
                } else {
                    radix.to_string()
                };
                let searchable = if matches!(*code, 2004 | 2005 | 2009) {
                    0
                } else {
                    3
                };
                let fixed = if matches!(*code, 3 | 2) { 1 } else { 0 };
                let case = if matches!(*code, 1 | 12 | 2005 | -15 | -9) {
                    1
                } else {
                    0
                };
                format!(
                "({}, {code}, {prec}, {}, {}, {}, 1, {case}, {searchable}, {}, {fixed}, 0, {}, \
                 {min}, {max}, {code}, NULL, {radix})",
                lit(name),
                opt(pre),
                opt(suf),
                opt(create),
                if matches!(*code, -5 | 2 | 3 | 4 | 5 | 7 | 8) { "0" } else { "NULL" },
                lit(name),
            )
            })
            .collect();
    if rows.is_empty() {
        return none(&[
            "TYPE_NAME",
            "DATA_TYPE:I",
            "PRECISION:I",
            "LITERAL_PREFIX",
            "LITERAL_SUFFIX",
            "CREATE_PARAMS",
            "NULLABLE:S",
            "CASE_SENSITIVE:S",
            "SEARCHABLE:S",
            "UNSIGNED_ATTRIBUTE:S",
            "FIXED_PREC_SCALE:S",
            "AUTO_INCREMENT:S",
            "LOCAL_TYPE_NAME",
            "MINIMUM_SCALE:S",
            "MAXIMUM_SCALE:S",
            "SQL_DATA_TYPE:I",
            "SQL_DATETIME_SUB:I",
            "NUM_PREC_RADIX:I",
        ]);
    }
    format!(
        "SELECT CAST(A AS VARCHAR(128)) AS TYPE_NAME, CAST(B AS INTEGER) AS DATA_TYPE, \
         CAST(C AS INTEGER) AS PRECISION, CAST(D AS VARCHAR(128)) AS LITERAL_PREFIX, \
         CAST(E AS VARCHAR(128)) AS LITERAL_SUFFIX, CAST(F AS VARCHAR(128)) AS CREATE_PARAMS, \
         CAST(G AS SMALLINT) AS NULLABLE, CAST(H AS SMALLINT) AS CASE_SENSITIVE, \
         CAST(I AS SMALLINT) AS SEARCHABLE, CAST(J AS SMALLINT) AS UNSIGNED_ATTRIBUTE, \
         CAST(K AS SMALLINT) AS FIXED_PREC_SCALE, CAST(L AS SMALLINT) AS AUTO_INCREMENT, \
         CAST(M AS VARCHAR(128)) AS LOCAL_TYPE_NAME, CAST(N AS SMALLINT) AS MINIMUM_SCALE, \
         CAST(O AS SMALLINT) AS MAXIMUM_SCALE, CAST(P AS INTEGER) AS SQL_DATA_TYPE, \
         CAST(Q AS INTEGER) AS SQL_DATETIME_SUB, CAST(R AS INTEGER) AS NUM_PREC_RADIX \
         FROM (VALUES {}) AS T(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R) \
         ORDER BY DATA_TYPE, TYPE_NAME",
        rows.join(", ")
    )
}

/// A result set of no rows with these columns (`NAME:I` INTEGER,
/// `NAME:S` SMALLINT, else VARCHAR(128)).
fn none(cols: &[&str]) -> String {
    let list: Vec<String> = cols
        .iter()
        .map(|c| match c.split_once(':') {
            Some((n, "I")) => format!("CAST(NULL AS INTEGER) AS {n}"),
            Some((n, _)) => format!("CAST(NULL AS SMALLINT) AS {n}"),
            None => format!("CAST(NULL AS VARCHAR(128)) AS {c}"),
        })
        .collect();
    format!(
        "SELECT {} FROM SYSIBM.SYSDUMMY1 WHERE 1 = 0",
        list.join(", ")
    )
}

/// A literal argument's value: None for NULL, a string's text.
fn value(a: &str) -> Option<String> {
    let a = a.trim();
    if a.eq_ignore_ascii_case("NULL") {
        return None;
    }
    match a.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        Some(s) => Some(s.replace("''", "'")),
        None => Some(a.to_string()),
    }
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A name matched by a LIKE pattern (`\` escapes `%` and `_`); no
/// pattern, an empty one or `%` match all.
fn like(col: &str, pat: Option<&str>) -> String {
    match pat {
        None | Some("") | Some("%") => "1 = 1".into(),
        Some(p) if !p.contains(['%', '_']) => format!("{col} = {}", lit(p)),
        Some(p) => format!("{col} LIKE {} ESCAPE '\\'", lit(p)),
    }
}

/// A name equal to this one; none or an empty one match all.
fn equal(col: &str, v: Option<&str>) -> String {
    match v {
        None | Some("") => "1 = 1".into(),
        Some(v) => format!("{col} = {}", lit(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_answered_and_their_arguments() {
        assert!(query("QSYS2.QCMDEXC", &[]).is_none());
        assert!(query("SYSIBM.NOSUCH", &[]).is_none());
        let q = query(
            "sysibm.SQLTABLES",
            &[
                "'%'",
                "'%'",
                "'%'",
                "'%'",
                "'DATATYPE=''JDBC'';GETSCHEMAS=1;CURSORHOLD=1'",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(
            q.contains("TABLE_SCHEM") && q.contains("SYSSCHEMAS") && q.contains("1 = 1"),
            "{q}"
        );
        // GETSCHEMAS=2: getSchemas(catalog, schemaPattern).
        let q = query(
            "SYSIBM.SQLTABLES",
            &["NULL", "'Q%'", "'%'", "'%'", "'GETSCHEMAS=2'"].map(String::from),
        )
        .unwrap();
        assert!(q.contains("SCHEMA_NAME LIKE 'Q%'"), "{q}");
        let q = query(
            "SYSIBM.SQLTABLES",
            &[
                "NULL",
                "'QGPL'",
                "'A\\_B%'",
                "'TABLE,VIEW'",
                "'DATATYPE=''JDBC'''",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(q.contains("TABLE_SCHEMA = 'QGPL'"), "{q}");
        assert!(q.contains("TABLE_NAME LIKE 'A\\_B%' ESCAPE '\\'"), "{q}");
        assert!(q.contains("TABLE_TYPE IN ('TABLE', 'VIEW')"), "{q}");
        let q = query("SYSIBM.SQLGETTYPEINFO", &["4", "''"].map(String::from)).unwrap();
        assert!(
            q.contains("('INTEGER', 4, 10") && !q.contains("'BIGINT'"),
            "{q}"
        );
    }
}
