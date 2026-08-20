#!/usr/bin/env sh
# Start (and if needed, create) the local demo Postgres instance and seed it
# with the `lab` schema every sift demo command expects.
#
# Idempotent: safe to run against an already-initialised, already-running
# cluster. Prints nothing on stdout except the port, so callers can capture it.
#
# Environment:
#   SIFT_DEMO_PGDATA           data dir            (default /tmp/sift-demo-pg)
#   SIFT_DEMO_PG_LOG           postmaster log      (default /tmp/sift-demo-pg.log)
#   SIFT_DEMO_PG_PORT          TCP port            (default 5433)
#   SIFT_DEMO_PG_SOCKET_DIR    unix socket dir     (default /tmp/sift-demo-pg-socket)
#   SIFT_DEMO_RESET            recreate sifttest   (default 0)
set -eu

pgdata="${SIFT_DEMO_PGDATA:-/tmp/sift-demo-pg}"
pglog="${SIFT_DEMO_PG_LOG:-/tmp/sift-demo-pg.log}"
pgport="${SIFT_DEMO_PG_PORT:-5433}"
pgsocket="${SIFT_DEMO_PG_SOCKET_DIR:-/tmp/sift-demo-pg-socket}"

mkdir -p "$pgsocket"

if [ ! -f "$pgdata/PG_VERSION" ]; then
  rm -rf "$pgdata"
  initdb -D "$pgdata" -U sift --auth=trust --no-locale --encoding=UTF8 >&2
  {
    echo "listen_addresses = '127.0.0.1'"
    echo "port = $pgport"
    echo "unix_socket_directories = '$pgsocket'"
    # Small demo cluster: do not reserve production-shaped memory on a laptop.
    echo "shared_buffers = '64MB'"
    echo "max_connections = 40"
  } >> "$pgdata/postgresql.conf"
fi
if ! grep -q "unix_socket_directories = '$pgsocket'" "$pgdata/postgresql.conf"; then
  echo "unix_socket_directories = '$pgsocket'" >> "$pgdata/postgresql.conf"
fi

if ! pg_ctl -D "$pgdata" status >/dev/null 2>&1; then
  pg_ctl -D "$pgdata" -l "$pglog" -w start >&2
fi

if [ "${SIFT_DEMO_RESET:-0}" = "1" ]; then
  dropdb -h 127.0.0.1 -p "$pgport" -U sift --if-exists --force sifttest >&2
fi
createdb -h 127.0.0.1 -p "$pgport" -U sift sifttest 2>/dev/null || true
psql -q -h 127.0.0.1 -p "$pgport" -U sift -d sifttest >&2 <<'SQL'
SET client_min_messages = warning;
CREATE SCHEMA IF NOT EXISTS lab;

CREATE TABLE IF NOT EXISTS lab.departments (
  id smallint PRIMARY KEY,
  name text NOT NULL UNIQUE,
  cost_center text NOT NULL UNIQUE,
  annual_budget numeric(14, 2) NOT NULL,
  active boolean NOT NULL DEFAULT true
);
INSERT INTO lab.departments (id, name, cost_center, annual_budget, active) VALUES
  (1, 'Engineering', 'ENG-100', 2400000.00, true),
  (2, 'Data', 'DAT-200', 1350000.00, true),
  (3, 'Operations', 'OPS-300', 980000.00, true),
  (4, 'Finance', 'FIN-400', 760000.00, true),
  (5, 'Sales', 'SAL-500', 1750000.00, true),
  (6, 'Research', 'RES-600', 1100000.00, false)
ON CONFLICT (id) DO UPDATE SET
  name = EXCLUDED.name,
  cost_center = EXCLUDED.cost_center,
  annual_budget = EXCLUDED.annual_budget,
  active = EXCLUDED.active;

CREATE TABLE IF NOT EXISTS lab.people (
  id integer PRIMARY KEY,
  name text NOT NULL,
  role text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE lab.people ADD COLUMN IF NOT EXISTS email text;
ALTER TABLE lab.people ADD COLUMN IF NOT EXISTS department_id smallint;
ALTER TABLE lab.people ADD COLUMN IF NOT EXISTS salary numeric(12, 2);
ALTER TABLE lab.people ADD COLUMN IF NOT EXISTS active boolean NOT NULL DEFAULT true;
ALTER TABLE lab.people ADD COLUMN IF NOT EXISTS profile jsonb NOT NULL DEFAULT '{}'::jsonb;

INSERT INTO lab.people (
  id, name, role, email, department_id, salary, active, profile, created_at
)
SELECT
  person_id,
  CASE person_id
    WHEN 1 THEN 'Ada Lovelace'
    WHEN 2 THEN 'Grace Hopper'
    WHEN 3 THEN 'Linus Torvalds'
    WHEN 4 THEN 'Margaret Hamilton'
    WHEN 5 THEN 'Edsger Dijkstra'
    WHEN 6 THEN 'Barbara Liskov'
    ELSE 'Demo Person ' || lpad(person_id::text, 3, '0')
  END,
  (ARRAY['engineer', 'analyst', 'operator', 'manager', 'designer', 'researcher'])[
    ((person_id - 1) % 6) + 1
  ],
  'person' || person_id || '@example.test',
  ((person_id - 1) % 6) + 1,
  48000.00 + ((person_id * 1379) % 92000),
  person_id % 17 <> 0,
  jsonb_build_object(
    'timezone', (ARRAY['UTC', 'Africa/Windhoek', 'Europe/London', 'America/New_York'])[
      ((person_id - 1) % 4) + 1
    ],
    'level', ((person_id - 1) % 7) + 1,
    'remote', person_id % 3 = 0
  ),
  timestamptz '2023-01-01 08:00:00+00' + person_id * interval '19 hours'
FROM generate_series(1, 200) AS ids(person_id)
ON CONFLICT (id) DO UPDATE SET
  name = EXCLUDED.name,
  role = EXCLUDED.role,
  email = EXCLUDED.email,
  department_id = EXCLUDED.department_id,
  salary = EXCLUDED.salary,
  active = EXCLUDED.active,
  profile = EXCLUDED.profile,
  created_at = EXCLUDED.created_at;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
    WHERE conrelid = 'lab.people'::regclass
      AND conname = 'people_department_fk'
  ) THEN
    ALTER TABLE lab.people
      ADD CONSTRAINT people_department_fk
      FOREIGN KEY (department_id) REFERENCES lab.departments(id);
  END IF;
END
$$;
CREATE UNIQUE INDEX IF NOT EXISTS people_email_idx ON lab.people (email);
CREATE INDEX IF NOT EXISTS people_department_idx ON lab.people (department_id, active);
CREATE INDEX IF NOT EXISTS people_profile_idx ON lab.people USING gin (profile);

CREATE TABLE IF NOT EXISTS lab.projects (
  id integer PRIMARY KEY,
  code text NOT NULL UNIQUE,
  name text NOT NULL,
  department_id smallint NOT NULL REFERENCES lab.departments(id),
  owner_id integer NOT NULL REFERENCES lab.people(id),
  status text NOT NULL CHECK (status IN ('planned', 'active', 'paused', 'complete')),
  budget numeric(14, 2) NOT NULL,
  starts_on date NOT NULL,
  ends_on date,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb
);
INSERT INTO lab.projects (
  id, code, name, department_id, owner_id, status, budget, starts_on, ends_on, metadata
)
SELECT
  project_id,
  'PRJ-' || lpad(project_id::text, 3, '0'),
  'Demo Project ' || project_id,
  ((project_id - 1) % 6) + 1,
  ((project_id * 7) % 200) + 1,
  (ARRAY['planned', 'active', 'active', 'paused', 'complete'])[
    ((project_id - 1) % 5) + 1
  ],
  25000.00 + project_id * 8750.00,
  date '2024-01-01' + project_id * 11,
  CASE WHEN project_id % 5 = 0 THEN date '2024-01-01' + project_id * 11 + 180 END,
  jsonb_build_object('priority', ((project_id - 1) % 4) + 1, 'billable', project_id % 2 = 0)
FROM generate_series(1, 36) AS ids(project_id)
ON CONFLICT (id) DO UPDATE SET
  code = EXCLUDED.code,
  name = EXCLUDED.name,
  department_id = EXCLUDED.department_id,
  owner_id = EXCLUDED.owner_id,
  status = EXCLUDED.status,
  budget = EXCLUDED.budget,
  starts_on = EXCLUDED.starts_on,
  ends_on = EXCLUDED.ends_on,
  metadata = EXCLUDED.metadata;
CREATE INDEX IF NOT EXISTS projects_status_idx ON lab.projects (status, starts_on DESC);

CREATE TABLE IF NOT EXISTS lab.project_assignments (
  project_id integer NOT NULL REFERENCES lab.projects(id),
  person_id integer NOT NULL REFERENCES lab.people(id),
  assignment_role text NOT NULL,
  allocation_percent numeric(5, 2) NOT NULL CHECK (allocation_percent > 0 AND allocation_percent <= 100),
  assigned_at timestamptz NOT NULL,
  PRIMARY KEY (project_id, person_id)
);
INSERT INTO lab.project_assignments (
  project_id, person_id, assignment_role, allocation_percent, assigned_at
)
SELECT
  project.id,
  ((project.id * 7 + slot * 13) % 200) + 1,
  (ARRAY['lead', 'developer', 'analyst', 'reviewer'])[((slot - 1) % 4) + 1],
  20.00 + ((project.id + slot * 5) % 9) * 7.50,
  timestamptz '2024-01-01 09:00:00+00' + (project.id * slot) * interval '7 hours'
FROM lab.projects AS project
CROSS JOIN generate_series(1, 12) AS slots(slot)
ON CONFLICT (project_id, person_id) DO UPDATE SET
  assignment_role = EXCLUDED.assignment_role,
  allocation_percent = EXCLUDED.allocation_percent,
  assigned_at = EXCLUDED.assigned_at;
CREATE INDEX IF NOT EXISTS project_assignments_person_idx
  ON lab.project_assignments (person_id, project_id);

CREATE TABLE IF NOT EXISTS lab.customers (
  id integer PRIMARY KEY,
  company_name text NOT NULL,
  contact_email text NOT NULL UNIQUE,
  region text NOT NULL,
  credit_limit numeric(12, 2) NOT NULL,
  tags text[] NOT NULL DEFAULT '{}',
  attributes jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL
);
INSERT INTO lab.customers (
  id, company_name, contact_email, region, credit_limit, tags, attributes, created_at
)
SELECT
  customer_id,
  'Customer Company ' || lpad(customer_id::text, 3, '0'),
  'customer' || customer_id || '@example.test',
  (ARRAY['Africa', 'Europe', 'North America', 'Asia Pacific'])[
    ((customer_id - 1) % 4) + 1
  ],
  5000.00 + (customer_id % 25) * 2500.00,
  ARRAY[
    CASE WHEN customer_id % 3 = 0 THEN 'priority' ELSE 'standard' END,
    CASE WHEN customer_id % 5 = 0 THEN 'partner' ELSE 'direct' END
  ],
  jsonb_build_object('employees', 10 + customer_id * 3, 'newsletter', customer_id % 2 = 0),
  timestamptz '2022-06-01 10:00:00+00' + customer_id * interval '31 hours'
FROM generate_series(1, 160) AS ids(customer_id)
ON CONFLICT (id) DO UPDATE SET
  company_name = EXCLUDED.company_name,
  contact_email = EXCLUDED.contact_email,
  region = EXCLUDED.region,
  credit_limit = EXCLUDED.credit_limit,
  tags = EXCLUDED.tags,
  attributes = EXCLUDED.attributes,
  created_at = EXCLUDED.created_at;

CREATE TABLE IF NOT EXISTS lab.orders (
  id bigint PRIMARY KEY,
  customer_id integer NOT NULL REFERENCES lab.customers(id),
  salesperson_id integer REFERENCES lab.people(id),
  status text NOT NULL CHECK (status IN ('draft', 'placed', 'paid', 'shipped', 'cancelled')),
  total numeric(14, 2) NOT NULL DEFAULT 0,
  currency char(3) NOT NULL,
  placed_at timestamptz NOT NULL,
  shipping_address jsonb NOT NULL
);
INSERT INTO lab.orders (
  id, customer_id, salesperson_id, status, total, currency, placed_at, shipping_address
)
SELECT
  order_id,
  ((order_id * 17) % 160) + 1,
  ((order_id * 11) % 200) + 1,
  (ARRAY['draft', 'placed', 'paid', 'shipped', 'cancelled'])[
    ((order_id - 1) % 5) + 1
  ],
  0,
  (ARRAY['USD', 'EUR', 'NAD', 'GBP'])[((order_id - 1) % 4) + 1],
  timestamptz '2024-01-01 12:00:00+00' + order_id * interval '5 hours',
  jsonb_build_object(
    'city', 'Demo City ' || ((order_id - 1) % 20 + 1),
    'country', (ARRAY['NA', 'ZA', 'DE', 'GB'])[((order_id - 1) % 4) + 1]
  )
FROM generate_series(1, 800) AS ids(order_id)
ON CONFLICT (id) DO UPDATE SET
  customer_id = EXCLUDED.customer_id,
  salesperson_id = EXCLUDED.salesperson_id,
  status = EXCLUDED.status,
  currency = EXCLUDED.currency,
  placed_at = EXCLUDED.placed_at,
  shipping_address = EXCLUDED.shipping_address;
CREATE INDEX IF NOT EXISTS orders_customer_idx ON lab.orders (customer_id, placed_at DESC);
CREATE INDEX IF NOT EXISTS orders_status_idx ON lab.orders (status, placed_at DESC);

CREATE TABLE IF NOT EXISTS lab.order_items (
  order_id bigint NOT NULL REFERENCES lab.orders(id) ON DELETE CASCADE,
  line_number smallint NOT NULL,
  product_sku text NOT NULL,
  description text NOT NULL,
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price numeric(12, 2) NOT NULL CHECK (unit_price >= 0),
  discount_percent numeric(5, 2) NOT NULL DEFAULT 0,
  PRIMARY KEY (order_id, line_number)
);
INSERT INTO lab.order_items (
  order_id, line_number, product_sku, description, quantity, unit_price, discount_percent
)
SELECT
  orders.id,
  line_number,
  'SKU-' || lpad((((orders.id + line_number * 29) % 240) + 1)::text, 4, '0'),
  'Demo product ' || (((orders.id + line_number * 29) % 240) + 1),
  ((orders.id + line_number) % 8) + 1,
  9.95 + ((orders.id * line_number * 7) % 400),
  CASE WHEN (orders.id + line_number) % 7 = 0 THEN 10.00 ELSE 0.00 END
FROM lab.orders
CROSS JOIN generate_series(1, 3) AS lines(line_number)
ON CONFLICT (order_id, line_number) DO UPDATE SET
  product_sku = EXCLUDED.product_sku,
  description = EXCLUDED.description,
  quantity = EXCLUDED.quantity,
  unit_price = EXCLUDED.unit_price,
  discount_percent = EXCLUDED.discount_percent;

UPDATE lab.orders AS orders
SET total = totals.total
FROM (
  SELECT
    order_id,
    round(sum(quantity * unit_price * (1 - discount_percent / 100.0)), 2) AS total
  FROM lab.order_items
  GROUP BY order_id
) AS totals
WHERE totals.order_id = orders.id;

CREATE TABLE IF NOT EXISTS lab.audit_events (
  id bigint PRIMARY KEY,
  actor_id integer REFERENCES lab.people(id),
  event_type text NOT NULL,
  object_type text NOT NULL,
  object_id text NOT NULL,
  payload jsonb NOT NULL,
  source_ip inet,
  occurred_at timestamptz NOT NULL
);
INSERT INTO lab.audit_events (
  id, actor_id, event_type, object_type, object_id, payload, source_ip, occurred_at
)
SELECT
  event_id,
  ((event_id * 19) % 200) + 1,
  (ARRAY['created', 'updated', 'viewed', 'exported', 'approved'])[
    ((event_id - 1) % 5) + 1
  ],
  (ARRAY['project', 'order', 'customer', 'report'])[((event_id - 1) % 4) + 1],
  ((event_id * 23) % 800 + 1)::text,
  jsonb_build_object('request_id', 'req-' || event_id, 'duration_ms', (event_id * 37) % 1500),
  ('10.20.' || ((event_id / 250) % 250) || '.' || ((event_id % 250) + 1))::inet,
  timestamptz '2024-01-01 00:00:00+00' + event_id * interval '47 minutes'
FROM generate_series(1, 1500) AS ids(event_id)
ON CONFLICT (id) DO UPDATE SET
  actor_id = EXCLUDED.actor_id,
  event_type = EXCLUDED.event_type,
  object_type = EXCLUDED.object_type,
  object_id = EXCLUDED.object_id,
  payload = EXCLUDED.payload,
  source_ip = EXCLUDED.source_ip,
  occurred_at = EXCLUDED.occurred_at;
CREATE INDEX IF NOT EXISTS audit_events_timeline_idx ON lab.audit_events (occurred_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_object_idx
  ON lab.audit_events (object_type, object_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_payload_idx ON lab.audit_events USING gin (payload);

CREATE TABLE IF NOT EXISTS lab.large (
  id bigint PRIMARY KEY,
  label text NOT NULL,
  category text NOT NULL,
  amount numeric(14, 2) NOT NULL,
  active boolean NOT NULL,
  payload jsonb NOT NULL,
  created_at timestamptz NOT NULL
);
INSERT INTO lab.large (id, label, category, amount, active, payload, created_at)
SELECT
  row_id,
  'Large demo row ' || lpad(row_id::text, 5, '0'),
  (ARRAY['alpha', 'beta', 'gamma', 'delta'])[((row_id - 1) % 4) + 1],
  round((row_id * 17.29)::numeric, 2),
  row_id % 7 <> 0,
  jsonb_build_object(
    'sequence', row_id,
    'bucket', row_id % 100,
    'fixture', 'lab.large'
  ),
  timestamptz '2024-01-01 00:00:00+00' + row_id * interval '1 minute'
FROM generate_series(1, 10000) AS ids(row_id)
ON CONFLICT (id) DO UPDATE SET
  label = EXCLUDED.label,
  category = EXCLUDED.category,
  amount = EXCLUDED.amount,
  active = EXCLUDED.active,
  payload = EXCLUDED.payload,
  created_at = EXCLUDED.created_at;
DELETE FROM lab.large WHERE id < 1 OR id > 10000;
CREATE INDEX IF NOT EXISTS large_category_idx ON lab.large (category, id);

CREATE OR REPLACE VIEW lab.people_directory AS
SELECT
  people.id,
  people.name,
  people.email,
  people.role,
  departments.name AS department,
  people.active,
  people.created_at
FROM lab.people
JOIN lab.departments ON departments.id = people.department_id;

CREATE OR REPLACE VIEW lab.project_staffing AS
SELECT
  projects.id AS project_id,
  projects.code,
  projects.name,
  projects.status,
  departments.name AS department,
  count(assignments.person_id) AS team_size,
  round(sum(assignments.allocation_percent), 2) AS total_allocation_percent
FROM lab.projects
JOIN lab.departments ON departments.id = projects.department_id
LEFT JOIN lab.project_assignments AS assignments ON assignments.project_id = projects.id
GROUP BY projects.id, projects.code, projects.name, projects.status, departments.name;

CREATE OR REPLACE VIEW lab.order_summary AS
SELECT
  orders.id,
  customers.company_name AS customer,
  people.name AS salesperson,
  orders.status,
  orders.total,
  orders.currency,
  count(items.line_number) AS line_count,
  orders.placed_at
FROM lab.orders
JOIN lab.customers ON customers.id = orders.customer_id
LEFT JOIN lab.people ON people.id = orders.salesperson_id
LEFT JOIN lab.order_items AS items ON items.order_id = orders.id
GROUP BY orders.id, customers.company_name, people.name;

CREATE MATERIALIZED VIEW IF NOT EXISTS lab.department_metrics AS
SELECT
  departments.id,
  departments.name,
  count(DISTINCT people.id) AS people_count,
  count(DISTINCT projects.id) AS project_count,
  coalesce(sum(DISTINCT projects.budget), 0) AS project_budget
FROM lab.departments
LEFT JOIN lab.people ON people.department_id = departments.id
LEFT JOIN lab.projects ON projects.department_id = departments.id
GROUP BY departments.id, departments.name
WITH DATA;
REFRESH MATERIALIZED VIEW lab.department_metrics;

CREATE OR REPLACE FUNCTION lab.people_by_role(requested_role text)
RETURNS TABLE (id integer, name text, email text, department text)
LANGUAGE sql
STABLE
AS $$
  SELECT directory.id, directory.name, directory.email, directory.department
  FROM lab.people_directory AS directory
  WHERE directory.role = requested_role
  ORDER BY directory.name
$$;

COMMENT ON SCHEMA lab IS 'Deterministic Sift desktop demo schema';
COMMENT ON TABLE lab.audit_events IS 'High-row-count table for grid paging and filtering tests';
COMMENT ON TABLE lab.large IS 'Exactly 10,000 deterministic rows for result-grid stress testing';
SQL

printf '%s\n' "$pgport"
