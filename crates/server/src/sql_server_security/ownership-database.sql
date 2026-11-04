SELECT CONVERT(bit,CASE WHEN d.owner_sid=p.sid THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN IS_SRVROLEMEMBER('sysadmin')=1 OR HAS_PERMS_BY_NAME(QUOTENAME(DB_NAME()),'DATABASE','TAKE OWNERSHIP')=1 THEN 1 ELSE 0 END),
 CONVERT(bit,CASE WHEN IS_SRVROLEMEMBER('sysadmin')=1 OR p.principal_id=SUSER_ID() OR HAS_PERMS_BY_NAME(QUOTENAME(@P2),'LOGIN','IMPERSONATE')=1 THEN 1 ELSE 0 END),
 CONVERT(bit,1),
 CONVERT(nvarchar(64),CONVERT(varchar(64),HASHBYTES('SHA2_256',
 (SELECT CONVERT(nvarchar(128),@@SERVERNAME) AS server_name, CURRENT_USER AS actor, SUSER_SNAME() AS login_name, ORIGINAL_LOGIN() AS original_login,
 d.database_id,d.name,d.create_date,d.owner_sid,d.is_trustworthy_on,d.is_db_chaining_on,d.state,d.is_read_only,
 p.principal_id,p.sid,p.name AS target,p.type,p.create_date AS target_created,p.modify_date AS target_modified,p.is_disabled
 FOR JSON PATH, INCLUDE_NULL_VALUES)),2)),
 CONVERT(bigint,0),CONVERT(bigint,0)
FROM sys.databases d CROSS JOIN sys.server_principals p
WHERE d.database_id=DB_ID() AND d.name=@P1 AND d.database_id>4 AND p.name=@P2 AND p.type IN ('S','U')
 AND NOT EXISTS(SELECT 1 FROM sys.database_principals u WHERE u.sid=p.sid AND u.principal_id<>1)
