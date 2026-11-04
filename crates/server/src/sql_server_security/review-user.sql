WITH owned AS (
 SELECT N'SCHEMA' AS kind, schema_id AS id, name FROM sys.schemas WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'PRINCIPAL', principal_id, name FROM sys.database_principals WHERE owning_principal_id=USER_ID(@P1)
 UNION ALL SELECT N'OBJECT', object_id, name FROM sys.objects WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'TYPE', user_type_id, name FROM sys.types WHERE principal_id=USER_ID(@P1) AND is_user_defined=1
 UNION ALL SELECT N'XML', xml_collection_id, name FROM sys.xml_schema_collections WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'ASSEMBLY', assembly_id, name FROM sys.assemblies WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'CERTIFICATE', certificate_id, name FROM sys.certificates WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'ASYMMETRIC_KEY', asymmetric_key_id, name FROM sys.asymmetric_keys WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'SYMMETRIC_KEY', symmetric_key_id, name FROM sys.symmetric_keys WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'SERVICE', service_id, name FROM sys.services WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'CONTRACT', service_contract_id, name FROM sys.service_contracts WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'MESSAGE', message_type_id, name FROM sys.service_message_types WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'ROUTE', route_id, name FROM sys.routes WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'REMOTE_BINDING', remote_service_binding_id, name FROM sys.remote_service_bindings WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'FULLTEXT_CATALOG', fulltext_catalog_id, name FROM sys.fulltext_catalogs WHERE principal_id=USER_ID(@P1)
 UNION ALL SELECT N'STOPLIST', stoplist_id, name FROM sys.fulltext_stoplists WHERE principal_id=USER_ID(@P1)
), affected_permissions AS (
 SELECT * FROM sys.database_permissions WHERE grantee_principal_id=USER_ID(@P1)
 OR grantor_principal_id=USER_ID(@P1) OR (class=4 AND major_id=USER_ID(@P1))
), memberships AS (
 SELECT role_principal_id,member_principal_id FROM sys.database_role_members
 WHERE member_principal_id=USER_ID(@P1)
)
SELECT
 CONVERT(bit,CASE
 WHEN @P3=N'create' AND p.principal_id IS NULL AND l.principal_id IS NOT NULL AND s.schema_id IS NOT NULL THEN 1
 WHEN @P3=N'remap' AND p.principal_id>4 AND p.type='S' AND p.authentication_type=1 AND l.principal_id IS NOT NULL AND p.principal_id<>USER_ID() THEN 1
 WHEN @P3=N'schema' AND p.principal_id>4 AND p.type='S' AND p.authentication_type IN (0,1) AND s.schema_id IS NOT NULL THEN 1
 WHEN @P3=N'drop' AND p.principal_id>4 AND p.type='S' AND p.authentication_type IN (0,1) AND p.principal_id<>USER_ID() THEN 1
 ELSE 0 END),
 CONVERT(bit,CASE
 WHEN @P3=N'create' AND (HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','ALTER ANY USER')=1 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','CREATE USER')=1) THEN 1
 WHEN @P3=N'remap' AND HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','CONTROL')=1 THEN 1
 WHEN @P3=N'schema' AND (p.principal_id=USER_ID() OR HAS_PERMS_BY_NAME(QUOTENAME(@P1),'USER','ALTER')=1) THEN 1
 WHEN @P3=N'drop' AND HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','ALTER ANY USER')=1 THEN 1
 ELSE 0 END),
 CONVERT(bit,CASE WHEN @P3=N'schema' OR USER_ID()=1 OR (
 HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW DEFINITION')=1
 AND (CONVERT(int,SERVERPROPERTY('ProductMajorVersion'))<16 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','VIEW SECURITY DEFINITION')=1)
 AND NOT EXISTS(SELECT 1 FROM sys.database_permissions denied
 WHERE denied.state='D' AND denied.grantee_principal_id IN (SELECT principal_id FROM sys.user_token)
 AND denied.permission_name IN ('CONTROL','VIEW DEFINITION','VIEW SECURITY DEFINITION'))) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN @P3 IN (N'create',N'remap') AND (l.sid=d.owner_sid OR EXISTS(
 SELECT 1 FROM sys.database_principals mapped WHERE mapped.sid=l.sid
 AND (p.principal_id IS NULL OR mapped.principal_id<>p.principal_id))) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN (@P3=N'remap' AND p.sid=l.sid)
 OR (@P3=N'schema' AND p.default_schema_name=s.name) THEN 1 ELSE 0 END),
 (SELECT COUNT_BIG(*) FROM owned),
 (SELECT COUNT_BIG(*) FROM memberships),
 (SELECT COUNT_BIG(*) FROM affected_permissions),
 CONVERT(bit,COALESCE(l.is_disabled,0)),
 CONVERT(nvarchar(64),CONVERT(varchar(64),HASHBYTES('SHA2_256',
 (SELECT CONVERT(nvarchar(128),@@SERVERNAME) AS server_name,
 d.database_id,d.name AS database_name,d.create_date AS database_created,d.owner_sid AS database_owner,d.collation_name,
 CURRENT_USER AS actor,SUSER_SNAME() AS login_name,SUSER_SID() AS login_sid,ORIGINAL_LOGIN() AS original_login,
 (SELECT u.principal_id,u.sid,u.name,u.type,u.owning_principal_id,u.is_fixed_role,u.create_date,u.modify_date,u.authentication_type,u.default_schema_name
 FROM sys.database_principals u WHERE u.principal_id=USER_ID() OR u.name=@P1
 OR (l.sid IS NOT NULL AND u.sid=l.sid) ORDER BY u.principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS principals,
 (SELECT sp.principal_id,sp.sid,sp.name,sp.type,sp.create_date,sp.modify_date,sp.is_disabled
 FROM sys.server_principals sp WHERE sp.principal_id=l.principal_id OR sp.sid=SUSER_SID()
 ORDER BY sp.principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS logins,
 (SELECT sc.schema_id,sc.name,sc.principal_id,owner.sid AS owner_sid,owner.create_date AS owner_created,owner.modify_date AS owner_modified
 FROM sys.schemas sc JOIN sys.database_principals owner ON owner.principal_id=sc.principal_id
 WHERE sc.schema_id=s.schema_id OR sc.name=p.default_schema_name ORDER BY sc.schema_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS schemas,
 (SELECT kind,id,name FROM owned ORDER BY kind,id FOR JSON PATH,INCLUDE_NULL_VALUES) AS owned,
 (SELECT * FROM memberships ORDER BY role_principal_id,member_principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS memberships,
 (SELECT * FROM affected_permissions ORDER BY class,major_id,minor_id,grantee_principal_id,grantor_principal_id,type FOR JSON PATH,INCLUDE_NULL_VALUES) AS permissions
 FOR JSON PATH,INCLUDE_NULL_VALUES)),2))
FROM sys.databases d
LEFT JOIN sys.database_principals p ON p.name=@P1
LEFT JOIN sys.server_principals l ON @P3 IN (N'create',N'remap') AND l.name=@P2 AND l.type='S'
LEFT JOIN sys.schemas s ON @P3 IN (N'create',N'schema') AND s.name=CASE WHEN @P3=N'create' THEN N'dbo' ELSE @P2 END
WHERE d.database_id=DB_ID()
