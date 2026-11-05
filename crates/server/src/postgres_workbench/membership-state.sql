WITH public_roles AS (
 SELECT oid, rolname, rolsuper, rolinherit, rolcreaterole, rolcreatedb,
        rolcanlogin, rolreplication, rolbypassrls, rolconnlimit
 FROM pg_catalog.pg_roles
), pair AS (
 SELECT a.oid::text AS oid, a.grantor::text AS grantor,
        a.admin_option, a.inherit_option, a.set_option
 FROM pg_catalog.pg_auth_members a
 JOIN public_roles r ON r.oid=a.roleid JOIN public_roles m ON m.oid=a.member
 WHERE r.rolname=$1::text AND m.rolname=$2::text
 ORDER BY a.oid LIMIT 201
), authority AS (
 SELECT a.oid::text AS oid, a.grantor::text AS grantor,
        a.admin_option, a.inherit_option, a.set_option
 FROM pg_catalog.pg_auth_members a
 JOIN public_roles r ON r.oid=a.roleid JOIN public_roles g ON g.oid=a.member
 WHERE r.rolname=$1::text AND g.rolname=$3::text AND a.admin_option
 ORDER BY a.oid LIMIT 201
)
SELECT (actor.rolsuper OR pg_catalog.pg_has_role(actor.oid,g.oid,'USAGE'))::text,
       (g.oid=10 OR EXISTS(SELECT 1 FROM authority))::text,
       COALESCE((SELECT pg_catalog.jsonb_agg(pair ORDER BY oid) FROM pair),'[]'::jsonb)::text,
       COALESCE((SELECT pg_catalog.jsonb_agg(authority ORDER BY oid) FROM authority),'[]'::jsonb)::text,
       pg_catalog.jsonb_build_array(
         pg_catalog.current_setting('server_version_num'),d.oid,d.datname,
         pg_catalog.to_jsonb(actor),pg_catalog.to_jsonb(session_actor),
         pg_catalog.to_jsonb(r),pg_catalog.to_jsonb(m),pg_catalog.to_jsonb(g))::text,
       r.oid::text,m.oid::text,g.oid::text
FROM public_roles r CROSS JOIN public_roles m CROSS JOIN public_roles g
CROSS JOIN public_roles actor CROSS JOIN public_roles session_actor
CROSS JOIN pg_catalog.pg_database d
WHERE r.rolname=$1::text AND m.rolname=$2::text AND g.rolname=$3::text
AND actor.rolname=current_user AND session_actor.rolname=session_user
AND d.datname=current_database()
