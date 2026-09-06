PRAGMA foreign_keys = ON;
BEGIN;
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE,
    preferences TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(preferences))
) STRICT;
CREATE TABLE products (
    sku TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    price_cents INTEGER NOT NULL CHECK(price_cents >= 0)
) WITHOUT ROWID, STRICT;
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    placed_at TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','paid','shipped'))
) STRICT;
CREATE TABLE order_items (
    order_id INTEGER NOT NULL REFERENCES orders(id),
    sku TEXT NOT NULL REFERENCES products(sku),
    quantity INTEGER NOT NULL CHECK(quantity > 0),
    unit_price_cents INTEGER NOT NULL,
    total_cents INTEGER GENERATED ALWAYS AS (quantity * unit_price_cents) STORED,
    PRIMARY KEY(order_id,sku)
) WITHOUT ROWID, STRICT;
CREATE TABLE order_changes (id INTEGER PRIMARY KEY,order_id INTEGER,old_status TEXT,new_status TEXT);
CREATE TRIGGER record_order_status AFTER UPDATE OF status ON orders
WHEN OLD.status <> NEW.status BEGIN
    INSERT INTO order_changes(order_id,old_status,new_status) VALUES(NEW.id,OLD.status,NEW.status);
END;
CREATE INDEX pending_orders ON orders(placed_at DESC) WHERE status='pending';
CREATE VIEW order_summary AS
SELECT o.id,c.name AS customer,o.placed_at,o.status,sum(i.total_cents) AS total_cents
FROM orders o JOIN customers c ON c.id=o.customer_id JOIN order_items i ON i.order_id=o.id
GROUP BY o.id,c.name,o.placed_at,o.status;
INSERT INTO customers VALUES (1,'Amara','amara@example.invalid','{"currency":"NAD"}'),(2,'Bjørn',NULL,'{"currency":"EUR"}'),(3,'Chipo','chipo@example.invalid','{"currency":"NAD"}');
INSERT INTO products VALUES('KB-01','Mechanical keyboard',129900),('NB-01','Notebook',14900),('PN-01','Fountain pen',39900);
INSERT INTO orders VALUES(1,1,'2026-09-01T09:30:00Z','paid'),(2,2,'2026-09-02T10:15:00Z','pending'),(3,3,'2026-09-03T14:00:00Z','shipped');
INSERT INTO order_items(order_id,sku,quantity,unit_price_cents) VALUES(1,'KB-01',1,129900),(1,'NB-01',2,14900),(2,'PN-01',1,39900),(3,'NB-01',3,14900);
CREATE TABLE large(id INTEGER PRIMARY KEY,label TEXT NOT NULL,payload BLOB) STRICT;
WITH RECURSIVE series(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM series WHERE id<100000)
INSERT INTO large SELECT id,printf('SQLite row %06d',id),CASE WHEN id%100=0 THEN x'00ff1020' END FROM series;
COMMIT;
