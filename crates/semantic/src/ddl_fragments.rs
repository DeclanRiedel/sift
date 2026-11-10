//! Complete native grammar fragments for typed migration preparation.
//! Passing this check bounds command shape, never function/trigger/native effects.
use sift_protocol::{ConstraintKind, Engine};
use sqlparser::{
    ast::TableConstraint,
    dialect::{Dialect, MsSqlDialect, PostgreSqlDialect, SQLiteDialect},
    keywords::Keyword,
    parser::{Parser, ParserError},
    tokenizer::{Token, Tokenizer, Whitespace},
};

#[derive(Debug, thiserror::Error)]
#[error("Migration fragment is not a complete supported native grammar construct")]
pub struct FragmentError;

fn complete<T>(
    engine: Engine,
    text: &str,
    parse: impl FnOnce(&mut Parser<'_>) -> Result<T, ParserError>,
) -> Result<T, FragmentError> {
    if text.is_empty() || text.len() > 65536 {
        return Err(FragmentError);
    }
    let dialect: Box<dyn Dialect> = match engine {
        Engine::Postgres => Box::new(PostgreSqlDialect {}),
        Engine::SqlServer => Box::new(MsSqlDialect {}),
        Engine::Sqlite => Box::new(SQLiteDialect {}),
    };
    // Comments must not absorb the renderer's suffix. Strings/quoted identifiers
    // containing comment-like bytes remain ordinary grammar tokens.
    let tokens = Tokenizer::new(dialect.as_ref(), text)
        .tokenize()
        .map_err(|_| FragmentError)?;
    if tokens.iter().any(|t| {
        matches!(
            t,
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)
            )
        )
    }) {
        return Err(FragmentError);
    }
    let mut parser = Parser::new(dialect.as_ref())
        .with_recursion_limit(64)
        .try_with_sql(text)
        .map_err(|_| FragmentError)?;
    let value = parse(&mut parser).map_err(|_| FragmentError)?;
    if parser.peek_token().token != Token::EOF {
        return Err(FragmentError);
    }
    Ok(value)
}

/// One expression, without trailing commands, operations or absorbing comments.
pub fn expression(engine: Engine, text: &str) -> Result<(), FragmentError> {
    complete(engine, text, |p| p.parse_expr())
        .map(|_| ())
        .or_else(|error| {
            if engine != Engine::SqlServer {
                return Err(error);
            }
            // SQL Server's native sequence default is not an Expr in sqlparser.
            complete(engine, text, |p| {
                if !p.parse_keywords(&[Keyword::NEXT, Keyword::VALUE, Keyword::FOR]) {
                    return Err(ParserError::ParserError("Expected sequence default".into()));
                }
                p.parse_object_name(false).map(|_| ())
            })
        })
}
/// One native type; qualifiers that introduce another column/operation refuse.
pub fn data_type(engine: Engine, text: &str) -> Result<(), FragmentError> {
    complete(engine, text, |p| p.parse_data_type()).map(|_| ())
}
/// One unnamed constraint body of the declared kind. The renderer supplies its
/// separately quoted name. Native NOT VALID is supported for CHECK and FK.
pub fn constraint(engine: Engine, text: &str, kind: ConstraintKind) -> Result<(), FragmentError> {
    let parsed = complete(engine, text, |p| {
        let parsed = p.parse_optional_table_constraint()?;
        if engine == Engine::Postgres
            && matches!(kind, ConstraintKind::Check | ConstraintKind::ForeignKey)
        {
            let _ = p.parse_keywords(&[Keyword::NOT, Keyword::VALID]);
        }
        Ok(parsed)
    })?
    .ok_or(FragmentError)?;
    let valid = matches!(
        (parsed, kind),
        (
            TableConstraint::PrimaryKey { name: None, .. },
            ConstraintKind::PrimaryKey
        ) | (
            TableConstraint::Unique { name: None, .. },
            ConstraintKind::Unique
        ) | (
            TableConstraint::ForeignKey { name: None, .. },
            ConstraintKind::ForeignKey
        ) | (
            TableConstraint::Check { name: None, .. },
            ConstraintKind::Check
        )
    );
    if valid {
        Ok(())
    } else {
        Err(FragmentError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragments_cannot_escape_into_another_command_or_operation() {
        for engine in [Engine::Postgres, Engine::SqlServer, Engine::Sqlite] {
            for bad in [
                "0; DROP TABLE secrets",
                "0, ADD COLUMN injected INTEGER",
                "0 -- absorb suffix",
                "0 /* absorb suffix */",
            ] {
                assert!(expression(engine, bad).is_err(), "{engine:?}: {bad}");
            }
            for bad in [
                "INTEGER); DROP TABLE secrets; --",
                "INTEGER, injected TEXT",
                "INTEGER /* absorb suffix */",
            ] {
                assert!(data_type(engine, bad).is_err(), "{engine:?}: {bad}");
            }
            for bad in [
                "CHECK (id > 0); DROP TABLE secrets",
                "CHECK (id > 0), ADD COLUMN injected INTEGER",
                "CONSTRAINT other CHECK (id > 0)",
                "CHECK (id > 0) -- suffix",
            ] {
                assert!(
                    constraint(engine, bad, ConstraintKind::Check).is_err(),
                    "{engine:?}: {bad}"
                );
            }
            assert!(constraint(engine, "UNIQUE (id)", ConstraintKind::Check).is_err());
            assert!(constraint(engine, "CHECK (id > 0)", ConstraintKind::Check).is_ok());
            assert!(expression(engine, "1 + 2").is_ok());
            assert!(expression(engine, "'literal -- /* ; , ADD' ").is_ok());
        }
        assert!(data_type(Engine::Postgres, "numeric(12, 3)").is_ok());
        assert!(data_type(Engine::SqlServer, "nvarchar(max)").is_ok());
        assert!(data_type(Engine::Postgres, "timestamp(6) without time zone").is_ok());
        assert!(expression(Engine::Postgres, "'value'::text").is_ok());
        assert!(expression(Engine::SqlServer, "NEXT VALUE FOR [dbo].[sequence]").is_ok());
        assert!(expression(
            Engine::SqlServer,
            "NEXT VALUE FOR [dbo].[sequence]; DROP TABLE secrets"
        )
        .is_err());
        assert!(constraint(
            Engine::Postgres,
            "CHECK (id > 0) NOT VALID",
            ConstraintKind::Check
        )
        .is_ok());
        assert!(expression(Engine::Postgres, &"x".repeat(65537)).is_err());
    }
}
