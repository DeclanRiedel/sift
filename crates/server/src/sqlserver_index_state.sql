SET ROWCOUNT 0; SET NOCOUNT ON;
DECLARE @object int=OBJECT_ID(QUOTENAME(@P1)+N'.'+QUOTENAME(@P2),N'U');
DECLARE @index int=(SELECT index_id FROM sys.indexes WHERE object_id=@object AND name=@P3);
DECLARE @authority bit=CONVERT(bit,COALESCE(HAS_PERMS_BY_NAME(QUOTENAME(@P1)+N'.'+QUOTENAME(@P2),N'OBJECT',N'ALTER'),0));
DECLARE @visible bit=CONVERT(bit,COALESCE(HAS_PERMS_BY_NAME(QUOTENAME(@P1)+N'.'+QUOTENAME(@P2),N'OBJECT',N'VIEW DEFINITION'),0));
DECLARE @supported bit=0,@pages bit=0;
SELECT @supported=CONVERT(bit,CASE WHEN DB_ID()>4 AND DATABASEPROPERTYEX(DB_NAME(),'Updateability')='READ_WRITE'
 AND t.is_ms_shipped=0 AND t.is_memory_optimized=0 AND t.temporal_type=0 AND t.is_filetable=0
 AND t.is_node=0 AND t.is_edge=0 AND t.is_replicated=0 AND t.is_merge_published=0 AND t.is_tracked_by_cdc=0
 AND i.type IN (1,2) AND i.is_disabled=0 AND i.is_hypothetical=0 AND fg.is_read_only=0
 AND (SELECT COUNT_BIG(*) FROM sys.partitions p WHERE p.object_id=t.object_id AND p.index_id=i.index_id)=1
 AND (SELECT COUNT_BIG(*) FROM sys.columns WHERE object_id=t.object_id)<=200
 AND (SELECT COUNT_BIG(*) FROM sys.index_columns WHERE object_id=t.object_id AND index_id=i.index_id)<=200
 AND NOT EXISTS(SELECT 1 FROM sys.computed_columns WHERE object_id=t.object_id AND (definition IS NULL OR DATALENGTH(definition)>8192))
 AND (i.has_filter=0 OR (i.filter_definition IS NOT NULL AND DATALENGTH(i.filter_definition)<=8192))
 THEN 1 ELSE 0 END),@pages=i.allow_page_locks
 FROM sys.tables t JOIN sys.indexes i ON i.object_id=t.object_id AND i.index_id=@index
 JOIN sys.filegroups fg ON fg.data_space_id=i.data_space_id
 WHERE t.object_id=@object;
DECLARE @state nvarchar(max)=(SELECT
 CONVERT(nvarchar(128),SERVERPROPERTY('ServerName')) AS server_name,DB_ID() AS database_id,DB_NAME() AS database_name,
 (SELECT create_date FROM sys.databases WHERE database_id=DB_ID()) AS database_created,
 CONVERT(varchar(172),SUSER_SID(),1) AS login_sid,USER_ID() AS user_id,
 (SELECT CONVERT(varchar(172),sid,1) FROM sys.database_principals WHERE principal_id=USER_ID()) AS user_sid,
 @authority AS authority,@visible AS visible,
 JSON_QUERY((SELECT object_id,schema_id,name,create_date,modify_date,is_memory_optimized,temporal_type,is_filetable,is_node,is_edge,is_replicated,is_merge_published,is_tracked_by_cdc FROM sys.tables WHERE object_id=@object FOR JSON PATH)) AS target,
 JSON_QUERY((SELECT index_id,name,type,is_unique,is_primary_key,is_unique_constraint,is_disabled,is_hypothetical,data_space_id,fill_factor,is_padded,ignore_dup_key,allow_row_locks,allow_page_locks,has_filter,LEFT(filter_definition,4096) AS filter_definition FROM sys.indexes WHERE object_id=@object AND index_id=@index FOR JSON PATH)) AS index_shape,
 JSON_QUERY((SELECT TOP(201) c.column_id,c.name,c.system_type_id,c.user_type_id,c.max_length,c.precision,c.scale,c.is_nullable,c.is_computed,c.collation_name,LEFT(cc.definition,4096) AS expression,cc.is_persisted FROM sys.columns c LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id WHERE c.object_id=@object ORDER BY c.column_id FOR JSON PATH)) AS columns_shape,
 JSON_QUERY((SELECT TOP(201) index_column_id,column_id,key_ordinal,is_descending_key,is_included_column,partition_ordinal FROM sys.index_columns WHERE object_id=@object AND index_id=@index ORDER BY index_column_id FOR JSON PATH)) AS keys_shape,
 JSON_QUERY((SELECT TOP(2) partition_id,hobt_id,partition_number,data_compression FROM sys.partitions WHERE object_id=@object AND index_id=@index ORDER BY partition_number FOR JSON PATH)) AS physical_shape,
 JSON_QUERY((SELECT stats_id,name,no_recompute,has_filter,LEFT(filter_definition,4096) AS filter_definition,STATS_DATE(object_id,stats_id) AS updated_at FROM sys.stats WHERE object_id=@object AND stats_id=@index FOR JSON PATH)) AS stats_shape
 FOR JSON PATH,WITHOUT_ARRAY_WRAPPER);
SELECT @authority,@visible,CONVERT(bit,CASE WHEN DATALENGTH(@state)<=1048576 THEN @supported ELSE 0 END),@pages,
 CONVERT(nvarchar(128),DB_NAME()),CONVERT(varchar(64),HASHBYTES('SHA2_256',@state),2);
