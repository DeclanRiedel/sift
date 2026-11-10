use super::*;
use serde_json::json;

fn policy(action: serde_json::Value) -> AiDelegationPolicy {
    serde_json::from_value(json!({
        "budget":{"max_tool_calls":5,"max_input_bytes":4096,"max_output_bytes":8192,"max_duration_secs":600},
        "rules":[{"id":Uuid::from_u128(1),"max_uses":3,"action":action}]
    })).unwrap()
}

fn claim(
    policy: &AiDelegationPolicy,
    usage: &DelegationUsage,
    action: PreparedDelegationAction<'_>,
) -> Result<DelegationDebit, DelegationError> {
    let reserved = reserve_tool_call(policy, usage, 128, 256)?;
    reserve_action(policy, &reserved, action, ArbitrarySqlPolicy::default())
}

#[test]
fn exact_sql_and_explicit_arbitrary_sql_keep_separate_authority() {
    let p = policy(json!({"kind":"exact_sql","sql":"DELETE FROM items WHERE id = 4","params":[]}));
    let sql = "DELETE FROM items WHERE id = 4";
    let used = claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Sql { sql, params: &[] },
    )
    .unwrap();
    assert_eq!(used.after.rules[&p.rules[0].id].uses, 1);
    assert_eq!(
        claim(
            &p,
            &DelegationUsage::default(),
            PreparedDelegationAction::Sql {
                sql,
                params: &[Value::Int32(4)]
            }
        ),
        Err(DelegationError::NoMatchingRule)
    );
    assert_eq!(
        claim(
            &p,
            &DelegationUsage::default(),
            PreparedDelegationAction::Sql {
                sql: "DELETE FROM items",
                params: &[]
            }
        ),
        Err(DelegationError::NoMatchingRule)
    );
    let broad = policy(json!({"kind":"arbitrary_sql","acknowledge_unbounded_effects":true}));
    for (instance_enabled, tenant_enabled, permitted) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, true),
    ] {
        let reserved = reserve_tool_call(&broad, &DelegationUsage::default(), 1, 1).unwrap();
        let result = reserve_action(
            &broad,
            &reserved,
            PreparedDelegationAction::Sql { sql, params: &[] },
            ArbitrarySqlPolicy {
                instance_enabled,
                tenant_enabled,
            },
        );
        assert_eq!(result.is_ok(), permitted);
    }
    let unreviewed = policy(json!({"kind":"arbitrary_sql","acknowledge_unbounded_effects":false}));
    assert_eq!(
        validate_policy(&unreviewed),
        Err(DelegationError::InvalidPolicy)
    );
}

#[test]
fn global_reservations_include_reads_and_output_and_cannot_overflow() {
    let p = policy(json!({"kind":"exact_sql","sql":"SELECT 1","params":[]}));
    let zero = DelegationUsage::default();
    let read = reserve_tool_call(&p, &zero, 2048, 4096).unwrap();
    let full = reserve_tool_call(&p, &read, 2048, 4096).unwrap();
    assert_eq!(full.tool_calls, 2);
    assert_eq!(
        reserve_tool_call(&p, &full, 1, 1),
        Err(DelegationError::BudgetExceeded)
    );
    for usage in [
        DelegationUsage {
            tool_calls: u32::MAX,
            ..zero.clone()
        },
        DelegationUsage {
            input_bytes: u64::MAX,
            ..zero.clone()
        },
        DelegationUsage {
            output_bytes: u64::MAX,
            ..zero.clone()
        },
    ] {
        assert_eq!(
            reserve_tool_call(&p, &usage, 1, 1),
            Err(DelegationError::BudgetExceeded)
        );
    }
    assert_eq!(zero, DelegationUsage::default());
}

#[test]
fn row_rules_bound_submitted_edits_across_calls_and_actual_table_and_columns() {
    let p = policy(
        json!({"kind":"row_edits","table":{"schema":"public","name":"items"},"operations":["update"],"write_columns":["label"],"max_submitted_rows":2,"max_total_submitted_rows":3}),
    );
    let edit = RowEdit::Update {
        key: RowKey {
            columns: vec![CellEdit {
                column: "id".into(),
                value: Value::Int32(1),
            }],
        },
        changes: vec![CellEdit {
            column: "label".into(),
            value: Value::Text("new".into()),
        }],
        expected: vec![],
    };
    let table = ObjectPath {
        schema: Some("public".into()),
        ..ObjectPath::new("items")
    };
    let mut edits = EditSet {
        table,
        edits: vec![edit.clone(), edit.clone()],
    };
    let exact = policy(json!({"kind":"exact_row_edits","edit_set":edits}));
    assert!(claim(
        &exact,
        &DelegationUsage::default(),
        PreparedDelegationAction::RowEdits(&edits)
    )
    .is_ok());
    let first = claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::RowEdits(&edits),
    )
    .unwrap();
    assert_eq!(first.after.rules[&p.rules[0].id].submitted_rows, 2);
    assert_eq!(
        claim(&p, &first.after, PreparedDelegationAction::RowEdits(&edits)),
        Err(DelegationError::BudgetExceeded)
    );
    edits.edits.pop();
    assert!(claim(
        &exact,
        &DelegationUsage::default(),
        PreparedDelegationAction::RowEdits(&edits)
    )
    .is_err());
    assert_eq!(
        claim(&p, &first.after, PreparedDelegationAction::RowEdits(&edits))
            .unwrap()
            .after
            .rules[&p.rules[0].id]
            .submitted_rows,
        3
    );
    edits.table.schema = Some("other".into());
    assert_eq!(
        claim(
            &p,
            &DelegationUsage::default(),
            PreparedDelegationAction::RowEdits(&edits)
        ),
        Err(DelegationError::NoMatchingRule)
    );
    edits.table.schema = Some("public".into());
    edits.edits = vec![RowEdit::Delete {
        key: RowKey { columns: vec![] },
        expected: vec![],
    }];
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::RowEdits(&edits)
    )
    .is_err());
    let mut other_column = edit;
    if let RowEdit::Update { changes, .. } = &mut other_column {
        changes[0].column = "privileged".into();
    }
    edits.edits = vec![other_column];
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::RowEdits(&edits)
    )
    .is_err());
}

#[test]
fn query_rules_bind_original_target_revision_and_before_hash() {
    let target = AiQueryExecutionTarget::PrivateEditor {
        desktop_id: Uuid::from_u128(7),
        item_id: 9,
    };
    let before = "a".repeat(64);
    let p = policy(
        json!({"kind":"query_edit","target":target,"before_sha256":before,"base_revision":4,"replacement":{"kind":"bounded","max_utf8_bytes":4}}),
    );
    let action = |revision| PreparedDelegationAction::QueryEdit {
        target: &target,
        before_sha256: &before,
        base_revision: revision,
        replacement: "éé",
    };
    assert!(claim(&p, &DelegationUsage::default(), action(4)).is_ok());
    assert!(claim(&p, &DelegationUsage::default(), action(5)).is_err());
    let room = AiQueryExecutionTarget::RoomDocument {
        room_id: 7,
        document_id: 9,
    };
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::QueryEdit {
            target: &room,
            before_sha256: &before,
            base_revision: 4,
            replacement: "abc"
        }
    )
    .is_err());
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::QueryEdit {
            target: &target,
            before_sha256: &before,
            base_revision: 4,
            replacement: "ééé"
        }
    )
    .is_err());
}

fn extension_review() -> AiExtensionExecutionReview {
    serde_json::from_value(json!({
        "tool":{"id":"fixture","title":"fixture","description":"fixture","operation":{
            "extension_id":"acme/test","contribution_id":"acme/test/database_provider/fixture","action":"run","classification":"write","target_kind":"tenant","target_id":"1","sanitized_arguments":{}
        },"input_schema":{"type":"object"},"output_schema":{},"required_context":[],"mcp_exposable":true,"schedulable":true,"interactive":false},
        "context":{"tenant_id":1,"profile_id":2},"arguments":{"labels":["safe"],"limit":2},
        "prepared_contract":{"package":"first","schema":{"type":"object"}},
        "source":{"tenant_id":1,"profile_id":2,"session":2,"connection":3,"generation":Uuid::from_u128(4),"profile_digest":"digest","database_label":"fixture","production":false,"engine":"postgres"}
    })).unwrap()
}

#[test]
fn extension_rules_freeze_contract_context_and_bound_nested_arguments() {
    let mut review = extension_review();
    let p = policy(
        json!({"kind":"extension","contract_sha256":extension_contract_digest(&review).unwrap(),"max_argument_bytes":256,
            "arguments":{"kind":"object","required":["labels"],"fields":{
                "labels":{"kind":"array","max_items":2,"items":{"kind":"string","max_utf8_bytes":4,"one_of":["safe"]}},
                "limit":{"kind":"integer","min":1,"max":3}
            }}
        }),
    );
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Extension(&review)
    )
    .is_ok());
    for invalid in [
        json!({"labels":["safe"],"extra":true}),
        json!({"labels":["evil"]}),
        json!({"labels":["safe"],"limit":4}),
        json!({"labels":[] ,"limit":1.5}),
        json!({"limit":1}),
    ] {
        review.arguments = invalid;
        assert_eq!(
            claim(
                &p,
                &DelegationUsage::default(),
                PreparedDelegationAction::Extension(&review)
            ),
            Err(DelegationError::NoMatchingRule)
        );
    }
    review.arguments = json!({"labels":["safe"]});
    review.prepared_contract["package"] = json!("replaced");
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Extension(&review)
    )
    .is_err());
    review.prepared_contract["package"] = json!("first");
    review.context.profile_id = Some(3);
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Extension(&review)
    )
    .is_err());
    review.context.profile_id = Some(2);
    review.tool.operation.classification = OperationClassification::Administrative;
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Extension(&review)
    )
    .is_err());
}

#[test]
fn migration_rules_require_plan_coverage_risks_and_both_rename_endpoints() {
    let objects = json!([{"kind":"table","qualified_name":"public.old"},{"kind":"table","qualified_name":"public.new"}]);
    let p = policy(
        json!({"kind":"migration","objects":objects,"changes":["rename"],"acknowledged_risks":["locking"],"max_changes":2,"max_total_changes":2}),
    );
    let node = |name: &str| json!({"id":name,"kind":"table","name":name,"qualified_name":name,"completeness":"complete","details":{"kind":"none"}});
    let changes: Vec<SchemaChange> = serde_json::from_value(json!([{"id":"rename","kind":"rename","object_before":node("public.old"),"object_after":node("public.new"),"risk":"locking","reversibility":"exact"}])).unwrap();
    let mut plan: MigrationPlan = serde_json::from_value(json!({
        "id":Uuid::from_u128(1),"run_id":Uuid::from_u128(2),"digest":"fixture","diff_digest":"fixture","expected_live_revision":1,
        "groups":[{"ordinal":0,"transactional":true,"statements":[{"ordinal":0,"sql":"ALTER TABLE old RENAME TO new","fingerprint":"fixture","change_ids":["rename"],"risk":"locking"}]}],
        "required_acknowledgements":["locking"],"expires_at":"2030-01-01T00:00:00Z"
    })).unwrap();
    let exact = policy(
        json!({"kind":"exact_migration","contract_sha256":migration_contract_digest(&plan, &changes).unwrap(),"change_count":changes.len()}),
    );
    assert!(claim(
        &exact,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes
        }
    )
    .is_ok());
    let first = claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes,
        },
    )
    .unwrap();
    assert_eq!(first.after.rules[&p.rules[0].id].schema_changes, 1);
    plan.groups[0].statements[0]
        .change_ids
        .push(SchemaChangeId("unreviewed".into()));
    assert!(claim(
        &exact,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes
        }
    )
    .is_err());
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes
        }
    )
    .is_err());
    plan.groups[0].statements[0].change_ids.pop();
    plan.required_acknowledgements
        .push(SchemaChangeRisk::DataLoss);
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes
        }
    )
    .is_err());
    plan.required_acknowledgements.pop();
    let mut changed = changes.clone();
    changed[0].object_after.as_mut().unwrap().qualified_name = "other.new".into();
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changed
        }
    )
    .is_err());
    plan.groups[0].statements[0].change_ids.clear();
    assert!(claim(
        &p,
        &DelegationUsage::default(),
        PreparedDelegationAction::Migration {
            plan: &plan,
            changes: &changes
        }
    )
    .is_err());
}

#[test]
fn overlapping_rules_do_not_bypass_global_call_limits_or_corrupt_usage() {
    let mut p = policy(json!({"kind":"exact_sql","sql":"SELECT 1","params":[]}));
    p.budget.max_tool_calls = 3;
    p.rules[0].max_uses = 1;
    let mut second = p.rules[0].clone();
    second.id = Uuid::from_u128(2);
    second.max_uses = 3;
    p.rules.push(second);
    let mut usage = DelegationUsage::default();
    for index in 0..3 {
        let debit = claim(
            &p,
            &usage,
            PreparedDelegationAction::Sql {
                sql: "SELECT 1",
                params: &[],
            },
        )
        .unwrap();
        assert_eq!(debit.rule_id, p.rules[usize::from(index > 0)].id);
        usage = debit.after;
    }
    assert_eq!(
        claim(
            &p,
            &usage,
            PreparedDelegationAction::Sql {
                sql: "SELECT 1",
                params: &[]
            }
        ),
        Err(DelegationError::BudgetExceeded)
    );
    let mut corrupt = DelegationUsage::default();
    corrupt.rules.insert(
        p.rules[0].id,
        DelegationRuleUsage {
            uses: 1,
            ..Default::default()
        },
    );
    assert_eq!(
        reserve_tool_call(&p, &corrupt, 1, 1),
        Err(DelegationError::BudgetExceeded)
    );
}

#[test]
fn canonical_hashes_private_debug_and_strict_policy_decoding() {
    let a = serde_json::from_str::<serde_json::Value>(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap();
    let b = serde_json::from_str::<serde_json::Value>(r#"{"a":0,"z":{"a":1,"b":2}}"#).unwrap();
    assert_eq!(canonical_digest(&a).unwrap(), canonical_digest(&b).unwrap());
    let mut p = policy(json!({"kind":"exact_sql","sql":"private sql secret","params":[]}));
    assert!(!format!("{p:?}").contains("private sql secret"));
    p.rules.push(p.rules[0].clone());
    assert_eq!(validate_policy(&p), Err(DelegationError::InvalidPolicy));
    let mut value = serde_json::to_value(&p).unwrap();
    value["model_selected_actor"] = json!(99);
    assert!(serde_json::from_value::<AiDelegationPolicy>(value).is_err());
}

#[test]
fn invalid_policy_shapes_and_foreign_counters_refuse_before_reservation() {
    let mut p = policy(json!({"kind":"exact_sql","sql":"SELECT 1","params":[]}));
    let mut foreign = DelegationUsage::default();
    foreign
        .rules
        .insert(Uuid::from_u128(999), DelegationRuleUsage::default());
    assert_eq!(
        reserve_tool_call(&p, &foreign, 1, 1),
        Err(DelegationError::BudgetExceeded)
    );
    p.budget.max_duration_secs = 3601;
    assert_eq!(validate_policy(&p), Err(DelegationError::InvalidPolicy));
    p.budget.max_duration_secs = 600;
    p.rules[0].action = AiDelegationActionRule::Extension {
        contract_sha256: "a".repeat(64),
        max_argument_bytes: 64,
        arguments: AiDelegationArgumentRule::Integer { min: 5, max: 4 },
    };
    assert_eq!(validate_policy(&p), Err(DelegationError::InvalidPolicy));
    let mut arguments = AiDelegationArgumentRule::Boolean;
    for _ in 0..18 {
        arguments = AiDelegationArgumentRule::Array {
            max_items: 1,
            items: Box::new(arguments),
        };
    }
    if let AiDelegationActionRule::Extension {
        arguments: actual, ..
    } = &mut p.rules[0].action
    {
        *actual = arguments;
    }
    assert_eq!(validate_policy(&p), Err(DelegationError::InvalidPolicy));
}

#[test]
fn refused_preparation_keeps_call_budget_and_dispatch_does_not_charge_twice() {
    let p = policy(json!({"kind":"exact_sql","sql":"SELECT 1","params":[]}));
    let reserved = reserve_tool_call(&p, &DelegationUsage::default(), 32, 128).unwrap();
    assert_eq!(
        reserve_action(
            &p,
            &reserved,
            PreparedDelegationAction::Sql {
                sql: "DELETE FROM items",
                params: &[]
            },
            ArbitrarySqlPolicy::default()
        ),
        Err(DelegationError::NoMatchingRule)
    );
    assert_eq!(reserved.tool_calls, 1);
    assert_eq!(reserved.output_bytes, 128);
    let debit = reserve_action(
        &p,
        &reserved,
        PreparedDelegationAction::Sql {
            sql: "SELECT 1",
            params: &[],
        },
        ArbitrarySqlPolicy::default(),
    )
    .unwrap();
    assert_eq!(debit.after.tool_calls, reserved.tool_calls);
    assert_eq!(debit.after.input_bytes, reserved.input_bytes);
    assert_eq!(debit.after.output_bytes, reserved.output_bytes);
    assert_eq!(
        reserve_action(
            &p,
            &debit.after,
            PreparedDelegationAction::Sql {
                sql: "SELECT 1",
                params: &[]
            },
            ArbitrarySqlPolicy::default()
        ),
        Err(DelegationError::BudgetExceeded)
    );
}
