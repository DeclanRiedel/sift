SELECT r.rolname::text,r.rolcanlogin::text,r.rolcreaterole::text,r.rolsuper::text,
pg_catalog.jsonb_build_object(
 'inherit',r.rolinherit,'create_database',r.rolcreatedb,'replication',r.rolreplication,
 'bypass_rls',r.rolbypassrls,'connection_limit',r.rolconnlimit,'valid_until',r.rolvaliduntil::text,
 'defaults',COALESCE((
  SELECT jsonb_agg(jsonb_build_object('setting',split_part(c,'=',1),
   'scope',CASE WHEN s.setdatabase=0 THEN 'all_databases' ELSE 'current_database' END,
   'value',left(substring(c FROM position('=' IN c)+1),64),
   'value_truncated',length(substring(c FROM position('=' IN c)+1))>64) ORDER BY s.setdatabase,c)
  FROM pg_catalog.pg_db_role_setting s CROSS JOIN LATERAL unnest(s.setconfig) c
  WHERE s.setrole=r.oid AND s.setdatabase IN (0,(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))
   AND split_part(c,'=',1) IN ('work_mem','maintenance_work_mem','statement_timeout','lock_timeout',
     'idle_in_transaction_session_timeout','jit','default_transaction_read_only','max_parallel_workers_per_gather')
 ),'[]'::jsonb)
)::text
FROM pg_catalog.pg_roles r ORDER BY r.rolname LIMIT $1::bigint OFFSET $2::bigint
