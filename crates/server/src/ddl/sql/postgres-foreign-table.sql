WITH target AS (
    SELECT c.* FROM pg_catalog.pg_class c WHERE c.oid = to_regclass('__OBJECT__')
), columns AS (
    SELECT a.attnum, format('%I %s', a.attname, format_type(a.atttypid, a.atttypmod)) ||
        COALESCE((SELECT ' OPTIONS (' || string_agg(format('%I %L', o.option_name, o.option_value), ', ' ORDER BY o.option_name) || ')'
            FROM pg_catalog.pg_options_to_table(a.attfdwoptions) o), '') ||
        CASE WHEN a.attcollation <> 0 AND a.attcollation <> ty.typcollation
            THEN format(' COLLATE %I.%I', cn.nspname, co.collname) ELSE '' END ||
        CASE WHEN d.adbin IS NOT NULL THEN ' DEFAULT ' || pg_get_expr(d.adbin, d.adrelid) ELSE '' END ||
        CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END AS definition
    FROM target t JOIN pg_catalog.pg_attribute a ON a.attrelid=t.oid
    JOIN pg_catalog.pg_type ty ON ty.oid=a.atttypid
    LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
    LEFT JOIN pg_catalog.pg_collation co ON co.oid=a.attcollation
    LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid=co.collnamespace
    WHERE a.attnum>0 AND NOT a.attisdropped
), constraints AS (
    SELECT oid, format('CONSTRAINT %I %s', conname, pg_get_constraintdef(oid)) AS definition
    FROM pg_catalog.pg_constraint WHERE conrelid=(SELECT oid FROM target) AND contype='c'
)
SELECT CASE
    WHEN t.relkind <> 'f' THEN 'sift:unsupported:object is not a foreign table'
    WHEN NOT pg_catalog.pg_has_role(t.relowner, 'USAGE')
        OR NOT pg_catalog.has_server_privilege(s.oid, 'USAGE')
        THEN 'sift:denied:foreign-table definition requires table ownership and server USAGE'
    WHEN t.relispartition OR EXISTS (SELECT 1 FROM pg_catalog.pg_inherits WHERE inhrelid=t.oid)
        OR t.reloptions IS NOT NULL OR t.reltablespace <> 0 OR t.relrowsecurity OR t.relforcerowsecurity
        OR EXISTS (SELECT 1 FROM pg_catalog.pg_policy WHERE polrelid=t.oid)
        OR EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid=t.oid AND contype<>'c')
        OR EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite WHERE ev_class=t.oid AND rulename <> '_RETURN')
        OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type ty ON ty.oid=a.atttypid
            WHERE a.attrelid=t.oid AND a.attnum>0 AND NOT a.attisdropped AND
                (a.attidentity<>'' OR a.attgenerated<>'' OR a.attoptions IS NOT NULL
                OR a.attstorage<>ty.typstorage OR a.attcompression<>''))
        THEN 'sift:unsupported:foreign-table partition, inheritance, storage, or rule shape requires separate export'
    ELSE format('CREATE FOREIGN TABLE %I.%I (', n.nspname,t.relname) || E'\n    ' ||
        COALESCE((SELECT string_agg(definition,E',\n    ' ORDER BY attnum) FROM columns),'') ||
        COALESCE((SELECT E',\n    ' || string_agg(definition,E',\n    ' ORDER BY oid) FROM constraints),'') ||
        E'\n) SERVER ' || quote_ident(s.srvname) ||
        COALESCE((SELECT ' OPTIONS (' || string_agg(format('%I %L', o.option_name, o.option_value), ', ' ORDER BY o.option_name) || ')'
            FROM pg_catalog.pg_options_to_table(f.ftoptions) o), '') || ';' ||
        COALESCE((SELECT E'\n' || string_agg(pg_get_triggerdef(g.oid) || ';' ||
            CASE g.tgenabled WHEN 'D' THEN format(' ALTER TABLE %I.%I DISABLE TRIGGER %I;',n.nspname,t.relname,g.tgname)
            WHEN 'R' THEN format(' ALTER TABLE %I.%I ENABLE REPLICA TRIGGER %I;',n.nspname,t.relname,g.tgname)
            WHEN 'A' THEN format(' ALTER TABLE %I.%I ENABLE ALWAYS TRIGGER %I;',n.nspname,t.relname,g.tgname) ELSE '' END,
            E'\n' ORDER BY g.oid) FROM pg_catalog.pg_trigger g WHERE g.tgrelid=t.oid AND NOT g.tgisinternal),'')
    END
FROM target t JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace
JOIN pg_catalog.pg_foreign_table f ON f.ftrelid=t.oid
JOIN pg_catalog.pg_foreign_server s ON s.oid=f.ftserver
