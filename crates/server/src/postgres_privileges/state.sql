WITH subject AS MATERIALIZED (
 SELECT oid,rolname,rolsuper,rolbypassrls FROM pg_catalog.pg_roles
 WHERE rolname=COALESCE($1::text,current_user::text)
), namespaces AS MATERIALIZED (
 SELECT oid,nspname FROM pg_catalog.pg_namespace
 WHERE nspname<>'information_schema' AND nspname NOT LIKE 'pg\_%' ESCAPE '\'
 AND (pg_catalog.has_schema_privilege(oid,'USAGE,CREATE') OR pg_catalog.pg_has_role(current_user,nspowner,'USAGE'))
), relations AS MATERIALIZED (
 SELECT c.*,n.nspname FROM pg_catalog.pg_class c JOIN namespaces n ON n.oid=c.relnamespace
 WHERE c.relkind IN ('r','p','v','m','f','S') AND (
 pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR
 CASE WHEN c.relkind='S' THEN pg_catalog.has_sequence_privilege(c.oid,'USAGE,SELECT,UPDATE')
 ELSE pg_catalog.has_table_privilege(c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER' || CASE WHEN pg_catalog.current_setting('server_version_num')::int>=170000 THEN ',MAINTAIN' ELSE '' END)
 OR pg_catalog.has_any_column_privilege(c.oid,'SELECT,INSERT,UPDATE,REFERENCES') END)
), objects AS (
 SELECT 'database'::text AS kind,d.oid AS object_oid,NULL::smallint AS column_number,NULL::text AS schema,NULL::text AS relation,d.datname::text AS name,false AS row_security_enabled,false AS row_security_forced,
 ARRAY['CONNECT','CREATE','TEMPORARY']::text[] AS privileges
 FROM pg_catalog.pg_database d WHERE d.datname=current_database()
 UNION ALL
 SELECT 'schema',oid,NULL,nspname,NULL,nspname,false,false,ARRAY['USAGE','CREATE'] FROM namespaces
 UNION ALL
 SELECT CASE WHEN relkind='S' THEN 'sequence' ELSE 'table' END,oid,NULL,nspname,NULL,relname,relrowsecurity,relforcerowsecurity,
 CASE WHEN relkind='S' THEN ARRAY['USAGE','SELECT','UPDATE'] ELSE ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] || CASE WHEN pg_catalog.current_setting('server_version_num')::int>=170000 THEN ARRAY['MAINTAIN'] ELSE ARRAY[]::text[] END END
 FROM relations
 UNION ALL
 SELECT 'column',c.oid,a.attnum,c.nspname,c.relname,a.attname,c.relrowsecurity,c.relforcerowsecurity,ARRAY['SELECT','INSERT','UPDATE','REFERENCES']
 FROM relations c JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid
 WHERE c.relkind<>'S' AND a.attnum>0 AND NOT a.attisdropped AND
 (pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR pg_catalog.has_column_privilege(c.oid,a.attnum,'SELECT,INSERT,UPDATE,REFERENCES'))
 UNION ALL
 SELECT 'routine',p.oid,NULL,n.nspname,NULL,p.proname,false,false,ARRAY['EXECUTE']
 FROM pg_catalog.pg_proc p JOIN namespaces n ON n.oid=p.pronamespace
 WHERE pg_catalog.has_function_privilege(p.oid,'EXECUTE') OR pg_catalog.pg_has_role(current_user,p.proowner,'USAGE')
), selected AS MATERIALIZED (
 SELECT * FROM objects ORDER BY kind,schema NULLS FIRST,relation NULLS FIRST,name,object_oid,column_number NULLS FIRST
 LIMIT $2::bigint OFFSET $3::bigint
), checked AS (
 SELECT o.kind,o.object_oid::bigint,o.column_number,o.schema,o.relation,o.name,o.row_security_enabled,o.row_security_forced,
 (SELECT pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object('privilege',p,'allowed',
 CASE o.kind
 WHEN 'database' THEN pg_catalog.has_database_privilege(s.oid,o.object_oid,p)
 WHEN 'schema' THEN pg_catalog.has_schema_privilege(s.oid,o.object_oid,p)
 WHEN 'table' THEN pg_catalog.has_table_privilege(s.oid,o.object_oid,p)
 WHEN 'column' THEN pg_catalog.has_column_privilege(s.oid,o.object_oid,o.column_number,p)
 WHEN 'sequence' THEN pg_catalog.has_sequence_privilege(s.oid,o.object_oid,p)
 WHEN 'routine' THEN pg_catalog.has_function_privilege(s.oid,o.object_oid,p) END,
 'grantable',CASE o.kind
 WHEN 'database' THEN pg_catalog.has_database_privilege(s.oid,o.object_oid,p || ' WITH GRANT OPTION')
 WHEN 'schema' THEN pg_catalog.has_schema_privilege(s.oid,o.object_oid,p || ' WITH GRANT OPTION')
 WHEN 'table' THEN pg_catalog.has_table_privilege(s.oid,o.object_oid,p || ' WITH GRANT OPTION')
 WHEN 'column' THEN pg_catalog.has_column_privilege(s.oid,o.object_oid,o.column_number,p || ' WITH GRANT OPTION')
 WHEN 'sequence' THEN pg_catalog.has_sequence_privilege(s.oid,o.object_oid,p || ' WITH GRANT OPTION')
 WHEN 'routine' THEN pg_catalog.has_function_privilege(s.oid,o.object_oid,p || ' WITH GRANT OPTION') END) ORDER BY ord)
 FROM pg_catalog.unnest(o.privileges) WITH ORDINALITY AS privilege(p,ord)) AS checks
 FROM selected o CROSS JOIN subject s
)
SELECT pg_catalog.jsonb_build_object('database',current_database(),'actor',current_user,'role',s.rolname,
 'role_superuser',s.rolsuper,'role_bypass_rls',s.rolbypassrls,
 'objects',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(c) ORDER BY c.kind,c.schema NULLS FIRST,c.relation NULLS FIRST,c.name,c.object_oid,c.column_number NULLS FIRST) FROM checked c),'[]'::jsonb),
 'next_offset',NULL,'page_limit_reached',false,'warning','')::text FROM subject s;
