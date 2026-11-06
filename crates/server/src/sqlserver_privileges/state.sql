SET NOCOUNT ON;
SET ROWCOUNT 0;
DECLARE @subject sysname=COALESCE(@P1,USER_NAME()), @actor sysname=USER_NAME(),
 @actor_id int=USER_ID(), @database sysname=DB_NAME(), @database_id int=DB_ID(),
 @subject_id int, @sid varbinary(85), @created datetime, @actor_sid varbinary(85), @actor_created datetime, @impersonated bit=0,
 @switched bit=0, @objects_json nvarchar(max), @owner varbinary(85);
IF NOT (USER_ID()=1 OR (
 HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW DEFINITION')=1
 AND (CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW SECURITY DEFINITION')=1)
 AND NOT EXISTS(SELECT 1 FROM sys.database_permissions d WHERE d.state='D'
 AND d.grantee_principal_id IN (SELECT principal_id FROM sys.user_token)
 AND d.permission_name IN ('CONTROL','VIEW DEFINITION','VIEW SECURITY DEFINITION'))))
 THROW 50001,'Effective-permission inspection requires complete database definition and security metadata authority',1;
SELECT @actor_sid=sid,@actor_created=create_date FROM sys.database_principals WHERE principal_id=@actor_id;
SELECT @subject=name,@subject_id=principal_id,@sid=sid,@created=create_date
 FROM sys.database_principals WHERE name=@subject;
IF @subject_id IS NULL OR (@subject_id<>@actor_id AND NOT EXISTS(
 SELECT 1 FROM sys.database_principals WHERE principal_id=@subject_id AND principal_id>4
 AND type='S' AND authentication_type IN (0,1)))
 THROW 50002,'Select the connected actor or an ordinary SQL database user',1;
SELECT @owner=owner_sid FROM sys.databases WHERE database_id=@database_id;
DECLARE @objects TABLE (
 ordinal bigint, kind nvarchar(16), object_id int, column_id int, schema_id int,
 schema_name sysname NULL, relation sysname NULL, name sysname,
 qualified nvarchar(517), class nvarchar(16), created datetime NULL,
 modified datetime NULL, owner_id int NULL
);
WITH native_objects AS (
 SELECT 0 rank,N'database' kind,@database_id object_id,0 column_id,0 schema_id,
 CAST(NULL AS sysname) schema_name,CAST(NULL AS sysname) relation,@database name,
 CONVERT(nvarchar(517),QUOTENAME(@database)) qualified,N'DATABASE' class,
 CAST(NULL AS datetime) created,CAST(NULL AS datetime) modified,CAST(NULL AS int) owner_id
 UNION ALL
 SELECT 1,N'schema',schema_id,0,schema_id,name,NULL,name,QUOTENAME(name),N'SCHEMA',NULL,NULL,principal_id
 FROM sys.schemas WHERE name NOT IN (N'sys',N'INFORMATION_SCHEMA') AND schema_id<16384
 UNION ALL
 SELECT 2,CASE o.type WHEN 'U' THEN N'table' WHEN 'V' THEN N'view' WHEN 'SO' THEN N'sequence' ELSE N'routine' END,
 o.object_id,0,o.schema_id,s.name,NULL,o.name,QUOTENAME(s.name)+N'.'+QUOTENAME(o.name),N'OBJECT',o.create_date,o.modify_date,o.principal_id
 FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
 WHERE o.is_ms_shipped=0 AND o.type IN ('U','V','SO','P','FN','IF','TF') AND s.name NOT IN (N'sys',N'INFORMATION_SCHEMA')
 UNION ALL
 SELECT 3,N'column',o.object_id,c.column_id,o.schema_id,s.name,o.name,c.name,QUOTENAME(s.name)+N'.'+QUOTENAME(o.name),N'OBJECT',o.create_date,o.modify_date,o.principal_id
 FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id JOIN sys.columns c ON c.object_id=o.object_id
 WHERE o.is_ms_shipped=0 AND o.type IN ('U','V') AND s.name NOT IN (N'sys',N'INFORMATION_SCHEMA')
), numbered AS (
 SELECT ROW_NUMBER() OVER(ORDER BY rank,object_id,column_id) ordinal,* FROM native_objects
)
INSERT @objects SELECT ordinal,kind,object_id,column_id,schema_id,schema_name,relation,name,qualified,class,created,modified,owner_id
 FROM numbered ORDER BY ordinal OFFSET @P2 ROWS FETCH NEXT @P3 ROWS ONLY;
BEGIN TRY
 IF @subject_id<>@actor_id BEGIN
  EXECUTE AS USER=@subject;
  SET @switched=1; SET @impersonated=1;
 END;
 IF USER_ID()<>@subject_id OR NOT EXISTS(SELECT 1 FROM sys.database_principals
 WHERE principal_id=@subject_id AND sid=@sid AND create_date=@created)
  THROW 50003,'Database principal identity changed during inspection',1;
 SELECT @objects_json=(
  SELECT o.kind,o.object_id,NULLIF(o.column_id,0) column_number,o.schema_name [schema],o.relation,o.name,
   JSON_QUERY((SELECT p.permission privilege,
    CONVERT(bit,CASE WHEN o.kind=N'column'
     THEN HAS_PERMS_BY_NAME(o.qualified,o.class,p.permission,QUOTENAME(o.name),N'COLUMN')
     ELSE HAS_PERMS_BY_NAME(o.qualified,o.class,p.permission) END) allowed
    FROM (VALUES
     (N'CONNECT',N'database'),(N'CREATE TABLE',N'database'),(N'CREATE VIEW',N'database'),
     (N'CREATE PROCEDURE',N'database'),(N'ALTER ANY SCHEMA',N'database'),
     (N'SELECT',N'database'),(N'INSERT',N'database'),(N'UPDATE',N'database'),(N'DELETE',N'database'),(N'EXECUTE',N'database'),(N'REFERENCES',N'database'),
     (N'SELECT',N'schema'),(N'INSERT',N'schema'),(N'UPDATE',N'schema'),(N'DELETE',N'schema'),(N'EXECUTE',N'schema'),(N'REFERENCES',N'schema'),
     (N'SELECT',N'table'),(N'INSERT',N'table'),(N'UPDATE',N'table'),(N'DELETE',N'table'),(N'REFERENCES',N'table'),
     (N'SELECT',N'view'),(N'INSERT',N'view'),(N'UPDATE',N'view'),(N'DELETE',N'view'),(N'REFERENCES',N'view'),
     (N'SELECT',N'column'),(N'UPDATE',N'column'),(N'REFERENCES',N'column'),
     (N'SELECT',N'sequence'),(N'UPDATE',N'sequence'),(N'EXECUTE',N'routine'),(N'SELECT',N'routine'),(N'REFERENCES',N'routine'),
     (N'CONTROL',N'all'),(N'ALTER',N'all'),(N'VIEW DEFINITION',N'all'),(N'TAKE OWNERSHIP',N'all')
    ) p(permission,kind)
    WHERE p.kind=o.kind OR (p.kind=N'all' AND o.kind<>N'column' AND (o.kind<>N'database' OR p.permission<>N'TAKE OWNERSHIP'))
    ORDER BY p.permission FOR JSON PATH,INCLUDE_NULL_VALUES)) checks
   FROM @objects o ORDER BY o.ordinal FOR JSON PATH,INCLUDE_NULL_VALUES);
 IF @switched=1 BEGIN REVERT; SET @switched=0; END;
END TRY
BEGIN CATCH
 IF @switched=1 REVERT;
 THROW;
END CATCH;
IF NOT (USER_ID()=1 OR (
 HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW DEFINITION')=1
 AND (CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW SECURITY DEFINITION')=1)
 AND NOT EXISTS(SELECT 1 FROM sys.database_permissions d WHERE d.state='D'
 AND d.grantee_principal_id IN (SELECT principal_id FROM sys.user_token)
 AND d.permission_name IN ('CONTROL','VIEW DEFINITION','VIEW SECURITY DEFINITION'))))
 THROW 50001,'Effective-permission inspection requires complete database definition and security metadata authority',1;
IF DB_ID()<>@database_id OR DB_NAME()<>@database OR USER_ID()<>@actor_id
 OR NOT EXISTS(SELECT 1 FROM sys.database_principals WHERE principal_id=@actor_id AND name=@actor AND sid=@actor_sid AND create_date=@actor_created)
 OR NOT EXISTS(SELECT 1 FROM sys.databases WHERE database_id=@database_id AND owner_sid=@owner)
 OR NOT EXISTS(SELECT 1 FROM sys.database_principals WHERE principal_id=@subject_id AND name=@subject AND sid=@sid AND create_date=@created)
 THROW 50003,'Database or principal identity changed during inspection',1;
IF EXISTS(SELECT 1 FROM @objects r WHERE
 (r.kind=N'schema' AND NOT EXISTS(SELECT 1 FROM sys.schemas s WHERE s.schema_id=r.schema_id AND s.name=r.name AND s.principal_id=r.owner_id))
 OR (r.class=N'OBJECT' AND NOT EXISTS(SELECT 1 FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
 WHERE o.object_id=r.object_id AND o.create_date=r.created AND o.modify_date=r.modified
 AND o.schema_id=r.schema_id AND s.name=r.schema_name AND ISNULL(o.principal_id,-1)=ISNULL(r.owner_id,-1)
 AND ((r.kind<>N'column' AND o.name=r.name) OR (r.kind=N'column' AND o.name=r.relation
 AND EXISTS(SELECT 1 FROM sys.columns c WHERE c.object_id=r.object_id AND c.column_id=r.column_id AND c.name=r.name))))))
 THROW 50003,'Catalog identity changed during inspection',1;
SELECT (SELECT @database [database],@actor actor,@subject principal,@subject_id principal_id,
 @impersonated database_user_token,JSON_QUERY(@objects_json) objects
 FOR JSON PATH,WITHOUT_ARRAY_WRAPPER,INCLUDE_NULL_VALUES);
