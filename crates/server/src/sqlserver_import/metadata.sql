SELECT TOP (2001)
 CONVERT(nvarchar(128),SERVERPROPERTY('ServerName')) AS [target.server],DB_NAME() AS [target.database],DB_ID() AS [target.database_id],
 CONVERT(varchar(170),SUSER_SID(),2) AS [target.login_sid],USER_ID() AS [target.user_id],CONVERT(varchar(170),(SELECT sid FROM sys.database_principals WHERE principal_id=USER_ID()),2) AS [target.user_sid],
 s.name AS [target.schema],s.schema_id AS [target.schema_id],s.principal_id AS [target.schema_owner],t.name AS [target.table],t.object_id AS [target.object_id],
 CONVERT(varchar(33),t.create_date,126) AS [target.created],CONVERT(varchar(33),t.modify_date,126) AS [target.modified],
 CONVERT(bit,COALESCE(HAS_PERMS_BY_NAME(QUOTENAME(s.name)+'.'+QUOTENAME(t.name),'OBJECT','ALTER'),0)) AS [target.can_alter],
 @@LANGUAGE AS [target.language],(SELECT date_format FROM sys.dm_exec_sessions WHERE session_id=@@SPID) AS [target.date_format],
 t.is_memory_optimized AS [target.memory_optimized],t.temporal_type AS [target.temporal],t.is_filetable AS [target.file_table],t.is_ms_shipped AS [target.system_table],
 c.column_id,c.name,c.system_type_id AS system_type,c.user_type_id AS user_type,TYPE_NAME(c.user_type_id) AS native_type_name,c.max_length,c.precision,c.scale,c.collation_name AS collation,c.is_nullable AS nullable,
 c.is_identity AS [identity],c.is_computed AS computed,c.generated_always_type AS generated,c.is_hidden AS hidden,c.is_column_set AS column_set,
 CONVERT(nvarchar(128),ic.seed_value) AS seed,CONVERT(nvarchar(128),ic.increment_value) AS increment,c.default_object_id AS default_id,
 CONVERT(varchar(64),HASHBYTES('SHA2_256',CONVERT(nvarchar(max),cc.definition)),2) AS computed_hash,
 CONVERT(bit,COALESCE(HAS_PERMS_BY_NAME(QUOTENAME(s.name)+'.'+QUOTENAME(t.name),'OBJECT','INSERT'),0)) AS can_insert,
 {ordinal} AS source_ordinal,({matches}) AS matches
FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id JOIN sys.columns c ON c.object_id=t.object_id
LEFT JOIN sys.identity_columns ic ON ic.object_id=c.object_id AND ic.column_id=c.column_id
LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
WHERE t.object_id=OBJECT_ID(@P1,'U') AND (PARSENAME(@P1,3) IS NULL OR DB_ID(PARSENAME(@P1,3))=DB_ID())
ORDER BY c.column_id FOR JSON PATH,INCLUDE_NULL_VALUES
