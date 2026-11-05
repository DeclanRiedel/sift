WITH public_roles AS (
 SELECT oid,rolname,rolsuper,rolinherit,rolcreaterole,rolcreatedb,
        rolcanlogin,rolreplication,rolbypassrls,rolconnlimit,
        CASE WHEN rolvaliduntil IS NULL THEN NULL
             WHEN NOT pg_catalog.isfinite(rolvaliduntil) THEN rolvaliduntil::text
             ELSE pg_catalog.to_char(rolvaliduntil AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') END AS valid_until
 FROM pg_catalog.pg_roles
), edges AS (
 SELECT a.* FROM pg_catalog.pg_auth_members a JOIN public_roles t
 ON a.roleid=t.oid OR a.member=t.oid OR a.grantor=t.oid
 WHERE t.rolname=$1::text ORDER BY a.roleid,a.member,a.grantor LIMIT 201
), dependencies AS (
 SELECT s.* FROM pg_catalog.pg_shdepend s JOIN public_roles t ON t.oid=s.refobjid
 WHERE s.refclassid='pg_catalog.pg_authid'::pg_catalog.regclass AND t.rolname=$1::text
 ORDER BY s.dbid,s.classid,s.objid,s.objsubid,s.deptype LIMIT 201
)
SELECT pg_catalog.jsonb_build_object(
 'version',pg_catalog.current_setting('server_version_num')::integer,
 'database',pg_catalog.jsonb_build_array(d.oid,d.datname),
 'actor',pg_catalog.to_jsonb(a),'session_actor',pg_catalog.to_jsonb(sa),
 'target',pg_catalog.to_jsonb(t),'destination',pg_catalog.to_jsonb(n),
 'admin',COALESCE(pg_catalog.pg_has_role(a.oid,t.oid,'MEMBER WITH ADMIN OPTION'),false),
 'edges',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(e) ORDER BY e.roleid,e.member,e.grantor) FROM edges e),'[]'::jsonb),
 'dependencies',COALESCE((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(dep) ORDER BY dep.dbid,dep.classid,dep.objid,dep.objsubid,dep.deptype) FROM dependencies dep),'[]'::jsonb)
)::text
FROM public_roles a CROSS JOIN public_roles sa CROSS JOIN pg_catalog.pg_database d
LEFT JOIN public_roles t ON t.rolname=$1::text
LEFT JOIN public_roles n ON n.rolname=$2::text
WHERE a.rolname=current_user AND sa.rolname=session_user AND d.datname=current_database()
