WITH roles AS (
 SELECT oid,rolname,rolsuper,rolcreaterole FROM pg_catalog.pg_roles
), defaults AS (
 SELECT s.setdatabase,substring(c FROM length($2::text)+2) AS value
 FROM pg_catalog.pg_db_role_setting s
 JOIN roles r ON r.oid=s.setrole
 CROSS JOIN LATERAL unnest(s.setconfig) c
 WHERE r.rolname=$1::text AND split_part(c,'=',1)=$2::text
)
SELECT pg_catalog.jsonb_build_object(
 'version',pg_catalog.current_setting('server_version_num')::integer,
 'database',pg_catalog.jsonb_build_array(d.oid,d.datname),
 'actor',pg_catalog.to_jsonb(a),'session_actor',pg_catalog.to_jsonb(sa),
 'target',pg_catalog.to_jsonb(t),
 'admin',COALESCE(pg_catalog.pg_has_role(a.oid,t.oid,'MEMBER WITH ADMIN OPTION'),false),
 'global_value',(SELECT left(value,65) FROM defaults WHERE setdatabase=0),
 'database_value',(SELECT left(value,65) FROM defaults WHERE setdatabase=d.oid)
)::text
FROM roles a CROSS JOIN roles sa CROSS JOIN pg_catalog.pg_database d
LEFT JOIN roles t ON t.rolname=$1::text
WHERE a.rolname=current_user AND sa.rolname=session_user AND d.datname=current_database()
