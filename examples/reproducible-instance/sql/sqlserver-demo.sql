-- Dedicated, repeatable demo database. Existing rows survive relaunches.
IF DB_ID(N'siftdemo') IS NULL CREATE DATABASE siftdemo;
GO
USE siftdemo;
GO
IF SCHEMA_ID(N'lab') IS NULL EXEC(N'CREATE SCHEMA lab');
GO
IF OBJECT_ID(N'lab.people', N'U') IS NULL
BEGIN
    CREATE TABLE lab.people(id int NOT NULL PRIMARY KEY, name nvarchar(120) NOT NULL, email nvarchar(200) UNIQUE);
    INSERT lab.people VALUES(1,N'Ada Lovelace',N'ada@example.test'),(2,N'Grace Hopper',N'grace@example.test'),(3,N'Alan Turing',N'alan@example.test');
END;
IF OBJECT_ID(N'lab.products', N'U') IS NULL
BEGIN
    CREATE TABLE lab.products(id int NOT NULL PRIMARY KEY, name nvarchar(120) NOT NULL, price decimal(12,2) NOT NULL);
    INSERT lab.products VALUES(1,N'Keyboard',129.90),(2,N'Notebook',14.90),(3,N'Pen',39.90);
END;
IF OBJECT_ID(N'lab.orders', N'U') IS NULL
BEGIN
    CREATE TABLE lab.orders(id int NOT NULL PRIMARY KEY, person_id int NOT NULL REFERENCES lab.people(id), placed_at datetime2 NOT NULL DEFAULT SYSUTCDATETIME(), status nvarchar(20) NOT NULL);
    INSERT lab.orders(id,person_id,status) VALUES(1,1,N'paid'),(2,2,N'pending'),(3,3,N'shipped');
END;
IF OBJECT_ID(N'lab.order_items', N'U') IS NULL
BEGIN
    CREATE TABLE lab.order_items(order_id int NOT NULL REFERENCES lab.orders(id), product_id int NOT NULL REFERENCES lab.products(id), quantity int NOT NULL CHECK(quantity>0), PRIMARY KEY(order_id,product_id));
    INSERT lab.order_items VALUES(1,1,1),(1,2,2),(2,3,1),(3,2,3);
END;
GO
CREATE OR ALTER VIEW lab.order_summary AS
SELECT o.id,p.name AS customer,o.placed_at,o.status,SUM(i.quantity * product.price) AS total
FROM lab.orders o JOIN lab.people p ON p.id=o.person_id
JOIN lab.order_items i ON i.order_id=o.id JOIN lab.products product ON product.id=i.product_id
GROUP BY o.id,p.name,o.placed_at,o.status;
GO
