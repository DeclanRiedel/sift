WITH parent AS (
    SELECT p.* FROM pg_catalog.pg_class p
    JOIN pg_catalog.pg_namespace n ON n.oid = p.relnamespace
    WHERE n.nspname = $1::text AND p.relname = $2::text AND p.relkind = 'p'
), child AS (
    SELECT c.* FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = $3::text AND c.relname = $4::text AND c.relkind = 'r'
), targets AS (
    SELECT * FROM parent UNION ALL SELECT * FROM child
), columns AS (
    SELECT a.attrelid,
        jsonb_agg(jsonb_build_array(a.attname, a.atttypid, a.atttypmod,
            a.attcollation, a.attnotnull, a.attgenerated, a.attidentity)
            ORDER BY a.attname) AS signature
    FROM pg_catalog.pg_attribute a
    WHERE a.attrelid IN (SELECT oid FROM targets) AND a.attnum > 0 AND NOT a.attisdropped
    GROUP BY a.attrelid
), dependencies AS (
    SELECT row_to_json(d)::text AS signature,
        pg_catalog.pg_describe_object(d.classid, d.objid, d.objsubid) AS description
    FROM pg_catalog.pg_depend d
    WHERE d.refclassid = 'pg_catalog.pg_class'::regclass
        AND d.refobjid IN (SELECT oid FROM targets)
    UNION ALL
    SELECT jsonb_build_array('default', pt.partdefid)::text,
        'Default partition: ' || pg_catalog.pg_describe_object('pg_catalog.pg_class'::regclass, pt.partdefid, 0)
    FROM pg_catalog.pg_partitioned_table pt
    WHERE pt.partrelid IN (SELECT oid FROM parent) AND pt.partdefid <> 0
), impact AS (
    SELECT json_agg(description ORDER BY description)::text AS objects
    FROM (SELECT DISTINCT left(description, 256) AS description
        FROM dependencies ORDER BY description LIMIT 51) bounded
)
SELECT pt.partstrat::text, pt.partnatts::bigint,
    (pg_catalog.pg_has_role(current_user, p.relowner, 'USAGE')
        AND pg_catalog.pg_has_role(current_user, c.relowner, 'USAGE')),
    (NOT c.relispartition AND NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid)),
    ((SELECT signature FROM columns WHERE attrelid = p.oid)
        IS NOT DISTINCT FROM (SELECT signature FROM columns WHERE attrelid = c.oid)),
    (pt.partdefid <> 0),
    md5(jsonb_build_array(
        current_user, current_setting('search_path'), current_setting('DateStyle'),
        current_setting('TimeZone'), current_setting('IntervalStyle'),
        to_jsonb(pt),
        (SELECT jsonb_agg(jsonb_build_array(t.oid, t.relowner, t.relkind,
            t.relnamespace, t.relpersistence, t.reloptions, t.relrowsecurity,
            t.relforcerowsecurity, t.relreplident) ORDER BY t.oid) FROM targets t),
        (SELECT jsonb_agg(to_jsonb(a) - 'attcacheoff' ORDER BY a.attrelid, a.attnum)
            FROM pg_catalog.pg_attribute a WHERE a.attrelid IN (SELECT oid FROM targets)),
        (SELECT jsonb_agg(to_jsonb(d) ORDER BY d.oid) FROM pg_catalog.pg_attrdef d
            WHERE d.adrelid IN (SELECT oid FROM targets)),
        (SELECT jsonb_agg(to_jsonb(k) ORDER BY k.oid) FROM pg_catalog.pg_constraint k
            WHERE k.conrelid IN (SELECT oid FROM targets) OR k.confrelid IN (SELECT oid FROM targets)),
        (SELECT jsonb_agg(to_jsonb(i) ORDER BY i.indexrelid) FROM pg_catalog.pg_index i
            WHERE i.indrelid IN (SELECT oid FROM targets)),
        (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.oid) FROM pg_catalog.pg_trigger t
            WHERE t.tgrelid IN (SELECT oid FROM targets)),
        (SELECT jsonb_agg(jsonb_build_array(tree.relid, tree.parentrelid,
            descendant.relpartbound::text, descendant.relowner) ORDER BY tree.relid)
            FROM pg_catalog.pg_partition_tree(p.oid) tree
            JOIN pg_catalog.pg_class descendant ON descendant.oid = tree.relid),
        (SELECT jsonb_agg(jsonb_build_array(signature, description) ORDER BY signature, description) FROM dependencies)
    )::text)::text,
    coalesce(impact.objects, '[]')::text
FROM parent p CROSS JOIN child c
JOIN pg_catalog.pg_partitioned_table pt ON pt.partrelid = p.oid
CROSS JOIN impact
