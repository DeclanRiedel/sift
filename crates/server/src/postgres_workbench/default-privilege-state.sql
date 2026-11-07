WITH creator AS MATERIALIZED (
 SELECT oid,rolname,rolsuper FROM pg_catalog.pg_roles WHERE rolname OPERATOR(pg_catalog.=) $1::pg_catalog.text
), recipient AS MATERIALIZED (
 SELECT oid,rolname FROM pg_catalog.pg_roles WHERE $3::pg_catalog.text IS NOT NULL AND rolname OPERATOR(pg_catalog.=) $3::pg_catalog.text
 UNION ALL SELECT 0::pg_catalog.oid,NULL::pg_catalog.name WHERE $3::pg_catalog.text IS NULL
), namespace AS MATERIALIZED (
 SELECT oid,nspname FROM pg_catalog.pg_namespace WHERE nspname OPERATOR(pg_catalog.=) $2::pg_catalog.text
 AND nspname OPERATOR(pg_catalog.<>) 'information_schema' AND pg_catalog.substr(nspname::pg_catalog.text,1,3) OPERATOR(pg_catalog.<>) 'pg_'
 UNION ALL SELECT 0::pg_catalog.oid,NULL::pg_catalog.name WHERE $2::pg_catalog.text IS NULL
), scopes AS MATERIALIZED (
 SELECT c.oid AS owner,0::pg_catalog.oid AS namespace,
 coalesce((SELECT defaclacl FROM pg_catalog.pg_default_acl WHERE defaclrole OPERATOR(pg_catalog.=) c.oid AND defaclnamespace OPERATOR(pg_catalog.=) 0 AND defaclobjtype OPERATOR(pg_catalog.=) $4::pg_catalog.text::pg_catalog."char"),pg_catalog.acldefault($4::pg_catalog.text::pg_catalog."char",c.oid)) AS acl FROM creator c
 UNION ALL
 SELECT c.oid,n.oid,coalesce((SELECT defaclacl FROM pg_catalog.pg_default_acl WHERE defaclrole OPERATOR(pg_catalog.=) c.oid AND defaclnamespace OPERATOR(pg_catalog.=) n.oid AND defaclobjtype OPERATOR(pg_catalog.=) $4::pg_catalog.text::pg_catalog."char"),'{}'::pg_catalog.aclitem[]) FROM creator c CROSS JOIN namespace n WHERE n.oid OPERATOR(pg_catalog.<>) 0
), entries AS MATERIALIZED (
 SELECT s.namespace,e.* FROM scopes s CROSS JOIN LATERAL (
 SELECT a.grantor::pg_catalog.int8 AS grantor,a.grantee::pg_catalog.int8 AS grantee,a.privilege_type AS privilege,a.is_grantable AS grantable,
 pg_catalog.pg_get_userbyid(a.grantor)::pg_catalog.text AS grantor_name,
 CASE WHEN a.grantee OPERATOR(pg_catalog.=) 0 THEN NULL ELSE pg_catalog.pg_get_userbyid(a.grantee)::pg_catalog.text END AS grantee_name
 FROM pg_catalog.aclexplode(CASE WHEN pg_catalog.cardinality(s.acl) OPERATOR(pg_catalog.>) 0 THEN s.acl ELSE NULL END) a ORDER BY a.grantor,a.grantee,a.privilege_type COLLATE pg_catalog."C" LIMIT 257
 ) e
), snapshot AS (
 SELECT pg_catalog.jsonb_build_object(
 'version',pg_catalog.current_setting('server_version_num')::pg_catalog.int4,
 'identity',pg_catalog.jsonb_build_array(d.oid,d.datname,a.oid,a.rolname,a.rolsuper,c.oid,c.rolname,c.rolsuper,r.oid,r.rolname,n.oid,n.nspname),
 'creator_oid',c.oid::pg_catalog.int8,'recipient_oid',r.oid::pg_catalog.int8,
 'can_change',pg_catalog.pg_has_role(current_user,c.oid,'USAGE'),
 'global_grants',coalesce((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(e) OPERATOR(pg_catalog.-) 'namespace' ORDER BY grantor,grantee,privilege COLLATE pg_catalog."C") FROM entries e WHERE namespace OPERATOR(pg_catalog.=) 0),'[]'::pg_catalog.jsonb),
 'schema_grants',coalesce((SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(e) OPERATOR(pg_catalog.-) 'namespace' ORDER BY grantor,grantee,privilege COLLATE pg_catalog."C") FROM entries e WHERE namespace OPERATOR(pg_catalog.=) n.oid AND namespace OPERATOR(pg_catalog.<>) 0),'[]'::pg_catalog.jsonb)
 ) AS state FROM creator c CROSS JOIN recipient r CROSS JOIN namespace n
 JOIN pg_catalog.pg_database d ON d.datname OPERATOR(pg_catalog.=) pg_catalog.current_database()
 JOIN pg_catalog.pg_roles a ON a.rolname OPERATOR(pg_catalog.=) current_user
)
SELECT pg_catalog.left(state::pg_catalog.text,131073)::pg_catalog.text FROM snapshot
