WITH actor AS MATERIALIZED (
 SELECT a.oid,a.rolname,a.rolsuper FROM pg_catalog.pg_roles a WHERE a.rolname=current_user
), recipient AS MATERIALIZED (
 SELECT r.oid,r.rolname FROM pg_catalog.pg_roles r WHERE $4::text IS NOT NULL AND r.rolname=$4::text
 UNION ALL SELECT 0::oid,NULL::name WHERE $4::text IS NULL
), namespaces AS MATERIALIZED (
 SELECT n.* FROM pg_catalog.pg_namespace n
 WHERE n.nspname<>'information_schema' AND pg_catalog.substr(n.nspname::text,1,3)<>'pg_'
 AND (pg_catalog.has_schema_privilege(n.oid,'USAGE,CREATE') OR pg_catalog.pg_has_role(current_user,n.nspowner,'USAGE'))
), relation AS MATERIALIZED (
 SELECT c.*,n.nspname FROM pg_catalog.pg_class c JOIN namespaces n ON n.oid=c.relnamespace
 WHERE c.oid=$2::bigint::oid AND c.relkind IN ('r','p','v','m','f','S')
), objects AS (
 SELECT d.oid,NULL::smallint AS subid,d.datname::text AS name,NULL::text AS column_name,d.datdba AS owner_oid,
 d.datacl AS acl,NULL::aclitem[] AS parent_acl,
 pg_catalog.format('DATABASE %I',d.datname) AS target_sql,
 pg_catalog.jsonb_build_array(d.oid,d.datname,d.datdba) AS identity,NULL::jsonb AS columns
 FROM pg_catalog.pg_database d WHERE $1::text='database' AND d.oid=$2::bigint::oid AND d.datname=current_database()
 UNION ALL
 SELECT n.oid,NULL,n.nspname,NULL,n.nspowner,n.nspacl,NULL,pg_catalog.format('SCHEMA %I',n.nspname),
 pg_catalog.jsonb_build_array(n.oid,n.nspname,n.nspowner),NULL
 FROM namespaces n WHERE $1::text='schema' AND n.oid=$2::bigint::oid
 UNION ALL
 SELECT c.oid,NULL,c.relname,NULL,c.relowner,c.relacl,NULL,
 pg_catalog.format('%s %I.%I',CASE WHEN c.relkind='S' THEN 'SEQUENCE' ELSE 'TABLE' END,c.nspname,c.relname),
 pg_catalog.jsonb_build_array(c.oid,c.relnamespace,c.nspname,c.relname,c.relowner,c.relkind),
 CASE WHEN $1::text='table' THEN (SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('identity',pg_catalog.jsonb_build_array(a.attnum,a.attname,a.atttypid,a.atttypmod),
 'raw_acl',a.attacl::text,'grants',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('grantor',e.grantor::bigint,'grantee',e.grantee::bigint,'privilege',e.privilege_type,'grantable',e.is_grantable) ORDER BY e.grantor,e.grantee,e.privilege_type COLLATE "C")
 FROM pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(a.attacl)>0 THEN a.attacl END) e),'[]'::jsonb)) ORDER BY a.attnum)
 FROM pg_catalog.pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped) END
 FROM relation c WHERE (($1::text='table' AND c.relkind<>'S') OR ($1::text='sequence' AND c.relkind='S'))
 AND (pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR
 CASE WHEN c.relkind='S' THEN pg_catalog.has_sequence_privilege(c.oid,'USAGE,SELECT,UPDATE')
 ELSE pg_catalog.has_table_privilege(c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER' || CASE WHEN pg_catalog.current_setting('server_version_num')::int>=170000 THEN ',MAINTAIN' ELSE '' END) OR pg_catalog.has_any_column_privilege(c.oid,'SELECT,INSERT,UPDATE,REFERENCES') END)
 UNION ALL
 SELECT c.oid,a.attnum,c.relname,a.attname,c.relowner,a.attacl,c.relacl,pg_catalog.format('TABLE %I.%I',c.nspname,c.relname),
 pg_catalog.jsonb_build_array(c.oid,c.relnamespace,c.nspname,c.relname,c.relowner,c.relkind,a.attnum,a.attname,a.atttypid,a.atttypmod),NULL
 FROM relation c JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid
 WHERE $1::text='column' AND c.relkind<>'S' AND a.attnum=$3::bigint::smallint AND a.attnum>0 AND NOT a.attisdropped
 AND (pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR pg_catalog.has_column_privilege(c.oid,a.attnum,'SELECT,INSERT,UPDATE,REFERENCES'))
 UNION ALL
 SELECT p.oid,NULL,p.proname,NULL,p.proowner,p.proacl,NULL,
 pg_catalog.format('ROUTINE %I.%I(%s)',n.nspname,p.proname,pg_catalog.oidvectortypes(p.proargtypes)),
 pg_catalog.jsonb_build_array(p.oid,p.pronamespace,n.nspname,p.proname,p.proowner,p.prokind,p.proargtypes::text,pg_catalog.oidvectortypes(p.proargtypes)),NULL
 FROM pg_catalog.pg_proc p JOIN namespaces n ON n.oid=p.pronamespace
 WHERE $1::text='routine' AND p.oid=$2::bigint::oid
 AND (pg_catalog.has_function_privilege(p.oid,'EXECUTE') OR pg_catalog.pg_has_role(current_user,p.proowner,'USAGE'))
), selected AS MATERIALIZED (
 SELECT o.*,COALESCE(o.acl,pg_catalog.acldefault(
 CASE $1::text WHEN 'database' THEN 'd' WHEN 'schema' THEN 'n' WHEN 'table' THEN 'r' WHEN 'column' THEN 'c' WHEN 'sequence' THEN 's' ELSE 'f' END::"char",o.owner_oid)) AS effective_acl
 FROM objects o WHERE COALESCE(pg_catalog.pg_column_size(o.acl),0)+COALESCE(pg_catalog.pg_column_size(o.parent_acl),0)<=32768
 AND COALESCE(pg_catalog.octet_length(o.columns::text),0)<=65536
), edges AS MATERIALIZED (
 SELECT e.* FROM selected o CROSS JOIN LATERAL pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(o.effective_acl)>0 THEN o.effective_acl END) e
), authority_edges AS MATERIALIZED (
 SELECT e.* FROM edges e
 UNION ALL
 SELECT e.* FROM selected o CROSS JOIN LATERAL pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(o.parent_acl)>0 THEN o.parent_acl END) e
 WHERE $1::text='column'
), grantors AS MATERIALIZED (
 SELECT r.oid,r.rolname FROM selected o CROSS JOIN actor a JOIN pg_catalog.pg_roles r ON r.oid=o.owner_oid
 WHERE a.rolsuper OR pg_catalog.pg_has_role(a.oid,o.owner_oid,'USAGE')
 UNION ALL
 SELECT DISTINCT r.oid,r.rolname FROM selected o CROSS JOIN actor a CROSS JOIN authority_edges e JOIN pg_catalog.pg_roles r ON r.oid=e.grantee
 WHERE NOT (a.rolsuper OR pg_catalog.pg_has_role(a.oid,o.owner_oid,'USAGE'))
 AND e.privilege_type=$5::text AND e.is_grantable AND pg_catalog.pg_has_role(a.oid,e.grantee,'USAGE')
)
SELECT pg_catalog.jsonb_build_object(
 'database',current_database(),'actor',current_user,
 'identity',pg_catalog.jsonb_build_array(pg_catalog.current_setting('server_version_num'),d.oid,d.datname,pg_catalog.pg_backend_pid(),a.oid,a.rolname,a.rolsuper,session_user,(SELECT oid FROM pg_catalog.pg_roles WHERE rolname=session_user),o.identity,r.oid,r.rolname,pg_catalog.current_setting('search_path')),
 'version',pg_catalog.current_setting('server_version_num')::int,
 'can_grant',CASE WHEN $5::text='MAINTAIN' AND pg_catalog.current_setting('server_version_num')::int<170000 THEN false ELSE CASE $1::text
 WHEN 'database' THEN pg_catalog.has_database_privilege(a.oid,o.oid,$5::text || ' WITH GRANT OPTION')
 WHEN 'schema' THEN pg_catalog.has_schema_privilege(a.oid,o.oid,$5::text || ' WITH GRANT OPTION')
 WHEN 'table' THEN pg_catalog.has_table_privilege(a.oid,o.oid,$5::text || ' WITH GRANT OPTION')
 WHEN 'column' THEN pg_catalog.has_column_privilege(a.oid,o.oid,o.subid,$5::text || ' WITH GRANT OPTION')
 WHEN 'sequence' THEN pg_catalog.has_sequence_privilege(a.oid,o.oid,$5::text || ' WITH GRANT OPTION')
 ELSE pg_catalog.has_function_privilege(a.oid,o.oid,$5::text || ' WITH GRANT OPTION') END END,
 'target_sql',o.target_sql,'column_name',o.column_name,'grantee_oid',r.oid::bigint,
 'grantee_sql',CASE WHEN r.oid=0 THEN 'PUBLIC' ELSE pg_catalog.format('%I',r.rolname) END,
 'grantors',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('oid',g.oid::bigint,'name',g.rolname) ORDER BY g.oid) FROM grantors g),'[]'::jsonb),
 'grants',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('grantor',e.grantor::bigint,'grantee',e.grantee::bigint,'privilege',e.privilege_type,'grantable',e.is_grantable) ORDER BY e.grantor,e.grantee,e.privilege_type COLLATE "C") FROM edges e),'[]'::jsonb),
  'parent_grants',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('grantor',e.grantor::bigint,'grantee',e.grantee::bigint,'privilege',e.privilege_type,'grantable',e.is_grantable) ORDER BY e.grantor,e.grantee,e.privilege_type COLLATE "C")
 FROM pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(o.parent_acl)>0 THEN o.parent_acl END) e),'[]'::jsonb),
 'acl',o.acl::text,'parent_acl',o.parent_acl::text,'columns',COALESCE(o.columns,'[]'::jsonb)
)::text FROM selected o CROSS JOIN actor a CROSS JOIN recipient r CROSS JOIN pg_catalog.pg_database d WHERE d.datname=current_database();
