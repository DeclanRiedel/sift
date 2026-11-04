WITH target AS (
 SELECT s.schema_id, s.name, s.principal_id AS old_owner, p.principal_id AS new_owner,
 p.sid AS new_sid, p.type AS new_type, p.create_date AS new_created, p.modify_date AS new_modified
 FROM sys.schemas s CROSS JOIN sys.database_principals p
 WHERE s.name=@P1 AND s.schema_id NOT IN (1,3,4) AND p.name=@P2 AND p.type IN ('S','U','G','E','X','C','K','R','A')
), contained AS (
 SELECT 1 AS class, object_id AS id, name, principal_id, type AS kind, create_date, modify_date FROM sys.objects WHERE schema_id=SCHEMA_ID(@P1)
 UNION ALL SELECT 6, user_type_id, name, principal_id, 'TY', NULL, NULL FROM sys.types WHERE schema_id=SCHEMA_ID(@P1) AND is_user_defined=1
 UNION ALL SELECT 10, xml_collection_id, name, principal_id, 'XC', create_date, modify_date FROM sys.xml_schema_collections WHERE schema_id=SCHEMA_ID(@P1)
), permissions AS (
 SELECT p.* FROM sys.database_permissions p
 WHERE (p.class=3 AND p.major_id=SCHEMA_ID(@P1)) OR EXISTS(SELECT 1 FROM contained c WHERE c.class=p.class AND c.id=p.major_id)
)
SELECT CONVERT(bit, CASE WHEN t.old_owner=t.new_owner THEN 1 ELSE 0 END),
 CONVERT(bit, CASE WHEN HAS_PERMS_BY_NAME(QUOTENAME(@P1),'SCHEMA','TAKE OWNERSHIP')=1 THEN 1 ELSE 0 END),
 CONVERT(bit, CASE WHEN t.new_owner=USER_ID()
 OR (t.new_type IN ('S','U','G','E','X','C','K') AND HAS_PERMS_BY_NAME(QUOTENAME(@P2),'USER','IMPERSONATE')=1)
 OR (t.new_type='R' AND (IS_MEMBER(@P2)=1 OR HAS_PERMS_BY_NAME(QUOTENAME(@P2),'ROLE','ALTER')=1))
 OR (t.new_type='A' AND HAS_PERMS_BY_NAME(QUOTENAME(@P2),'APPLICATION ROLE','ALTER')=1) THEN 1 ELSE 0 END),
 CONVERT(bit, CASE WHEN USER_ID()=1 OR (
 HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW DEFINITION')=1
 AND (CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW SECURITY DEFINITION')=1)
 AND NOT EXISTS(SELECT 1 FROM sys.database_permissions denied
 WHERE denied.state='D' AND denied.grantee_principal_id IN (SELECT principal_id FROM sys.user_token)
 AND denied.permission_name IN ('CONTROL','VIEW DEFINITION','VIEW SECURITY DEFINITION')
 AND denied.class IN (0,1,3,4,6,10))) THEN 1 ELSE 0 END),
 CONVERT(nvarchar(64),CONVERT(varchar(64),HASHBYTES('SHA2_256',
 (SELECT CONVERT(nvarchar(128),@@SERVERNAME) AS server_name, DB_ID() AS database_id, DB_NAME() AS database_name,
 CURRENT_USER AS actor, SUSER_SNAME() AS login_name, ORIGINAL_LOGIN() AS original_login,
 t.schema_id,t.name,t.old_owner,t.new_owner,t.new_sid,t.new_type,t.new_created,t.new_modified,
 (SELECT * FROM contained ORDER BY class,id FOR JSON PATH, INCLUDE_NULL_VALUES) AS contained,
 (SELECT * FROM permissions ORDER BY class,major_id,minor_id,grantee_principal_id,grantor_principal_id,type FOR JSON PATH, INCLUDE_NULL_VALUES) AS permissions
 FOR JSON PATH, INCLUDE_NULL_VALUES)),2)),
 (SELECT COUNT_BIG(*) FROM permissions WHERE class=3),
 (SELECT COUNT_BIG(*) FROM permissions p JOIN contained c ON c.class=p.class AND c.id=p.major_id WHERE c.principal_id IS NULL)
FROM target t
