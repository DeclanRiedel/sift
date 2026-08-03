ALTER TABLE saved_query
ADD COLUMN revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0);
