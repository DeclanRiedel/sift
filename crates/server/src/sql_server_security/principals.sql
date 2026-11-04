SELECT TOP (101) CONVERT(nvarchar(128), p.name), CONVERT(nvarchar(60), p.type_desc),
 COALESCE(CONVERT(nvarchar(60), p.authentication_type_desc), N''),
 CONVERT(nvarchar(128), l.name), CONVERT(nvarchar(128), p.default_schema_name)
FROM sys.database_principals p
LEFT JOIN sys.server_principals l ON l.sid=p.sid AND l.type=p.type
 AND ((p.type='S' AND p.authentication_type=1) OR p.type IN ('U','G'))
WHERE p.type IN ('S','U','G','R') ORDER BY p.name
