DECLARE @id int = OBJECT_ID(N'__OBJECT__', N'U');
DECLARE @name nvarchar(520) = QUOTENAME(OBJECT_SCHEMA_NAME(@id)) + N'.' + QUOTENAME(OBJECT_NAME(@id));
IF @id IS NULL SELECT CAST(NULL AS nvarchar(max));
ELSE IF HAS_PERMS_BY_NAME(@name,N'OBJECT',N'VIEW DEFINITION')<>1
    OR EXISTS (SELECT 1 FROM sys.triggers WHERE parent_id=@id AND OBJECT_DEFINITION(object_id) IS NULL)
    OR EXISTS (SELECT 1 FROM sys.triggers t JOIN sys.sql_modules m ON m.object_id=t.object_id
        WHERE t.parent_id=@id AND (m.uses_ansi_nulls=0 OR m.uses_quoted_identifier=0
        OR UPPER(LTRIM(m.definition)) NOT LIKE N'CREATE%'))
    OR EXISTS (SELECT 1 FROM sys.indexes i JOIN sys.filegroups f ON f.data_space_id=i.data_space_id
        WHERE i.object_id=@id AND i.index_id=0 AND f.is_default=0)
    OR EXISTS (SELECT 1 FROM sys.tables WHERE object_id=@id AND
    (temporal_type<>0 OR is_memory_optimized=1 OR is_filetable=1 OR is_tracked_by_cdc=1 OR is_replicated=1))
    OR EXISTS (SELECT 1 FROM sys.security_predicates WHERE target_object_id=@id)
    OR EXISTS (SELECT 1 FROM sys.columns WHERE object_id=@id AND
        (is_filestream=1 OR is_sparse=1 OR is_column_set=1 OR generated_always_type<>0 OR encryption_type IS NOT NULL OR is_masked=1))
    OR EXISTS (SELECT 1 FROM sys.indexes i LEFT JOIN sys.data_spaces d ON d.data_space_id=i.data_space_id
        WHERE i.object_id=@id AND (i.type NOT IN (0,1,2) OR i.is_disabled=1 OR i.is_hypothetical=1 OR d.type=N'PS'))
    OR EXISTS (SELECT 1 FROM sys.partitions WHERE object_id=@id AND data_compression<>0)
    OR EXISTS (SELECT 1 FROM sys.foreign_keys WHERE parent_object_id=@id AND (is_disabled=1 OR is_not_trusted=1 OR is_not_for_replication=1))
    OR EXISTS (SELECT 1 FROM sys.check_constraints WHERE parent_object_id=@id AND (is_disabled=1 OR is_not_trusted=1 OR is_not_for_replication=1))
    OR EXISTS (SELECT 1 FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id
        WHERE c.object_id=@id AND (t.is_assembly_type=1 OR c.xml_collection_id<>0 OR c.rule_object_id<>0))
    SELECT N'sift:unsupported:table policies, temporal/memory/replication, advanced columns, storage, indexes or constraint states';
ELSE BEGIN
    DECLARE @ddl nvarchar(max);
    SELECT @ddl=N'SET ANSI_NULLS ON; SET QUOTED_IDENTIFIER ON;'+CHAR(10)+N'CREATE TABLE '+@name+N' ('+CHAR(10)+STRING_AGG(CAST(N'    '+QUOTENAME(c.name)+
        CASE WHEN c.is_computed=1 THEN N' AS '+cc.definition+CASE WHEN cc.is_persisted=1 THEN N' PERSISTED' ELSE N'' END
             +CASE WHEN c.is_nullable=0 AND cc.is_persisted=1 THEN N' NOT NULL' ELSE N'' END
        ELSE N' '+CASE WHEN ty.is_user_defined=1 THEN QUOTENAME(SCHEMA_NAME(ty.schema_id))+N'.'+QUOTENAME(ty.name)
            ELSE QUOTENAME(ty.name)+CASE
                WHEN ty.name IN (N'varchar',N'char',N'varbinary',N'binary') THEN N'('+CASE WHEN c.max_length=-1 THEN N'max' ELSE CONVERT(nvarchar(10),c.max_length) END+N')'
                WHEN ty.name IN (N'nvarchar',N'nchar') THEN N'('+CASE WHEN c.max_length=-1 THEN N'max' ELSE CONVERT(nvarchar(10),c.max_length/2) END+N')'
                WHEN ty.name IN (N'decimal',N'numeric') THEN N'('+CONVERT(nvarchar(10),c.precision)+N','+CONVERT(nvarchar(10),c.scale)+N')'
                WHEN ty.name IN (N'datetime2',N'datetimeoffset',N'time') THEN N'('+CONVERT(nvarchar(10),c.scale)+N')'
                WHEN ty.name=N'float' THEN N'('+CONVERT(nvarchar(10),c.precision)+N')' ELSE N'' END END+
            CASE WHEN c.collation_name IS NOT NULL THEN N' COLLATE '+c.collation_name ELSE N'' END+
            CASE WHEN c.is_identity=1 THEN N' IDENTITY('+CONVERT(nvarchar(100),ic.seed_value)+N','+CONVERT(nvarchar(100),ic.increment_value)+N')'+CASE WHEN ic.is_not_for_replication=1 THEN N' NOT FOR REPLICATION' ELSE N'' END ELSE N'' END+
            CASE WHEN c.is_rowguidcol=1 THEN N' ROWGUIDCOL' ELSE N'' END+
            CASE WHEN dc.definition IS NOT NULL THEN N' CONSTRAINT '+QUOTENAME(dc.name)+N' DEFAULT '+dc.definition ELSE N'' END+
            CASE WHEN c.is_nullable=1 THEN N' NULL' ELSE N' NOT NULL' END END AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N','+CHAR(10)) WITHIN GROUP (ORDER BY c.column_id)
    FROM sys.columns c JOIN sys.types ty ON ty.user_type_id=c.user_type_id
    LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
    LEFT JOIN sys.identity_columns ic ON ic.object_id=c.object_id AND ic.column_id=c.column_id
    LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id WHERE c.object_id=@id;
    SELECT @ddl=@ddl+COALESCE((SELECT N','+CHAR(10)+STRING_AGG(CAST(N'    CONSTRAINT '+QUOTENAME(name)+N' CHECK '+definition AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N','+CHAR(10)) FROM sys.check_constraints WHERE parent_object_id=@id),N'');
    SELECT @ddl=@ddl+N');'+CHAR(10);
    SELECT @ddl=@ddl+COALESCE(STRING_AGG(CAST(
        CASE WHEN i.is_primary_key=1 OR i.is_unique_constraint=1 THEN N'ALTER TABLE '+@name+N' ADD CONSTRAINT '+QUOTENAME(i.name)+CASE WHEN i.is_primary_key=1 THEN N' PRIMARY KEY ' ELSE N' UNIQUE ' END
        ELSE N'CREATE '+CASE WHEN i.is_unique=1 THEN N'UNIQUE ' ELSE N'' END END+
        CASE WHEN i.type=1 THEN N'CLUSTERED ' ELSE N'NONCLUSTERED ' END+
        CASE WHEN i.is_primary_key=1 OR i.is_unique_constraint=1 THEN N'' ELSE N'INDEX '+QUOTENAME(i.name)+N' ON '+@name+N' ' END+
        N'('+keys.cols+N')'+COALESCE(N' INCLUDE ('+inc.cols+N')',N'')+
        COALESCE(N' WHERE '+i.filter_definition,N'')+N' WITH (PAD_INDEX = '+CASE WHEN i.is_padded=1 THEN N'ON' ELSE N'OFF' END+
        CASE WHEN i.fill_factor=0 THEN N'' ELSE N', FILLFACTOR = '+CONVERT(nvarchar(10),i.fill_factor) END+N', IGNORE_DUP_KEY = '+CASE WHEN i.ignore_dup_key=1 THEN N'ON' ELSE N'OFF' END+
        N', ALLOW_ROW_LOCKS = '+CASE WHEN i.allow_row_locks=1 THEN N'ON' ELSE N'OFF' END+
        N', ALLOW_PAGE_LOCKS = '+CASE WHEN i.allow_page_locks=1 THEN N'ON' ELSE N'OFF' END+N')'+
        COALESCE(N' ON '+QUOTENAME(ds.name),N'')+N';' AS nvarchar(max)) COLLATE DATABASE_DEFAULT,CHAR(10)),N'')
    FROM sys.indexes i LEFT JOIN sys.data_spaces ds ON ds.data_space_id=i.data_space_id
    CROSS APPLY (SELECT STRING_AGG(CAST(QUOTENAME(c.name)+CASE WHEN ic.is_descending_key=1 THEN N' DESC' ELSE N' ASC' END AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N', ') WITHIN GROUP (ORDER BY ic.key_ordinal) cols
        FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
        WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id AND ic.key_ordinal>0) keys
    CROSS APPLY (SELECT STRING_AGG(CAST(QUOTENAME(c.name) AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N', ') WITHIN GROUP (ORDER BY ic.index_column_id) cols
        FROM sys.index_columns ic JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
        WHERE ic.object_id=i.object_id AND ic.index_id=i.index_id AND ic.is_included_column=1) inc
    WHERE i.object_id=@id AND i.type IN (1,2);
    SELECT @ddl=@ddl+COALESCE((SELECT CHAR(10)+STRING_AGG(CAST(N'ALTER TABLE '+@name+N' ADD CONSTRAINT '+QUOTENAME(f.name)+N' FOREIGN KEY ('+src.cols+N') REFERENCES '+QUOTENAME(OBJECT_SCHEMA_NAME(f.referenced_object_id))+N'.'+QUOTENAME(OBJECT_NAME(f.referenced_object_id))+N' ('+dst.cols+N') ON DELETE '+REPLACE(f.delete_referential_action_desc,N'_',N' ')+N' ON UPDATE '+REPLACE(f.update_referential_action_desc,N'_',N' ')+N';' AS nvarchar(max)) COLLATE DATABASE_DEFAULT,CHAR(10))
        FROM sys.foreign_keys f
        CROSS APPLY (SELECT STRING_AGG(CAST(QUOTENAME(COL_NAME(parent_object_id,parent_column_id)) AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N', ') WITHIN GROUP (ORDER BY constraint_column_id) cols FROM sys.foreign_key_columns WHERE constraint_object_id=f.object_id) src
        CROSS APPLY (SELECT STRING_AGG(CAST(QUOTENAME(COL_NAME(referenced_object_id,referenced_column_id)) AS nvarchar(max)) COLLATE DATABASE_DEFAULT,N', ') WITHIN GROUP (ORDER BY constraint_column_id) cols FROM sys.foreign_key_columns WHERE constraint_object_id=f.object_id) dst
        WHERE f.parent_object_id=@id),N'');
    SELECT @ddl=@ddl+COALESCE((SELECT CHAR(10)+N'GO'+CHAR(10)+STRING_AGG(CAST(OBJECT_DEFINITION(object_id)+CHAR(10)+N'GO'+CHAR(10)+CASE WHEN is_disabled=1 THEN N'DISABLE TRIGGER '+QUOTENAME(OBJECT_SCHEMA_NAME(object_id))+N'.'+QUOTENAME(name)+N' ON '+@name+N';'+CHAR(10)+N'GO'+CHAR(10) ELSE N'' END AS nvarchar(max)) COLLATE DATABASE_DEFAULT,CHAR(10)+N'GO'+CHAR(10)) FROM sys.triggers WHERE parent_id=@id),N'');
    SELECT @ddl;
END
