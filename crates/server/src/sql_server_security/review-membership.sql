SELECT
 CONVERT(bit,CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name=@P1 AND type='R' AND is_fixed_role=0) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN EXISTS(SELECT 1 FROM sys.database_principals WHERE name=@P2) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN EXISTS(SELECT 1 FROM sys.database_role_members m JOIN sys.database_principals r ON r.principal_id=m.role_principal_id JOIN sys.database_principals u ON u.principal_id=m.member_principal_id WHERE r.name=@P1 AND u.name=@P2) THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN HAS_PERMS_BY_NAME(QUOTENAME(@P1),'ROLE','ALTER')=1 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','ALTER ANY ROLE')=1 OR IS_MEMBER('db_securityadmin')=1 THEN 1 ELSE 0 END),
 CONVERT(nvarchar(64),CONVERT(varchar(64),HASHBYTES('SHA2_256',
 (SELECT CONVERT(nvarchar(128),@@SERVERNAME) AS server_name,
 d.database_id,d.name AS database_name,d.create_date AS database_created,d.owner_sid AS database_owner,d.collation_name,
 CURRENT_USER AS actor,SUSER_SNAME() AS login_name,SUSER_SID() AS login_sid,ORIGINAL_LOGIN() AS original_login,
 (SELECT p.principal_id,p.sid,p.name,p.type,p.owning_principal_id,p.is_fixed_role,p.create_date,p.modify_date,p.authentication_type FROM sys.database_principals p WHERE p.principal_id=USER_ID() OR p.name IN (@P1,@P2) ORDER BY p.principal_id FOR JSON PATH,INCLUDE_NULL_VALUES) AS principals,
 (SELECT m.role_principal_id,m.member_principal_id FROM sys.database_role_members m JOIN sys.database_principals r ON r.principal_id=m.role_principal_id JOIN sys.database_principals u ON u.principal_id=m.member_principal_id WHERE r.name=@P1 AND u.name=@P2 FOR JSON PATH) AS membership
 FOR JSON PATH,INCLUDE_NULL_VALUES)),2))
FROM sys.databases d WHERE d.database_id=DB_ID()
