WITH permissions AS (
 SELECT p.* FROM sys.database_permissions p JOIN sys.schemas s ON s.schema_id=p.major_id JOIN sys.database_principals u ON u.principal_id=p.grantee_principal_id
 WHERE p.class=3 AND s.name=@P1 AND u.name=@P2 AND p.permission_name='SELECT'
)
SELECT
 CONVERT(bit,CASE WHEN EXISTS(SELECT 1 FROM sys.schemas WHERE name=@P1) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name=@P2) THEN 1 ELSE 0 END),
 (SELECT CASE WHEN COUNT(*)=0 THEN N'' WHEN COUNT(*)=1 THEN MAX(CONVERT(nvarchar(60),state_desc)) ELSE N'MULTIPLE_GRANTORS' END FROM permissions),
 CONVERT(bit,CASE WHEN HAS_PERMS_BY_NAME(QUOTENAME(@P1),'SCHEMA','CONTROL')=1 THEN 1 ELSE 0 END),
 CONVERT(nvarchar(64),CONVERT(varchar(64),HASHBYTES('SHA2_256',
 (SELECT CONVERT(nvarchar(128),@@SERVERNAME) AS server_name,
 d.database_id,d.name AS database_name,d.create_date AS database_created,d.owner_sid AS database_owner,d.collation_name,
 CURRENT_USER AS actor,SUSER_SNAME() AS login_name,SUSER_SID() AS login_sid,ORIGINAL_LOGIN() AS original_login,
 (SELECT s.schema_id,s.name,s.principal_id FROM sys.schemas s WHERE s.name=@P1 FOR JSON PATH) AS target_schema,
 (SELECT p.principal_id,p.sid,p.name,p.type,p.owning_principal_id,p.is_fixed_role,p.create_date,p.modify_date,p.authentication_type FROM sys.database_principals p
 WHERE p.principal_id=USER_ID() OR p.name=@P2 OR p.principal_id IN (SELECT principal_id FROM sys.schemas WHERE name=@P1) OR p.principal_id IN (SELECT grantor_principal_id FROM permissions)
 ORDER BY p.principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS principals,
 (SELECT * FROM permissions ORDER BY grantee_principal_id,grantor_principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS permissions
 FOR JSON PATH,INCLUDE_NULL_VALUES)),2))
FROM sys.databases d WHERE d.database_id=DB_ID()
