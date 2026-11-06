WITH subject AS MATERIALIZED (
 SELECT oid,rolname,rolsuper,rolbypassrls FROM pg_catalog.pg_roles
 WHERE rolname=COALESCE($1::text,current_user::text)
), namespaces AS MATERIALIZED (
 SELECT oid,nspname,nspowner,nspacl FROM pg_catalog.pg_namespace
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
 ARRAY['CONNECT','CREATE','TEMPORARY']::text[] AS privileges, d.datdba AS owner_oid,d.datacl AS native_acl,'d'::text AS acl_kind
 FROM pg_catalog.pg_database d WHERE d.datname=current_database()
 UNION ALL
 SELECT 'schema',oid,NULL,nspname,NULL,nspname,false,false,ARRAY['USAGE','CREATE'],nspowner,nspacl,'n' FROM namespaces
 UNION ALL
 SELECT CASE WHEN relkind='S' THEN 'sequence' ELSE 'table' END,oid,NULL,nspname,NULL,relname,relrowsecurity,relforcerowsecurity,
 CASE WHEN relkind='S' THEN ARRAY['USAGE','SELECT','UPDATE'] ELSE ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] || CASE WHEN pg_catalog.current_setting('server_version_num')::int>=170000 THEN ARRAY['MAINTAIN'] ELSE ARRAY[]::text[] END END,relowner,relacl,CASE WHEN relkind='S' THEN 's' ELSE 'r' END
 FROM relations
 UNION ALL
 SELECT 'column',c.oid,a.attnum,c.nspname,c.relname,a.attname,c.relrowsecurity,c.relforcerowsecurity,ARRAY['SELECT','INSERT','UPDATE','REFERENCES'],c.relowner,a.attacl,'c'
 FROM relations c JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid
 WHERE c.relkind<>'S' AND a.attnum>0 AND NOT a.attisdropped AND
 (pg_catalog.pg_has_role(current_user,c.relowner,'USAGE') OR pg_catalog.has_column_privilege(c.oid,a.attnum,'SELECT,INSERT,UPDATE,REFERENCES'))
 UNION ALL
 SELECT 'routine',p.oid,NULL,n.nspname,NULL,p.proname,false,false,ARRAY['EXECUTE'],p.proowner,p.proacl,'f'
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
 FROM pg_catalog.unnest(o.privileges) WITH ORDINALITY AS privilege(p,ord)) AS checks,
 (WITH expanded AS MATERIALIZED (
   SELECT e.grantor::bigint AS grantor_oid,g.rolname::text AS grantor,
     e.grantee::bigint AS grantee_oid,r.rolname::text AS grantee,
     e.privilege_type AS privilege,e.is_grantable AS grantable,
     CASE WHEN e.grantee=0 OR e.grantee=s.oid THEN true
       ELSE pg_catalog.pg_has_role(s.oid,e.grantee,'USAGE') END AS applies_to_subject
   FROM pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(COALESCE(o.native_acl,pg_catalog.acldefault(o.acl_kind::"char",o.owner_oid)))>0
     THEN (COALESCE(o.native_acl,pg_catalog.acldefault(o.acl_kind::"char",o.owner_oid)))[1:65] END) e
   LEFT JOIN pg_catalog.pg_roles g ON g.oid=e.grantor
   LEFT JOIN pg_catalog.pg_roles r ON r.oid=e.grantee
   ORDER BY e.grantee,e.grantor,e.privilege_type,e.is_grantable LIMIT 65
 ), visible AS (
   SELECT * FROM expanded ORDER BY grantee_oid,grantor_oid,privilege,grantable LIMIT 64
 )
 SELECT pg_catalog.jsonb_build_object('owner_oid',o.owner_oid::bigint,
   'owner',(SELECT rolname::text FROM pg_catalog.pg_roles WHERE oid=o.owner_oid),
   'defaulted',o.native_acl IS NULL,
   'grants',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(v) ORDER BY v.grantee_oid,v.grantor_oid,v.privilege,v.grantable) FROM visible v),'[]'::jsonb),
   'truncated',COALESCE(pg_catalog.cardinality(o.native_acl)>65,false) OR (SELECT count(*)>64 FROM expanded))) AS acl
 FROM selected o CROSS JOIN subject s
)
SELECT pg_catalog.jsonb_build_object('database',current_database(),'actor',current_user,'role',s.rolname,
 'role_superuser',s.rolsuper,'role_bypass_rls',s.rolbypassrls,
 'objects',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(c) ORDER BY c.kind,c.schema NULLS FIRST,c.relation NULLS FIRST,c.name,c.object_oid,c.column_number NULLS FIRST) FROM checked c),'[]'::jsonb),
 'next_offset',NULL,'page_limit_reached',false,'warning','')::text FROM subject s;
