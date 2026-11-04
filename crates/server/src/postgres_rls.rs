//! Owner-checked native RLS actions with bounded expression parsing.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sha2::{Digest, Sha256};
use sift_protocol::{
    ConnectionId, ExecuteRequestHttp, OperationKind, PostgresObjectAction as Action,
    PostgresObjectPreview, PostgresPolicyCommand as Command, PostgresPolicyRole as Role,
    PostgresRowSecuritySetting as Setting, SessionId, Value,
};
use sqlparser::{
    ast::{Expr, Visit, Visitor},
    dialect::PostgreSqlDialect,
    parser::Parser,
    tokenizer::Token,
};
use std::ops::ControlFlow;

const STATE: &str = include_str!("postgres_rls/state.sql");
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn quote(name: &str) -> ApiResult<String> {
    Ok(crate::postgres_workbench::quote(
        crate::postgres_workbench::identifier(name)?,
    ))
}

pub(crate) fn handles(action: &Action) -> bool {
    matches!(
        action,
        Action::CreatePolicy { .. }
            | Action::AlterPolicy { .. }
            | Action::DropPolicy { .. }
            | Action::SetRowSecurity { .. }
    )
}
fn expression(input: &str) -> ApiResult<String> {
    if input.trim().is_empty() || input.len() > 2048 || input.contains('\0') {
        return Err(bad(
            "Policy expressions must contain 1..=2048 bytes without NUL",
        ));
    }
    let mut parser = Parser::new(&PostgreSqlDialect {})
        .with_recursion_limit(64)
        .try_with_sql(input)
        .map_err(|_| bad("Unsupported policy expression syntax"))?;
    let expr = parser
        .parse_expr()
        .map_err(|_| bad("Unsupported policy expression syntax"))?;
    if parser.peek_token().token != Token::EOF {
        return Err(bad(
            "Provide exactly one policy expression, without trailing SQL",
        ));
    }
    struct Binds;
    impl Visitor for Binds {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::Value(sqlparser::ast::Value::Placeholder(_))) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    if expr.visit(&mut Binds).is_break() {
        return Err(bad("Policy definitions cannot contain bind parameters"));
    }
    Ok(expr.to_string())
}
fn roles(values: &[Role]) -> ApiResult<(String, Vec<String>)> {
    if values.is_empty() || values.len() > 32 {
        return Err(bad(
            "Choose 1..=32 policy roles, including explicit PUBLIC when intended",
        ));
    }
    let mut names = Vec::new();
    let mut sql = Vec::new();
    for role in values {
        match role {
            Role::Public => sql.push("PUBLIC".into()),
            Role::Named(name) => {
                sql.push(quote(name)?);
                names.push(name.clone());
            }
        }
    }
    Ok((sql.join(", "), names))
}
fn conditions(
    command: Command,
    using: &Option<String>,
    check: &Option<String>,
) -> ApiResult<String> {
    if command == Command::Insert && using.is_some() {
        return Err(bad("INSERT policies accept WITH CHECK, not USING"));
    }
    if matches!(command, Command::Select | Command::Delete) && check.is_some() {
        return Err(bad("SELECT/DELETE policies accept USING, not WITH CHECK"));
    }
    let mut sql = String::new();
    if let Some(value) = using {
        sql.push_str(&format!(" USING ({})", expression(value)?));
    }
    if let Some(value) = check {
        sql.push_str(&format!(" WITH CHECK ({})", expression(value)?));
    }
    Ok(sql)
}
fn command(value: Command) -> &'static str {
    match value {
        Command::All => "ALL",
        Command::Select => "SELECT",
        Command::Insert => "INSERT",
        Command::Update => "UPDATE",
        Command::Delete => "DELETE",
    }
}

pub(crate) async fn preview(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    action: Action,
) -> ApiResult<PostgresObjectPreview> {
    let (schema, table, name) = match &action {
        Action::CreatePolicy {
            schema,
            table,
            name,
            ..
        }
        | Action::AlterPolicy {
            schema,
            table,
            name,
            ..
        }
        | Action::DropPolicy {
            schema,
            table,
            name,
        } => (schema, table, name.as_str()),
        Action::SetRowSecurity { schema, table, .. } => (schema, table, ""),
        _ => unreachable!("RLS action routing"),
    };
    let relation = format!("{}.{}", quote(schema)?, quote(table)?);
    if !matches!(&action, Action::SetRowSecurity { .. }) {
        quote(name)?;
    }
    let named_roles = match &action {
        Action::CreatePolicy { definition, .. } => roles(&definition.roles)?.1,
        Action::AlterPolicy {
            roles: Some(values),
            ..
        } => roles(values)?.1,
        _ => vec![],
    };
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: STATE.into(),
                params: vec![
                    Value::Text(schema.clone()),
                    Value::Text(table.clone()),
                    Value::Text(name.into()),
                    Value::Text(
                        serde_json::to_string(&named_roles)
                            .map_err(|_| bad("Invalid role list"))?,
                    ),
                ],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::PreviewPostgresObject,
        )
        .await?;
    let state = response
        .rows
        .first()
        .map(|row| row.values.as_slice())
        .ok_or_else(|| bad("Select an existing ordinary or partitioned table"))?;
    let [Value::Bool(owned), Value::Bool(exists), Value::Bool(roles_exist), Value::Text(_), native_command, Value::Bool(standard_strings)] =
        state
    else {
        return Err(bad("Invalid policy catalog response"));
    };
    if !owned {
        return Err(ApiError::Forbidden(
            "PostgreSQL table ownership is required".into(),
        ));
    }
    if !roles_exist {
        return Err(bad("A selected policy role does not exist"));
    }
    // Canonical SQL string literals assume native standard string interpretation.
    // Refuse the legacy backslash mode rather than reinterpret a condition at apply.
    if !standard_strings
        && matches!(
            &action,
            Action::CreatePolicy { .. } | Action::AlterPolicy { .. }
        )
    {
        return Err(bad(
            "Policy editing requires standard_conforming_strings=on",
        ));
    }
    let sql = match &action {
        Action::CreatePolicy { definition, .. } => {
            if *exists {
                return Err(bad("Policy already exists"));
            }
            let role_sql = roles(&definition.roles)?.0;
            format!(
                "CREATE POLICY {} ON {relation} AS {} FOR {} TO {role_sql}{}",
                quote(name)?,
                if definition.permissive {
                    "PERMISSIVE"
                } else {
                    "RESTRICTIVE"
                },
                command(definition.command),
                conditions(
                    definition.command,
                    &definition.using_expression,
                    &definition.check_expression
                )?
            )
        }
        Action::AlterPolicy {
            roles: values,
            using_expression,
            check_expression,
            ..
        } => {
            if !exists {
                return Err(bad("Policy does not exist"));
            }
            if values.is_none() && using_expression.is_none() && check_expression.is_none() {
                return Err(bad("Choose policy roles or conditions to alter"));
            }
            let native = match native_command {
                Value::Text(value) => value.as_str(),
                _ => return Err(bad("Invalid native policy command")),
            };
            let cmd = match native {
                "*" => Command::All,
                "r" => Command::Select,
                "a" => Command::Insert,
                "w" => Command::Update,
                "d" => Command::Delete,
                _ => return Err(bad("Unsupported native policy command")),
            };
            let role_sql = values
                .as_ref()
                .map(|values| roles(values).map(|(sql, _)| format!(" TO {sql}")))
                .transpose()?
                .unwrap_or_default();
            format!(
                "ALTER POLICY {} ON {relation}{role_sql}{}",
                quote(name)?,
                conditions(cmd, using_expression, check_expression)?
            )
        }
        Action::DropPolicy { .. } => {
            if !exists {
                return Err(bad("Policy does not exist"));
            }
            format!("DROP POLICY {} ON {relation} RESTRICT", quote(name)?)
        }
        Action::SetRowSecurity { setting, .. } => format!(
            "ALTER TABLE {relation} {} ROW LEVEL SECURITY",
            match setting {
                Setting::Enable => "ENABLE",
                Setting::Disable => "DISABLE",
                Setting::Force => "FORCE",
                Setting::NoForce => "NO FORCE",
            }
        ),
        _ => unreachable!(),
    };
    let mut digest = Sha256::new();
    digest.update(sql.as_bytes());
    digest.update(serde_json::to_vec(state).map_err(|_| bad("Policy fingerprint failed"))?);
    Ok(PostgresObjectPreview { action, sql, precondition: format!("{:x}", digest.finalize()), warning: "RLS changes affect future row visibility and writes. Missing CREATE conditions allow all rows; restrictive policies also need a permissive policy. Dropping restrictive policies or disabling RLS can broaden access; enabled RLS without applicable policies denies rows. Owners normally bypass RLS unless forced; superusers/BYPASSRLS still bypass it. Native PostgreSQL validates expression types and privileges at apply; preview holds no locks.".into(), impact: None })
}

pub(crate) fn redact(action: &mut Action) {
    match action {
        Action::CreatePolicy { definition, .. } => {
            definition.using_expression = None;
            definition.check_expression = None;
        }
        Action::AlterPolicy {
            using_expression,
            check_expression,
            ..
        } => {
            *using_expression = None;
            *check_expression = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_expression_is_one_bounded_expression_without_binds() {
        assert!(expression("true); DROP TABLE protected; --").is_err());
        assert!(expression("true; SELECT 1").is_err());
        assert!(expression("EXISTS (SELECT 1 WHERE $1 = 1)").is_err());
        assert!(expression(&"(".repeat(100)).is_err());
        assert!(expression(&"x".repeat(2049)).is_err());
        assert!(
            expression("owner_name = CURRENT_USER AND note <> 'x''; DROP TABLE t; --'")
                .unwrap()
                .contains("DROP TABLE t")
        );
        assert!(expression("true /* ; DROP TABLE t */")
            .unwrap()
            .eq_ignore_ascii_case("true"));
    }
    #[test]
    fn policy_roles_and_command_conditions_are_explicit() {
        let (sql, names) = roles(&[Role::Public, Role::Named("r\"ole".into())]).unwrap();
        assert_eq!(sql, "PUBLIC, \"r\"\"ole\"");
        assert_eq!(names, vec!["r\"ole"]);
        assert!(roles(&[]).is_err());
        assert!(conditions(Command::Insert, &Some("true".into()), &None).is_err());
        assert!(conditions(Command::Select, &None, &Some("true".into())).is_err());
        assert!(conditions(Command::Delete, &None, &Some("true".into())).is_err());
        assert!(conditions(Command::Update, &Some("true".into()), &Some("false".into())).is_ok());
    }
    #[test]
    fn audit_projection_removes_policy_expression_bodies() {
        let mut action = Action::CreatePolicy {
            schema: "public".into(),
            table: "t".into(),
            name: "p".into(),
            definition: sift_protocol::PostgresPolicyDefinition {
                command: Command::All,
                permissive: true,
                roles: vec![Role::Public],
                using_expression: Some("private_value = 'secret'".into()),
                check_expression: Some("other_secret".into()),
            },
        };
        redact(&mut action);
        let encoded = serde_json::to_string(&action).unwrap();
        assert!(!encoded.contains("private_value"));
        assert!(!encoded.contains("secret"));
    }
}
