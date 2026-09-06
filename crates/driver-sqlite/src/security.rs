//! SQLite itself enforces SQL authority, including PRAGMAs executed at prepare.
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    Connection,
};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
thread_local! {
    static BUSY: RefCell<Option<(Arc<AtomicBool>, u32, Instant)>> = const { RefCell::new(None) };
}
pub fn install(
    conn: &Connection,
    internal: Arc<AtomicBool>,
    readonly: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    busy_timeout_ms: u32,
    effects: Arc<AtomicU8>,
) -> rusqlite::Result<()> {
    BUSY.with(|slot| *slot.borrow_mut() = Some((closing.clone(), busy_timeout_ms, Instant::now())));
    conn.busy_handler(Some(|attempt| {
        BUSY.with(|slot| {
            let mut state = slot.borrow_mut();
            let Some((closing, limit, start)) = state.as_mut() else {
                return false;
            };
            if attempt == 0 {
                *start = Instant::now();
            }
            if closing.load(Ordering::Acquire)
                || start.elapsed() >= Duration::from_millis(u64::from(*limit))
            {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
            true
        })
    }))?;
    conn.progress_handler(1000, Some(move || closing.load(Ordering::Acquire)));
    conn.authorizer(Some(move |context: AuthContext<'_>| {
        use AuthAction::*;
        if internal.load(Ordering::Acquire) {
            return Authorization::Allow;
        }
        match context.action {
            Insert { table_name } | Update { table_name, .. } | Delete { table_name } => {
                effects.fetch_or(
                    if matches!(
                        table_name,
                        "sqlite_master"
                            | "sqlite_schema"
                            | "sqlite_temp_master"
                            | "sqlite_temp_schema"
                    ) {
                        2
                    } else {
                        1
                    },
                    Ordering::AcqRel,
                );
            }
            CreateIndex { .. }
            | CreateTable { .. }
            | CreateTempIndex { .. }
            | CreateTempTable { .. }
            | CreateTempTrigger { .. }
            | CreateTempView { .. }
            | CreateTrigger { .. }
            | CreateView { .. }
            | DropIndex { .. }
            | DropTable { .. }
            | DropTempIndex { .. }
            | DropTempTable { .. }
            | DropTempTrigger { .. }
            | DropTempView { .. }
            | DropTrigger { .. }
            | DropView { .. }
            | Analyze { .. }
            | Reindex { .. }
            | AlterTable { .. } => {
                effects.fetch_or(2, Ordering::AcqRel);
            }
            _ => {}
        }
        let read = matches!(
            context.action,
            Read { .. } | Select | Function { .. } | Recursive | Pragma { .. }
        );
        if readonly.load(Ordering::Acquire) && !read {
            return Authorization::Deny;
        }
        let allowed = match context.action {
            Attach { .. }
            | Detach { .. }
            | Transaction { .. }
            | Savepoint { .. }
            | CreateVtable { .. }
            | DropVtable { .. }
            | Unknown { .. } => false,
            Function { function_name } => !["load_extension", "writefile", "readfile", "edit"]
                .iter()
                .any(|name| function_name.eq_ignore_ascii_case(name)),
            Pragma {
                pragma_name,
                pragma_value,
            } => {
                let name = pragma_name.to_ascii_lowercase();
                match name.as_str() {
                    "table_info" | "table_xinfo" | "index_list" | "index_info" | "index_xinfo"
                    | "foreign_key_list" => true,
                    "schema_version" | "user_version" | "application_id" | "compile_options"
                    | "database_list" | "table_list" | "integrity_check" | "quick_check"
                    | "foreign_key_check" | "page_count" | "freelist_count" => {
                        pragma_value.is_none()
                    }
                    _ => false,
                }
            }
            _ => true,
        };
        if allowed {
            Authorization::Allow
        } else {
            Authorization::Deny
        }
    }));
    Ok(())
}
