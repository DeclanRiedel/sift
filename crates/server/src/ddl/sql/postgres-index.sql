SELECT CASE
    WHEN NOT (pg_catalog.pg_has_role(current_user, table_rel.relowner, 'USAGE')
              OR current_setting('is_superuser') = 'on')
        THEN 'sift:denied:index definition requires effective table ownership'
    WHEN index_rel.relkind <> 'i' OR EXISTS (
        SELECT 1 FROM pg_catalog.pg_inherits parent_index
        WHERE parent_index.inhrelid = index_rel.oid)
        THEN 'sift:unsupported:partitioned or attached index belongs to its partition hierarchy'
    WHEN EXISTS (
        SELECT 1 FROM pg_catalog.pg_constraint constraint_info
        WHERE constraint_info.conindid = index_rel.oid)
        THEN 'sift:unsupported:constraint-backed index belongs to its constraint'
    WHEN EXISTS (
        SELECT 1 FROM pg_catalog.pg_depend extension_dep
        WHERE extension_dep.classid = 'pg_catalog.pg_class'::regclass
          AND extension_dep.objid IN (index_rel.oid, table_rel.oid)
          AND extension_dep.deptype = 'e')
        THEN 'sift:unsupported:extension member index or table belongs to its extension'
    WHEN NOT index_info.indisvalid OR NOT index_info.indisready
        THEN 'sift:unsupported:invalid or not-ready index cannot be replayed safely'
    ELSE pg_catalog.pg_get_indexdef(index_rel.oid) || ';' ||
        CASE WHEN index_info.indisclustered THEN format(E'\nALTER TABLE %I.%I CLUSTER ON %I;',
            table_ns.nspname, table_rel.relname, index_rel.relname) ELSE '' END ||
        CASE WHEN index_info.indisreplident THEN format(E'\nALTER TABLE %I.%I REPLICA IDENTITY USING INDEX %I;',
            table_ns.nspname, table_rel.relname, index_rel.relname) ELSE '' END
END
FROM pg_catalog.pg_class index_rel
JOIN pg_catalog.pg_namespace index_ns ON index_ns.oid = index_rel.relnamespace
JOIN pg_catalog.pg_index index_info ON index_info.indexrelid = index_rel.oid
JOIN pg_catalog.pg_class table_rel ON table_rel.oid = index_info.indrelid
JOIN pg_catalog.pg_namespace table_ns ON table_ns.oid = table_rel.relnamespace
WHERE index_ns.nspname = '__SCHEMA__' AND index_rel.relname = '__NAME__'
