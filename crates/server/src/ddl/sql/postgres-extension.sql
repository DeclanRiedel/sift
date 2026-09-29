SELECT CASE
    WHEN NOT (pg_catalog.pg_has_role(current_user, e.extowner, 'USAGE')
              OR current_setting('is_superuser') = 'on')
        THEN 'sift:denied:extension definition requires effective ownership'
    WHEN NOT e.extrelocatable
        THEN 'sift:unsupported:extension has a fixed or non-relocatable schema'
    WHEN e.extconfig IS NOT NULL OR e.extcondition IS NOT NULL
        THEN 'sift:unsupported:extension configuration tables require separate export'
    WHEN EXISTS (
        SELECT 1 FROM pg_catalog.pg_depend d
        WHERE d.classid = 'pg_catalog.pg_extension'::regclass
          AND d.objid = e.oid
          AND d.refclassid = 'pg_catalog.pg_extension'::regclass
          AND d.deptype IN ('n', 'a', 'i', 'e'))
        THEN 'sift:unsupported:extension prerequisites require dependency ordering'
    WHEN EXISTS (
        SELECT 1 FROM pg_catalog.pg_depend d
        WHERE d.refclassid = 'pg_catalog.pg_extension'::regclass
          AND d.refobjid = e.oid AND d.deptype = 'e'
          AND d.classid = 'pg_catalog.pg_class'::regclass)
        THEN 'sift:unsupported:extension relation members require separate fidelity proof'
    WHEN current_setting('server_version_num')::integer / 10000 <> 16
         OR e.extname <> 'pg_trgm' OR e.extversion <> '1.6'
        THEN 'sift:unsupported:extension package has no verified member manifest'
    -- Pristine PostgreSQL 16 pg_trgm 1.6 membership, with its schema
    -- qualified identity normalized so installation in any schema compares.
    WHEN (SELECT md5(string_agg(id.type || '|' ||
                  replace(id.identity, format('%I.', n.nspname), '@.'),
                  E'\n' ORDER BY id.type,
                  replace(id.identity, format('%I.', n.nspname), '@.')))
          FROM pg_catalog.pg_depend d
          CROSS JOIN LATERAL pg_catalog.pg_identify_object(
              d.classid, d.objid, d.objsubid) id
          WHERE d.refclassid = 'pg_catalog.pg_extension'::regclass
            AND d.refobjid = e.oid AND d.deptype = 'e')
         IS DISTINCT FROM 'c5c91c3e917788d034a2bfeea33c4f39'
        THEN 'sift:unsupported:extension member manifest differs from the verified package'
    WHEN NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_available_extension_versions v
        WHERE v.name = e.extname AND v.version = e.extversion
          AND v.relocatable AND coalesce(cardinality(v.requires), 0) = 0)
        THEN 'sift:unsupported:exact relocatable extension package version is unavailable'
    ELSE format('CREATE EXTENSION %I WITH SCHEMA %I VERSION %L;',
        e.extname, n.nspname, e.extversion)
END
FROM pg_catalog.pg_extension e
JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace
WHERE e.extname = '__NAME__' AND n.nspname = '__SCHEMA__'
