WITH target AS (
 SELECT c.* FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
 WHERE n.nspname=$1::text AND c.relname=$2::text AND c.relkind IN ('r','p')
), chosen_roles AS (
 SELECT wanted.name, r.oid FROM pg_catalog.jsonb_array_elements_text($4::text::jsonb) wanted(name)
 LEFT JOIN pg_catalog.pg_roles r ON r.rolname=wanted.name
)
SELECT pg_catalog.pg_has_role(current_user,c.relowner,'USAGE'),
 EXISTS(SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid AND p.polname=$3::text),
 NOT EXISTS(SELECT 1 FROM chosen_roles WHERE oid IS NULL),
 pg_catalog.md5(pg_catalog.jsonb_build_array(current_user,pg_catalog.current_setting('search_path'),pg_catalog.current_setting('standard_conforming_strings'),
 c.oid,c.relowner,c.relkind,c.relrowsecurity,c.relforcerowsecurity,
 (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(p) ORDER BY p.oid) FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid),
 (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(a)-'attcacheoff' ORDER BY a.attnum) FROM pg_catalog.pg_attribute a WHERE a.attrelid=c.oid),
 (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(r) ORDER BY r.name) FROM chosen_roles r))::text)::text,
 (SELECT p.polcmd::text FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid AND p.polname=$3::text),
 pg_catalog.current_setting('standard_conforming_strings')='on'
FROM target c
